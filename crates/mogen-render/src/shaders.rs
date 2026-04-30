pub(super) const VS_SRC: &str = r#"#version 330 core
layout (location = 0) in vec3 a_pos;
layout (location = 1) in vec3 a_normal;
layout (location = 2) in vec2 a_uv;
layout (location = 3) in vec4 a_joints;
layout (location = 4) in vec4 a_weights;

uniform mat4 u_viewproj;
// Per-batch matrix palette. Rigid batches use single-bone weights
// (`a_weights = [1,0,0,0]`, `a_joints[0]` = palette slot for the source node)
// so the same skinning path covers static and skinned geometry uniformly.
// Keep in sync with MAX_JOINTS in viewer.rs.
uniform mat4 u_joint_mats[128];
// Preview shader selector. 0=Standard, 1=Toon, 2=PS1, 3=CRT, 4=Matcap.
// Wireframe runs the Standard path with polygon-mode set to LINE on the CPU
// side, so it doesn't need its own branch here.
uniform int u_shader_mode;

out vec3 v_world_pos;
out vec3 v_normal;
out vec2 v_uv;
// Affine-interpolated copy of the UV for the PS1 preview mode. `noperspective`
// disables the perspective-correct divide, which reproduces the wobbling /
// sliding texture seams characteristic of PS1-era hardware. The standard
// fragment shader still samples the perspective-correct `v_uv`.
noperspective out vec2 v_uv_aff;

void main() {
    // Clamp defensively: huge skins get their palette truncated on the CPU
    // side, but a vertex might still reference a joint beyond 127 if the
    // caller's skin has more than MAX_JOINTS. Out-of-range uniform reads are
    // undefined, so keep the index in range unconditionally.
    ivec4 ji = clamp(ivec4(a_joints), ivec4(0), ivec4(127));
    mat4 palette = u_joint_mats[ji.x] * a_weights.x
                 + u_joint_mats[ji.y] * a_weights.y
                 + u_joint_mats[ji.z] * a_weights.z
                 + u_joint_mats[ji.w] * a_weights.w;
    vec4 pos4 = palette * vec4(a_pos, 1.0);
    // mat3(palette) is a reasonable approximation of the normal transform so
    // long as the palette stays close to rigid; the FS re-normalizes anyway.
    vec3 n = mat3(palette) * a_normal;
    vec4 clip = u_viewproj * pos4;
    if (u_shader_mode == 2) {
        // PS1 vertex snap: quantize NDC.xy to a coarse grid so the geometry
        // jitters in screen space as the camera moves, matching how the
        // console's fixed-point vertex pipeline rounded positions before
        // rasterization.
        float grid = 160.0;
        vec2 ndc = clip.xy / clip.w;
        ndc = floor(ndc * grid) / grid;
        clip.xy = ndc * clip.w;
    }
    gl_Position = clip;
    v_world_pos = pos4.xyz;
    v_normal = n;
    v_uv = a_uv;
    v_uv_aff = a_uv;
}
"#;

pub(super) const FS_SRC: &str = r#"#version 330 core
in vec3 v_world_pos;
in vec3 v_normal;
in vec2 v_uv;
noperspective in vec2 v_uv_aff;
out vec4 frag;

uniform vec3 u_camera_pos;
// Mirrors the VS uniform. See `PreviewShader::shader_mode` for the mapping.
uniform int u_shader_mode;

// Per-batch material scalars.
uniform vec3 u_base_color;
uniform float u_base_color_alpha;
uniform float u_metallic;
uniform float u_roughness;
uniform vec3 u_emissive;
uniform float u_emissive_strength;
// KHR_materials_transmission factor in [0,1]. We don't have a real
// refraction pass, but we use it to suppress the diffuse term: a glass
// surface mostly lets light pass through and only reflects specularly,
// so for `transmission` close to 1 the diffuse + diffuse-IBL contributions
// are scaled toward zero, leaving the Fresnel highlights intact. Combined
// with `Blend` alpha this is a cheap stand-in that reads as "translucent
// glass" instead of the milky-opaque look you get from alpha alone.
uniform float u_transmission;
// glTF alpha pipeline. 0 = Opaque, 1 = Mask, 2 = Blend.
uniform int u_alpha_mode;
uniform float u_alpha_cutoff;

// Texture toggles + samplers (one set per material slot). Albedo/emissive are
// uploaded as sRGB; the others are linear.
uniform int u_use_base_tex;
uniform int u_use_mr_tex;
uniform int u_use_normal_tex;
uniform int u_use_ao_tex;
uniform int u_use_emissive_tex;
uniform sampler2D u_base_tex;
uniform sampler2D u_mr_tex;
uniform sampler2D u_normal_tex;
uniform sampler2D u_ao_tex;
uniform sampler2D u_emissive_tex;

// Two analytic key/fill lights for direct illumination plus a procedural sky
// dome that doubles as the IBL probe for ambient + specular reflection.
uniform vec3 u_key_dir;
uniform vec3 u_fill_dir;
uniform vec3 u_sky_top;
uniform vec3 u_sky_horizon;
uniform vec3 u_sky_ground;
uniform vec3 u_sun_dir;
uniform vec3 u_sun_color;

const float PI = 3.14159265359;

// AgX tonemap (Troy Sobotka). Polynomial fit by Benjamin "MrLixm" /
// Filament refinements. Maps scene-referred linear sRGB to display-referred
// linear sRGB; matches Blender's default view transform much better than
// Reinhard, especially for bright sky / sun reflections that would otherwise
// pin to white. Final sRGB encode is left to GL_FRAMEBUFFER_SRGB.
const mat3 AgXInsetMatrix = mat3(
    0.842479062253094,  0.0784335999999992, 0.0792237451477643,
    0.0423282422610123, 0.878468636469772,  0.0791661274605434,
    0.0423756549057051, 0.0784336,          0.879142973793104);

const mat3 AgXOutsetMatrix = mat3(
     1.19687900512017,    -0.0980208811401368, -0.0990297440797205,
    -0.0528968517574562,   1.15190312990417,   -0.0989611768448433,
    -0.0529716355144438,  -0.0980434501171241,  1.15107367264116);

vec3 agxDefaultContrastApprox(vec3 x) {
    vec3 x2 = x * x;
    vec3 x4 = x2 * x2;
    return  15.5     * x4 * x2
          - 40.14    * x4 * x
          + 31.96    * x4
          -  6.868   * x2 * x
          +  0.4298  * x2
          +  0.1191  * x
          -  0.00232;
}

vec3 agx_tonemap(vec3 color) {
    color = AgXInsetMatrix * color;
    color = max(color, vec3(1e-10));
    color = log2(color);
    // Remap [-12.47, 4.03] EV → [0, 1].
    color = (color + 12.47393) / (12.47393 + 4.026069);
    color = clamp(color, 0.0, 1.0);
    color = agxDefaultContrastApprox(color);
    color = AgXOutsetMatrix * color;
    // Sigmoid output is in display-encoded sRGB. The framebuffer is sRGB,
    // so undo the display encoding here and let the hardware re-encode on
    // write — otherwise the gamma curve gets applied twice.
    color = pow(max(color, 0.0), vec3(2.2));
    return color;
}

// Three-band sky: zenith → horizon for the upper hemisphere, horizon → ground
// for the lower one, plus a tight sun disc. This is what reflections sample
// in the absence of a real cubemap.
vec3 sample_sky(vec3 dir) {
    float y = clamp(dir.y, -1.0, 1.0);
    vec3 sky;
    if (y >= 0.0) {
        sky = mix(u_sky_horizon, u_sky_top, smoothstep(0.0, 1.0, y));
    } else {
        sky = mix(u_sky_horizon, u_sky_ground, smoothstep(0.0, 1.0, -y));
    }
    float sun = max(dot(dir, -u_sun_dir), 0.0);
    sky += u_sun_color * pow(sun, 256.0) * 8.0;
    return sky;
}

// Schüler's "normal mapping without precomputed tangents": derive a TBN from
// screen-space derivatives of position+UV. Quality dips on coarse meshes but
// it lets us drop tangents from the vertex stream entirely.
mat3 cotangent_frame(vec3 N, vec3 p, vec2 uv) {
    vec3 dp1 = dFdx(p);
    vec3 dp2 = dFdy(p);
    vec2 duv1 = dFdx(uv);
    vec2 duv2 = dFdy(uv);
    vec3 dp2perp = cross(dp2, N);
    vec3 dp1perp = cross(N, dp1);
    vec3 T = dp2perp * duv1.x + dp1perp * duv2.x;
    vec3 B = dp2perp * duv1.y + dp1perp * duv2.y;
    float invmax = inversesqrt(max(dot(T, T), dot(B, B)));
    return mat3(T * invmax, B * invmax, N);
}

// GGX/Trowbridge-Reitz NDF.
float D_GGX(float NdH, float a) {
    float a2 = a * a;
    float d = (NdH * NdH * (a2 - 1.0) + 1.0);
    return a2 / max(PI * d * d, 1e-7);
}

vec3 F_Schlick(float cosT, vec3 F0) {
    return F0 + (1.0 - F0) * pow(clamp(1.0 - cosT, 0.0, 1.0), 5.0);
}

// Sébastien Lagarde's roughness-aware Fresnel for IBL: keeps the Fresnel
// edge from blowing out when the surface is rough.
vec3 F_Schlick_rough(float cosT, vec3 F0, float roughness) {
    return F0 + (max(vec3(1.0 - roughness), F0) - F0)
              * pow(clamp(1.0 - cosT, 0.0, 1.0), 5.0);
}

// Smith joint visibility (numerator absorbs the Cook-Torrance 1/(4 NdL NdV)).
float V_SmithGGX(float NdV, float NdL, float a) {
    float a2 = a * a;
    float ggxV = NdL * sqrt(NdV * NdV * (1.0 - a2) + a2);
    float ggxL = NdV * sqrt(NdL * NdL * (1.0 - a2) + a2);
    return 0.5 / max(ggxV + ggxL, 1e-4);
}

// Karis' fitted env BRDF (UE4 split-sum approximation, scale + bias only).
vec2 env_brdf_approx(float NdV, float roughness) {
    vec4 c0 = vec4(-1.0, -0.0275, -0.572, 0.022);
    vec4 c1 = vec4(1.0,  0.0425,  1.040, -0.040);
    vec4 r = roughness * c0 + c1;
    float a004 = min(r.x * r.x, exp2(-9.28 * NdV)) * r.x + r.y;
    return vec2(-1.04, 1.04) * a004 + r.zw;
}

// Splits the result into diffuse/specular so the caller can composite them
// separately. Transmissive materials need this because reflected light passes
// through the alpha blend at full strength while diffuse/absorbed light fades
// with alpha.
void brdf_direct(vec3 N, vec3 V, vec3 L, vec3 albedo, float metallic, float roughness, vec3 F0, float diffuse_scale,
                 out vec3 diff_out, out vec3 spec_out) {
    vec3 H = normalize(V + L);
    float NdL = max(dot(N, L), 0.0);
    float NdV = max(dot(N, V), 0.0);
    float NdH = max(dot(N, H), 0.0);
    float HdV = max(dot(H, V), 0.0);
    float a = roughness * roughness;
    float D = D_GGX(NdH, a);
    vec3 F = F_Schlick(HdV, F0);
    float Vis = V_SmithGGX(NdV, NdL, a);
    vec3 spec = D * F * Vis;
    vec3 kd = (1.0 - F) * (1.0 - metallic);
    // `diffuse_scale` is `(1 - transmission)`; for fully transmissive glass
    // it kills the diffuse term so only the Fresnel-reflected specular
    // survives.
    vec3 diff = kd * albedo * diffuse_scale / PI;
    diff_out = diff * NdL;
    spec_out = spec * NdL;
}

void main() {
    // PS1 mode swaps to the affine-interpolated UV so textured surfaces get
    // the authentic "sliding UV" look. All other modes use the perspective-
    // correct varying.
    vec2 uv = (u_shader_mode == 2) ? v_uv_aff : v_uv;
    // Gather material samples.
    vec4 base_sample = vec4(u_base_color, u_base_color_alpha);
    if (u_use_base_tex == 1) {
        base_sample *= texture(u_base_tex, uv);
    }
    // Alpha pipeline. `Mask` discards before any lighting work — cheap exit.
    // `Opaque` ignores the alpha channel; `Blend` propagates the authored
    // alpha; transmissive materials (no real refraction pass here) reduce
    // the head-on alpha by transmission so the background shows through,
    // and the Fresnel ramp below restores it at grazing angles.
    if (u_alpha_mode == 1 && base_sample.a < u_alpha_cutoff) {
        discard;
    }
    bool will_blend = (u_alpha_mode == 2) || (u_transmission > 0.0);
    float out_alpha = will_blend ? base_sample.a : 1.0;
    if (u_transmission > 0.0) {
        out_alpha *= (1.0 - u_transmission);
    }
    vec3 albedo = base_sample.rgb;

    float metallic = u_metallic;
    float roughness = u_roughness;
    if (u_use_mr_tex == 1) {
        // glTF convention: G = roughness, B = metallic. Multiply with the
        // authored scalars so a tint of metallic=0 still kills metalness.
        vec3 mr = texture(u_mr_tex, uv).rgb;
        roughness *= mr.g;
        metallic *= mr.b;
    }
    roughness = clamp(roughness, 0.045, 1.0);

    vec3 Ngeom = normalize(v_normal);
    // Double-sided materials see the back face of thin sheets; flip the
    // geometric normal toward the camera so the BRDF stays positive on the
    // back (otherwise leaves and cloth go black through their underside).
    if (!gl_FrontFacing) {
        Ngeom = -Ngeom;
    }
    vec3 V = normalize(u_camera_pos - v_world_pos);
    vec3 N = Ngeom;
    if (u_use_normal_tex == 1) {
        vec3 mapped = texture(u_normal_tex, uv).xyz * 2.0 - 1.0;
        // Boost the XY deviation so subtle maps (e.g. those derived from
        // albedo luminance in `pbr_maps.rs`) read as real bumps under
        // realtime lighting. Re-normalising afterwards keeps the vector
        // unit-length; a strength of 2 roughly doubles the slope without
        // flipping the facing sign.
        mapped.xy *= 2.0;
        // Tangent space uses dFdx/dFdy, which only run on triangles with
        // varying UVs. Mapped vector is renormalised on output.
        N = normalize(cotangent_frame(Ngeom, v_world_pos, uv) * mapped);
    }

    float ao = (u_use_ao_tex == 1) ? texture(u_ao_tex, uv).r : 1.0;
    vec3 emissive = u_emissive * u_emissive_strength;
    if (u_use_emissive_tex == 1) {
        emissive *= texture(u_emissive_tex, uv).rgb;
    }

    vec3 F0 = mix(vec3(0.04), albedo, metallic);
    float diffuse_scale = 1.0 - u_transmission;

    // Direct lighting: a warm key + cool fill. We keep diffuse and specular
    // separate so transmissive materials can composite them differently
    // (see the premultiplied-alpha output at the end).
    vec3 key_color  = vec3(1.00, 0.96, 0.90) * 1.10;
    vec3 fill_color = vec3(0.70, 0.78, 0.95) * 0.40;
    vec3 diff_key, spec_key, diff_fill, spec_fill;
    brdf_direct(N, V, normalize(-u_key_dir),  albedo, metallic, roughness, F0, diffuse_scale, diff_key,  spec_key);
    brdf_direct(N, V, normalize(-u_fill_dir), albedo, metallic, roughness, F0, diffuse_scale, diff_fill, spec_fill);
    vec3 diff_direct = diff_key * key_color + diff_fill * fill_color;
    vec3 spec_direct = spec_key * key_color + spec_fill * fill_color;

    // Image-based lighting from the analytic sky. The diffuse irradiance is a
    // crude average of the sky in the normal direction and straight up so the
    // result still has some directional structure but doesn't track the
    // reflection dir. The specular probe samples the actual reflection ray;
    // we fade it toward the diffuse sample as roughness rises to fake the
    // pre-filtered mip chain a real IBL would provide.
    float NdV = max(dot(N, V), 0.0);
    vec3 R = reflect(-V, N);
    vec3 spec_env = sample_sky(R);
    vec3 diff_env = 0.5 * sample_sky(N) + 0.5 * sample_sky(vec3(0.0, 1.0, 0.0));
    vec3 prefiltered = mix(spec_env, diff_env, roughness * roughness);
    vec3 F_ibl = F_Schlick_rough(NdV, F0, roughness);
    vec2 envBRDF = env_brdf_approx(NdV, roughness);
    vec3 specular_ibl = prefiltered * (F_ibl * envBRDF.x + envBRDF.y);
    vec3 kd_ibl = (1.0 - F_ibl) * (1.0 - metallic) * diffuse_scale;
    vec3 diffuse_ibl = kd_ibl * diff_env * albedo;

    // Split the lit radiance into two compositing channels:
    //   `absorbed` — diffuse + emissive, fades with alpha in the blend.
    //   `reflected` — direct + IBL specular, stays at full strength so a
    //                 transmissive surface still shows environment reflections
    //                 (KHR_materials_transmission preview without SSR).
    // AO modulates both direct and indirect diffuse so cavities read as shaded
    // even under a dominant key light (pure IBL-only AO was invisible on
    // well-lit surfaces). Specular is damped slightly to avoid hotspots
    // glowing out of crevices.
    float ao_spec = mix(1.0, ao, 0.5);
    vec3 absorbed = (diff_direct + diffuse_ibl) * ao + emissive;
    vec3 reflected = spec_direct * ao_spec + specular_ibl * ao_spec;

    // AgX is the closest match for Blender's default view transform without
    // shipping a 3D LUT. Tonemap the sum then split back by per-channel
    // ratio so the two halves share one color grade — tonemapping each
    // independently would drift hue balance when either side saturates.
    vec3 combined = absorbed + reflected;
    vec3 tonemapped = agx_tonemap(combined);
    vec3 reflect_frac = clamp(reflected / max(combined, vec3(1e-4)), vec3(0.0), vec3(1.0));
    vec3 reflected_tm = tonemapped * reflect_frac;
    vec3 absorbed_tm = tonemapped - reflected_tm;

    // Style overrides. Stylistic modes don't model reflection-vs-transmission,
    // so they collapse everything into `absorbed_tm` and the transparent
    // result fades uniformly. Keep these branches in sync with the matching
    // `shader_mode` values in `preview_shader.rs`.
    if (u_shader_mode == 1) {
        // Toon: 4-band quantized lambert using the key-light direction, tinted
        // with the authored albedo plus a dark Fresnel-based outline.
        float NdL = max(dot(N, normalize(-u_key_dir)), 0.0);
        float band;
        if      (NdL > 0.80) band = 1.00;
        else if (NdL > 0.50) band = 0.72;
        else if (NdL > 0.25) band = 0.42;
        else                 band = 0.20;
        vec3 toon = albedo * band + emissive;
        float outline = pow(1.0 - max(dot(N, V), 0.0), 4.0);
        absorbed_tm = mix(toon, vec3(0.03), smoothstep(0.75, 0.95, outline));
        reflected_tm = vec3(0.0);
    } else if (u_shader_mode == 2) {
        // PS1 retro: ordered 4x4 Bayer dither combined with a 5-bit-per-
        // channel colour quantize. Vertex snap + affine UVs live in the VS
        // and the `uv` selection above.
        float bayer[16] = float[16](
             0.0/16.0,  8.0/16.0,  2.0/16.0, 10.0/16.0,
            12.0/16.0,  4.0/16.0, 14.0/16.0,  6.0/16.0,
             3.0/16.0, 11.0/16.0,  1.0/16.0,  9.0/16.0,
            15.0/16.0,  7.0/16.0, 13.0/16.0,  5.0/16.0
        );
        int bx = int(mod(gl_FragCoord.x, 4.0));
        int by = int(mod(gl_FragCoord.y, 4.0));
        float thr = bayer[by * 4 + bx] - 0.5;
        float levels = 32.0;
        absorbed_tm = clamp(floor(tonemapped * levels + thr + 0.5) / levels, 0.0, 1.0);
        reflected_tm = vec3(0.0);
    } else if (u_shader_mode == 3) {
        // CRT: horizontal scanlines + an RGB aperture-grille mask. No post-
        // process FBO, so this rides on gl_FragCoord directly — the mask
        // pattern is in screen pixels, which matches how real trinitrons
        // looked under any camera zoom.
        float scan = 0.80 + 0.20 * cos(gl_FragCoord.y * 3.14159);
        int col = int(mod(gl_FragCoord.x, 3.0));
        vec3 mask = vec3(0.85);
        if (col == 0) mask.r = 1.15;
        else if (col == 1) mask.g = 1.15;
        else mask.b = 1.15;
        absorbed_tm = clamp(tonemapped * scan * mask, 0.0, 1.0);
        reflected_tm = vec3(0.0);
    } else if (u_shader_mode == 4) {
        // Matcap: a clay-lit hemisphere preview that ignores the PBR
        // material entirely. Great for checking silhouette and surface
        // curvature while sculpting.
        float hemi = N.y * 0.5 + 0.5;
        vec3 low  = vec3(0.18, 0.17, 0.16);
        vec3 high = vec3(0.93, 0.92, 0.90);
        vec3 clay = mix(low, high, smoothstep(0.0, 1.0, hemi));
        float ndotv = max(dot(N, V), 0.0);
        float rim = pow(1.0 - ndotv, 3.0);
        absorbed_tm = clay + rim * vec3(0.45);
        reflected_tm = vec3(0.0);
    }

    // Final alpha. For non-transmissive Blend materials this is just
    // `base.a`. For transmissive materials we ramp the alpha back up at
    // grazing angles so the Fresnel rim of a glass sphere stays visible.
    // pow5 of (1 - NdV) is the same shape Schlick uses for Fresnel.
    if (u_transmission > 0.0) {
        float fresnel_rim = pow(1.0 - NdV, 5.0);
        out_alpha = mix(out_alpha, base_sample.a, fresnel_rim);
    }

    // Premultiplied output. The fragment contributes
    //   src.rgb = absorbed * alpha + reflected
    //   src.a   = alpha
    // and the compositor (glBlendFunc(ONE, ONE_MINUS_SRC_ALPHA), set in
    // renderer.rs) produces
    //   final = absorbed * alpha + reflected + bg * (1 - alpha)
    // so reflected light from the environment lands on the framebuffer at
    // full strength even when the surface is mostly see-through.
    frag = vec4(absorbed_tm * out_alpha + reflected_tm, out_alpha);
}
"#;

pub(super) const GIZMO_VS: &str = r#"#version 330 core
layout (location = 0) in vec3 a_pos;
layout (location = 1) in vec3 a_color;
uniform mat4 u_viewproj;
uniform vec3 u_origin;
uniform float u_scale;
out vec3 v_color;
void main() {
    vec3 world = u_origin + a_pos * u_scale;
    gl_Position = u_viewproj * vec4(world, 1.0);
    v_color = a_color;
}
"#;

pub(super) const GIZMO_FS: &str = r#"#version 330 core
in vec3 v_color;
out vec4 frag;
void main() {
    frag = vec4(v_color, 1.0);
}
"#;

pub(super) const GRID_VS: &str = r#"#version 330 core
layout (location = 0) in vec2 a_pos;
uniform mat4 u_inv_viewproj;
out vec3 v_near;
out vec3 v_far;
void main() {
    // Unproject this fragment's NDC position at the near and far planes to
    // get the world-space ray endpoints. The FS reconstructs the ray and
    // intersects it with Y = 0.
    vec4 pn = u_inv_viewproj * vec4(a_pos, -1.0, 1.0);
    vec4 pf = u_inv_viewproj * vec4(a_pos,  1.0, 1.0);
    v_near = pn.xyz / pn.w;
    v_far  = pf.xyz / pf.w;
    // Place the quad on the far plane; the FS overrides depth via
    // gl_FragDepth so the depth test lines up with the world-space hit.
    gl_Position = vec4(a_pos, 1.0, 1.0);
}
"#;

pub(super) const GRID_FS: &str = r#"#version 330 core
in vec3 v_near;
in vec3 v_far;
out vec4 frag;
uniform mat4 u_viewproj;
uniform vec3 u_camera_pos;

// One band of grid lines at the given world-space spacing. Returns 1 on a
// line and 0 between, anti-aliased via screen-space derivatives so lines
// stay a constant pixel width regardless of camera distance.
float grid_layer(vec2 coord, float spacing) {
    vec2 g = coord / spacing;
    vec2 d = fwidth(g);
    vec2 a = abs(fract(g - 0.5) - 0.5) / max(d, vec2(1e-6));
    float line = min(a.x, a.y);
    return 1.0 - clamp(line, 0.0, 1.0);
}

// Highlight a single world axis. Uses fwidth-based AA for the same constant
// pixel width, so the axis stays sharp at every zoom level.
float axis_line(float coord) {
    float d = fwidth(coord);
    return 1.0 - clamp(abs(coord) / max(d, 1e-6), 0.0, 1.0);
}

void main() {
    vec3 dir = v_far - v_near;
    // Ray-plane intersect with Y = 0. t < 0 means the plane is behind the
    // ray's origin (camera looking up away from the floor) — discard so the
    // grid never wraps back through the camera.
    float t = -v_near.y / dir.y;
    if (t < 0.0) discard;
    vec3 P = v_near + t * dir;

    // Write proper per-fragment depth so the scene occludes the grid the
    // same way it would a real ground plane.
    vec4 clip = u_viewproj * vec4(P, 1.0);
    gl_FragDepth = clamp(clip.z / clip.w * 0.5 + 0.5, 0.0, 1.0);

    // Two banded scales: 1-unit minor + 10-unit major lines. As the camera
    // pulls back, the minor band's fwidth widens past the line spacing and
    // smoothly fades to a flat tint; the major band stays visible long
    // after, which gives the grid a natural logarithmic LOD without an
    // explicit scheme.
    float minor = grid_layer(P.xz, 1.0);
    float major = grid_layer(P.xz, 10.0);

    // Distance fade — drops to zero by ~200 units from the camera so the
    // far horizon dissolves into the background instead of forming a hard
    // edge. Squared so the falloff is gentle near and steep far.
    float dist = length(P.xz - u_camera_pos.xz);
    float dist_fade = 1.0 - smoothstep(0.0, 1.0, dist / 200.0);
    dist_fade *= dist_fade;
    // Glancing-angle fade — without this the horizon turns into a solid
    // grey band when the camera looks nearly horizontally, because every
    // pixel covers a huge swath of grid cells.
    vec3 view = normalize(u_camera_pos - P);
    float angle_fade = clamp(abs(view.y) * 4.0, 0.0, 1.0);
    float fade = dist_fade * angle_fade;

    // X axis runs along Z = 0; Z axis runs along X = 0. Godot/glTF axis
    // colours: X red, Z blue. Y is up and never appears as a grid line.
    float x_axis = axis_line(P.z);
    float z_axis = axis_line(P.x);

    vec3 base_color = vec3(0.38, 0.39, 0.40);
    float minor_a = minor * 0.07;
    float major_a = major * 0.20;
    float alpha = max(minor_a, major_a);
    vec3 color = base_color;
    // Promote axis lines above the regular grid when they're brighter than
    // the underlying band — the faint colour tint reads as "this is the
    // world origin" without competing with the rest of the grid.
    if (x_axis * 0.45 > alpha) {
        color = vec3(0.55, 0.42, 0.42);
        alpha = x_axis * 0.45;
    }
    if (z_axis * 0.45 > alpha) {
        color = vec3(0.42, 0.48, 0.58);
        alpha = z_axis * 0.45;
    }

    alpha *= fade;
    if (alpha <= 0.0) discard;
    frag = vec4(color, alpha);
}
"#;
