pub(crate) const VS_SRC: &str = r#"#version 330 core
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
// Preview shader selector. 0=Standard, 1=Toon, 2=CRT, 3=Matcap.
// Wireframe runs the Standard path with polygon-mode set to LINE on the CPU
// side, so it doesn't need its own branch here.
uniform int u_shader_mode;

out vec3 v_world_pos;
out vec3 v_normal;
out vec2 v_uv;

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
    gl_Position = u_viewproj * pos4;
    v_world_pos = pos4.xyz;
    v_normal = n;
    v_uv = a_uv;
}
"#;

pub(crate) const FS_SRC: &str = r#"#version 330 core
in vec3 v_world_pos;
in vec3 v_normal;
in vec2 v_uv;
out vec4 frag;

uniform vec3 u_camera_pos;
// Mirrors the VS uniform. See `PreviewShader::shader_mode` for the mapping.
uniform int u_shader_mode;
// Per-material shader override. 0=Standard (PBR), 1=Water. Distinct from
// `u_shader_mode` (a global preview-style selector): per-material shaders
// only apply to surfaces whose authored `Material::shader` opted in, which
// is why the FS reads them separately.
uniform int u_material_shader;
// Seconds since renderer start, monotonic. Drives time-varying material
// shaders (e.g. water waves). Only the per-material shader branches sample
// it; the standard PBR path is time-invariant.
uniform float u_time;

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
// Per-material multiplier on the tangent-space normal's XY components, applied
// after the texture sample. 1.0 = use the map verbatim; 0 = flat shading.
// Mirrors glTF's `normalTexture.scale` so the slider in the materials panel
// has a live preview effect without re-baking the PNG.
uniform float u_normal_scale;
// Mirrors `Material::uv_scale`. Already baked into `v_uv` for texture
// sampling; surfaced separately so procedural shaders that need to dodge
// UV-parameterization singularities (water turbulence on a sphere/ellipsoid
// pole, for example) can evaluate their pattern in world space scaled by
// the same density knob authors expect.
uniform vec2 u_uv_scale;

// Two analytic key/fill lights for direct illumination plus a procedural sky
// dome that doubles as the IBL probe for ambient + specular reflection. The
// key/fill pair is the *fallback* used only when the scene declares no DSL
// lights — once `u_num_lights > 0`, direct illumination iterates the
// `u_light_*` arrays below instead.
uniform vec3 u_key_dir;
uniform vec3 u_fill_dir;
uniform vec3 u_sky_top;
uniform vec3 u_sky_horizon;
uniform vec3 u_sky_ground;
uniform vec3 u_sun_dir;
uniform vec3 u_sun_color;

// User-authored punctual lights (`KHR_lights_punctual`). Arrays are sized to
// MAX_LIGHTS in `lights.rs`; entries beyond `u_num_lights` are unread. Layout:
//   u_light_kind:  0=directional, 1=point, 2=spot
//   u_light_pos:   world-space position (point/spot only — ignored for dir)
//   u_light_dir:   world-space unit direction the light points along (the
//                  node's local -Z, transformed by world rotation)
//   u_light_color: linear RGB pre-multiplied by `intensity`
//   u_light_range: distance cutoff for point/spot; 0 = unlimited
//   u_light_cone:  (cos(inner), cos(outer)) for spot; (1,1) otherwise
const int MAX_LIGHTS = 8;
uniform int u_num_lights;
uniform int u_light_kind[MAX_LIGHTS];
uniform vec3 u_light_pos[MAX_LIGHTS];
uniform vec3 u_light_dir[MAX_LIGHTS];
uniform vec3 u_light_color[MAX_LIGHTS];
uniform float u_light_range[MAX_LIGHTS];
uniform vec2 u_light_cone[MAX_LIGHTS];

// Shadow mapping. Two parallel pools of depth maps:
//   - 2D array texture for directional + spot casters. Up to MAX_SHADOW_2D
//     slices (matches `MAX_SHADOW_2D` in `shadows.rs`).
//   - Per-caster cubemaps for point lights, capped at MAX_SHADOW_CUBE.
// `u_light_shadow_2d_idx[i]` selects which atlas slice (or -1 for "this
// light doesn't cast"); `u_light_shadow_cube_idx[i]` selects which cubemap
// slot, again -1 for "no shadow". A light can use at most one of the two —
// the renderer guarantees the indices are mutually exclusive.
//
// `u_shadow_fallback_idx` mirrors the same encoding but for the analytic
// key/fill fallback rig used when `u_num_lights == 0`. -1 disables.
const int MAX_SHADOW_2D = 4;
const int MAX_SHADOW_CUBE = 2;
uniform sampler2DArrayShadow u_shadow_2d;
uniform samplerCubeShadow u_shadow_cube0;
uniform samplerCubeShadow u_shadow_cube1;
uniform mat4 u_shadow_2d_viewproj[MAX_SHADOW_2D];
uniform vec3 u_shadow_cube_pos[MAX_SHADOW_CUBE];
uniform float u_shadow_cube_far[MAX_SHADOW_CUBE];
uniform int u_light_shadow_2d_idx[MAX_LIGHTS];
uniform int u_light_shadow_cube_idx[MAX_LIGHTS];
uniform int u_shadow_fallback_idx;
uniform float u_shadow_bias_const;
uniform float u_shadow_bias_slope;
uniform float u_shadow_strength;
// Inverse texel size of the 2D atlas — used to step the PCF kernel by
// whole texels regardless of map resolution. Single-tap hardware compare
// already does a 2×2 PCF tap; the manual kernel below adds extra taps
// only when [`ShadowQuality::pcf_taps`] asks for them.
uniform float u_shadow_2d_texel;
// Number of PCF taps per shadow lookup. 1 = hardware-only (cheapest, still
// looks fine on the lower presets). 5 = 5-sample Poisson disk for a softer
// penumbra. 9 = 9-sample disk. Driven by the active `ShadowQuality` preset
// so users on slower GPUs can dial the per-fragment cost down without
// touching map resolution.
uniform int u_shadow_pcf_taps;

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

// Three-band sky dome: zenith → horizon for the upper hemisphere, horizon →
// ground for the lower one. Smooth and low-frequency by design — safe to
// sample in the diffuse IBL path where high-frequency features (the sun
// disc) would otherwise leak in as fake specular highlights on rough
// surfaces.
vec3 sample_sky_dome(vec3 dir) {
    float y = clamp(dir.y, -1.0, 1.0);
    vec3 sky;
    if (y >= 0.0) {
        sky = mix(u_sky_horizon, u_sky_top, smoothstep(0.0, 1.0, y));
    } else {
        sky = mix(u_sky_horizon, u_sky_ground, smoothstep(0.0, 1.0, -y));
    }
    return sky;
}

// Dome plus a tight sun disc. Used for the specular reflection probe so a
// polished surface still mirrors the sun. Never call this from the diffuse
// path — at high roughness the prefilter mix collapses to the diffuse
// sample, and a sun-bearing sample there would produce a sharp bright spot
// where the surface normal happens to align with the sun, masquerading as
// specular on a fully matte material.
vec3 sample_sky(vec3 dir) {
    vec3 sky = sample_sky_dome(dir);
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

// Tileable water turbulence after David Hoskins (GLSL Sandbox), original
// turbulence by joltz0r. Period 1 in UV — `mod(uv*TAU, TAU)` wraps the
// argument exactly every 1.0 so the field tiles cleanly across the surface.
// Used by the water shader as a procedural caustic-like normal layer:
// authors keep `uv_scale` as the ripple-density knob (it's already baked
// into v_uv by `flatten`), while the bulk wave silhouette and foam come
// from the world-XZ directional sines. Returns a peaky height-like scalar
// (pow(., 8) at the tail) — finite-difference gradients of this read as
// thin caustic lines, which is the look we want.
float water_turbulence(vec2 uv) {
    const float TAU = 6.28318530718;
    float t = u_time * 0.5 + 23.0;
    vec2 p = mod(uv * TAU, TAU) - 250.0;
    vec2 i = p;
    float c = 1.0;
    float inten = 0.005;
    for (int n = 0; n < 5; ++n) {
        float tt = t * (1.0 - (3.5 / float(n + 1)));
        i = p + vec2(cos(tt - i.x) + sin(tt + i.y),
                     sin(tt - i.y) + cos(tt + i.x));
        c += 1.0 / length(vec2(p.x / (sin(i.x + tt) / inten),
                               p.y / (cos(i.y + tt) / inten)));
    }
    c /= 5.0;
    c = 1.17 - pow(c, 1.4);
    return pow(abs(c), 8.0);
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

// Pre-baked Poisson-disk offsets used by the multi-tap PCF path below. Eight
// directions evenly spaced on the unit circle plus a centre tap; the FS
// reads the prefix matching `u_shadow_pcf_taps` so we can dial taps without
// re-binding a different sampler.
const vec2 PCF_DISK[9] = vec2[9](
    vec2( 0.0,  0.0),
    vec2( 1.0,  0.0),
    vec2(-1.0,  0.0),
    vec2( 0.0,  1.0),
    vec2( 0.0, -1.0),
    vec2( 0.7071,  0.7071),
    vec2(-0.7071,  0.7071),
    vec2( 0.7071, -0.7071),
    vec2(-0.7071, -0.7071)
);

// Sample the 2D-array shadow atlas at slice `idx` for fragment `world`.
// Returns 1.0 = fully lit, 0.0 = fully shadowed. The hardware does the depth
// compare via `sampler2DArrayShadow`; the multi-tap loop adds Poisson-disk
// offsets when `u_shadow_pcf_taps > 1` for softer penumbras. Bias scales
// with surface slope so glancing surfaces avoid acne without losing contact
// between feet and ground.
float sample_shadow_2d(int idx, vec3 world, vec3 N, vec3 L) {
    if (idx < 0) return 1.0;
    mat4 vp = u_shadow_2d_viewproj[idx];
    vec4 clip = vp * vec4(world, 1.0);
    if (clip.w <= 0.0) return 1.0;
    vec3 ndc = clip.xyz / clip.w;
    vec3 uvz = ndc * 0.5 + 0.5;
    if (uvz.z > 1.0) return 1.0;
    if (uvz.x < 0.0 || uvz.x > 1.0 || uvz.y < 0.0 || uvz.y > 1.0) return 1.0;
    float NdL = max(dot(N, L), 0.0);
    float bias = u_shadow_bias_const + u_shadow_bias_slope * (1.0 - NdL);
    float ref = uvz.z - bias;
    int taps = clamp(u_shadow_pcf_taps, 1, 9);
    if (taps <= 1) {
        // Single hardware-PCF tap. The driver still does a 2×2 bilinear
        // depth compare under the hood, so this is already smoother than
        // a hard-edge nearest sample.
        return texture(u_shadow_2d, vec4(uvz.xy, float(idx), ref));
    }
    float texel = u_shadow_2d_texel;
    float sum = 0.0;
    for (int i = 0; i < taps; ++i) {
        vec2 ofs = PCF_DISK[i] * texel;
        sum += texture(u_shadow_2d, vec4(uvz.xy + ofs, float(idx), ref));
    }
    return sum / float(taps);
}

// Sample a point-light cubemap. World-space distance to the light is
// linearised against `far_plane` to recover the same depth value the depth
// pass wrote via `gl_FragDepth`. GLSL 330 forbids dynamic indexing into a
// sampler array, so we fan out the two slot cases with explicit `if`s.
float sample_shadow_cube(int idx, vec3 world, vec3 N, vec3 L) {
    if (idx < 0) return 1.0;
    if (idx > 1) return 1.0;
    vec3 lp = (idx == 0) ? u_shadow_cube_pos[0] : u_shadow_cube_pos[1];
    float far = (idx == 0) ? u_shadow_cube_far[0] : u_shadow_cube_far[1];
    if (far <= 0.0) return 1.0;
    vec3 to_frag = world - lp;
    float dist = length(to_frag);
    if (dist >= far) return 1.0;
    float NdL = max(dot(N, L), 0.0);
    float bias = u_shadow_bias_const + u_shadow_bias_slope * (1.0 - NdL);
    float ref = clamp(dist / far - bias, 0.0, 1.0);
    if (idx == 0) {
        return texture(u_shadow_cube0, vec4(to_frag, ref));
    }
    return texture(u_shadow_cube1, vec4(to_frag, ref));
}

// Resolve the per-light shadow factor for light index `i`. Returns 1.0 when
// no caster is wired up; otherwise blends towards `1.0 - u_shadow_strength`
// at the occluded extreme so even fully-shadowed regions retain a hint of
// light (matches DCC defaults; pure black shadows look broken under sky IBL).
float light_shadow_factor(int i, vec3 world, vec3 N, vec3 L) {
    int idx_2d = u_light_shadow_2d_idx[i];
    int idx_cube = u_light_shadow_cube_idx[i];
    float occ = 1.0;
    if (idx_2d >= 0) {
        occ = sample_shadow_2d(idx_2d, world, N, L);
    } else if (idx_cube >= 0) {
        occ = sample_shadow_cube(idx_cube, world, N, L);
    }
    return mix(1.0 - u_shadow_strength, 1.0, occ);
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
    vec2 uv = v_uv;
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
        // Per-material slope scaling, mirrored from `Material::normal_strength`
        // / glTF's `normalTexture.scale`. Re-normalising afterwards keeps the
        // vector unit-length, so a strength of 0 collapses cleanly to the
        // geometric normal.
        mapped.xy *= u_normal_scale;
        // Tangent space uses dFdx/dFdy, which only run on triangles with
        // varying UVs. Mapped vector is renormalised on output.
        N = normalize(cotangent_frame(Ngeom, v_world_pos, uv) * mapped);
    }

    float ao = (u_use_ao_tex == 1) ? texture(u_ao_tex, uv).r : 1.0;
    vec3 emissive = u_emissive * u_emissive_strength;
    if (u_use_emissive_tex == 1) {
        emissive *= texture(u_emissive_tex, uv).rgb;
    }

    // Water-shader pre-pass. Computes a wave-perturbed normal here so it's
    // ready when the main composition replaces `absorbed_tm` / `reflected_tm`
    // after the standard tonemap split. The standard PBR pipeline still runs
    // in between, but its output is overwritten — water is a near-mirror
    // dielectric whose appearance is dominated by sky reflection + sun glint
    // rather than the diffuse + small-spec mix the PBR path is built around.
    // Trying to coax water out of the PBR split alone produces a painted-blue
    // look (the diffuse contribution overpowers the reflection at low NdV).
    //
    // Material knobs that flow into the wave field:
    //   uv_scale        → ripple density (cycles per world unit). Higher
    //                     values pack more ripples into the same surface.
    //   normal_strength → slope multiplier on the procedural normal.
    //                     Mirrors how `u_normal_scale` scales authored
    //                     normal maps for standard materials.
    //   normal_texture  → blended on top of the procedural normal at half
    //                     strength so authors can layer extra detail
    //                     (drips, surface debris) without losing the ripple.
    if (u_material_shader == 1) {
        // Procedural turbulence normal. Domain is world-XZ scaled by
        // `u_uv_scale` so authors keep a per-material density knob without
        // touching `v_uv` — UV-driven patterns spiral around the poles of
        // spherical/ellipsoid unwraps, world-space evaluation has no such
        // singularity. Three-tap finite difference gives a world-XZ gradient
        // that we decode as a tangent-space normal and rotate via the
        // cotangent frame so it perturbs the geometric surface correctly.
        vec2 turb_uv = v_world_pos.xz * u_uv_scale;
        float h0 = water_turbulence(turb_uv);
        float eps = 0.002;
        float hu = water_turbulence(turb_uv + vec2(eps, 0.0));
        float hv = water_turbulence(turb_uv + vec2(0.0, eps));
        vec2 turb_grad = vec2(hu - h0, hv - h0) / eps;
        vec3 nt = normalize(vec3(-turb_grad * 0.03 * u_normal_scale, 1.0));
        vec3 N_proc = normalize(cotangent_frame(Ngeom, v_world_pos, v_uv) * nt);

        // Optional authored normal map blended on top. Half-weight mix
        // keeps the procedural ripple readable while letting the map add
        // extra high-frequency detail (drips, surface debris, etc.).
        if (u_use_normal_tex == 1) {
            vec3 mapped = texture(u_normal_tex, uv).xyz * 2.0 - 1.0;
            mapped.xy *= u_normal_scale;
            vec3 N_tex = normalize(cotangent_frame(N_proc, v_world_pos, uv) * mapped);
            N = normalize(mix(N_proc, N_tex, 0.5));
        } else {
            N = N_proc;
        }
    }

    vec3 F0 = mix(vec3(0.04), albedo, metallic);
    float diffuse_scale = 1.0 - u_transmission;

    // Direct lighting. When the scene declares its own punctual lights we
    // walk those; otherwise we fall back to a fixed warm-key + cool-fill rig
    // so untouched scenes still read well. Diffuse/specular are kept separate
    // so transmissive materials can composite them differently (see the
    // premultiplied-alpha output at the end).
    vec3 diff_direct = vec3(0.0);
    vec3 spec_direct = vec3(0.0);
    if (u_num_lights > 0) {
        // glTF KHR_lights_punctual attenuation:
        //   distance: window(d, range) / d^2
        //   spot:     clamp((cos(theta) - cos(outer)) / (cos(inner) - cos(outer)), 0, 1)
        // The window function smoothly fades to zero at `range` for point/spot
        // (unlimited / directional skip the falloff entirely).
        for (int i = 0; i < MAX_LIGHTS; ++i) {
            if (i >= u_num_lights) break;
            int kind = u_light_kind[i];
            vec3 L;
            float atten;
            if (kind == 0) {
                // Directional: light direction is fixed, no falloff.
                L = -normalize(u_light_dir[i]);
                atten = 1.0;
            } else {
                vec3 to_light = u_light_pos[i] - v_world_pos;
                float d = length(to_light);
                L = to_light / max(d, 1e-4);
                float r = u_light_range[i];
                float window;
                if (r > 0.0) {
                    float t = clamp(d / r, 0.0, 1.0);
                    float t4 = t * t * t * t;
                    window = clamp(1.0 - t4, 0.0, 1.0);
                } else {
                    window = 1.0;
                }
                atten = window / max(d * d, 1e-4);
                if (kind == 2) {
                    // Spot cone: cos_inner is bigger than cos_outer (smaller
                    // angle = bigger cosine). The cosine of the angle between
                    // the light's forward direction and the vector toward the
                    // shaded fragment must lie inside [cos_outer, cos_inner].
                    float ct = dot(L, -normalize(u_light_dir[i]));
                    vec2 cone = u_light_cone[i];
                    float denom = max(cone.x - cone.y, 1e-4);
                    float spot = clamp((ct - cone.y) / denom, 0.0, 1.0);
                    atten *= spot;
                }
            }
            if (atten <= 0.0) continue;
            vec3 dpart, spart;
            brdf_direct(N, V, L, albedo, metallic, roughness, F0, diffuse_scale, dpart, spart);
            float shadow = light_shadow_factor(i, v_world_pos, N, L);
            vec3 lc = u_light_color[i] * atten * shadow;
            diff_direct += dpart * lc;
            spec_direct += spart * lc;
        }
    } else {
        vec3 key_color  = vec3(1.00, 0.96, 0.90) * 1.10;
        vec3 fill_color = vec3(0.70, 0.78, 0.95) * 0.40;
        vec3 diff_key, spec_key, diff_fill, spec_fill;
        vec3 key_L  = normalize(-u_key_dir);
        vec3 fill_L = normalize(-u_fill_dir);
        brdf_direct(N, V, key_L,  albedo, metallic, roughness, F0, diffuse_scale, diff_key,  spec_key);
        brdf_direct(N, V, fill_L, albedo, metallic, roughness, F0, diffuse_scale, diff_fill, spec_fill);
        // Only the key (sun) direction is shadowed in the fallback rig; the
        // fill light is intentionally diffuse-only ambient and casting it
        // would produce banded double shadows on the ground.
        float key_shadow = 1.0;
        if (u_shadow_fallback_idx >= 0) {
            float occ = sample_shadow_2d(u_shadow_fallback_idx, v_world_pos, N, key_L);
            key_shadow = mix(1.0 - u_shadow_strength, 1.0, occ);
        }
        diff_direct = diff_key * key_color * key_shadow + diff_fill * fill_color;
        spec_direct = spec_key * key_color * key_shadow + spec_fill * fill_color;
    }

    // Image-based lighting from the analytic sky. The diffuse irradiance is a
    // crude average of the sky in the normal direction and straight up so the
    // result still has some directional structure but doesn't track the
    // reflection dir. The specular probe samples the actual reflection ray;
    // we fade it toward the diffuse sample as roughness rises to fake the
    // pre-filtered mip chain a real IBL would provide.
    float NdV = max(dot(N, V), 0.0);
    vec3 R = reflect(-V, N);
    vec3 spec_env = sample_sky(R);
    // Diffuse IBL pulls from the sun-free dome only. The sun is already
    // accounted for as a directional contribution in the direct lighting
    // pass, and the 256-power disc is far too high-frequency for a diffuse
    // probe — leaving it in produces a focused bright spot on rough
    // surfaces wherever N happens to align with the sun, which reads as
    // specular even at roughness=1.
    vec3 diff_env = 0.5 * sample_sky_dome(N) + 0.5 * sample_sky_dome(vec3(0.0, 1.0, 0.0));
    vec3 prefiltered = mix(spec_env, diff_env, roughness * roughness);
    vec3 F_ibl = F_Schlick_rough(NdV, F0, roughness);
    vec2 envBRDF = env_brdf_approx(NdV, roughness);
    vec3 specular_ibl = prefiltered * (F_ibl * envBRDF.x + envBRDF.y);
    vec3 kd_ibl = (1.0 - F_ibl) * (1.0 - metallic) * diffuse_scale;
    vec3 diffuse_ibl = kd_ibl * diff_env * albedo;

    // Split the lit radiance into two compositing channels:
    //   `absorbed` — diffuse light, fades with alpha in the blend.
    //   `reflected` — direct + IBL specular, stays at full strength so a
    //                 transmissive surface still shows environment reflections
    //                 (KHR_materials_transmission preview without SSR).
    // Emissive is intentionally excluded here and composited *after* AgX
    // (see below) — running it through the tonemap desaturates hot
    // emissives toward white, which is wrong for things like neon tubes
    // and warning lights where the authored hue is the whole point.
    // AO modulates both direct and indirect diffuse so cavities read as shaded
    // even under a dominant key light (pure IBL-only AO was invisible on
    // well-lit surfaces). Specular is damped slightly to avoid hotspots
    // glowing out of crevices.
    float ao_spec = mix(1.0, ao, 0.5);
    vec3 absorbed = (diff_direct + diffuse_ibl) * ao;
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

    // Emissive is composited in display space, after AgX, so the authored
    // hue survives at high `emissive_strength` (AgX intentionally rolls
    // saturated highlights toward white, which makes a "red emergency
    // light" at strength=8 look like a flat white panel). A mild Fresnel
    // rim boost (`pow(1-NdV, 2)`) brightens the grazing edge of curved
    // emissive surfaces — the cheap stand-in for a real bloom halo on a
    // neon tube or glowing ring. Per-channel `1 - exp(-x)` softly
    // saturates above 1 while preserving hue, so a strength sweep reads
    // as the channel getting brighter rather than greyer. The Toon and
    // CRT branches below replace `absorbed_tm` wholesale, so they take
    // their own emissive paths (Toon adds raw `emissive` in its band
    // sum; CRT/Matcap are stylistic and intentionally omit it).
    vec3 emissive_rim = emissive * (1.0 + 1.5 * pow(1.0 - NdV, 2.0));
    vec3 emissive_disp = vec3(1.0) - exp(-emissive_rim);
    absorbed_tm += emissive_disp;

    // Water-shader composition. Replaces the PBR pipeline's
    // `absorbed_tm` / `reflected_tm` outputs wholesale with a self-
    // contained mix of body tint + sky reflection + sun glint + foam.
    // The PBR work above ran (and even consumed the wave-perturbed N) —
    // it's overwritten here because the diffuse-dominated PBR split
    // can't produce a convincing water look by itself: at typical
    // top-down viewing angles the diffuse contribution drowns out the
    // 2% spec, and the surface reads as flat-painted rather than
    // reflective.
    //
    // Material knobs that flow into composition:
    //   metallic         → Fresnel F0 base. 0 = clean dielectric water
    //                      (2% reflectance head-on); 1 = liquid metal
    //                      (mercury, molten silver) where the body tint
    //                      *is* the reflection colour at all angles.
    //   roughness        → sky-reflection blur + sun-glint sharpness +
    //                      foam intensity. Tied here as the obvious
    //                      single "smooth ↔ choppy" knob.
    //   transmission     → see-through factor. 0 = opaque body; 1 = body
    //                      absorption fully recedes so the sky reflection
    //                      and what's behind the surface dominate.
    //   alpha_mode=Blend → translucent water (with Fresnel-rim opacity so
    //                      the silhouette stays visible at grazing angles).
    //                      Combine with `transmission` to dial how much of
    //                      the pool floor shows through.
    //   emissive         → glow (lava, magic potion, irradiated swamp).
    //                      Composited post-tonemap to preserve hue at
    //                      high `emissive_strength`, same as the standard
    //                      path.
    //   base_color_texture / emissive_texture → optional tint / glow maps.
    if (u_material_shader == 1) {
        float NdV_w = clamp(dot(N, V), 0.0, 1.0);

        // Body tint: optional texture multiplied onto the authored colour
        // lets authors paint shallow / deep variation across a single
        // surface (e.g. a darker drop-off in a coastal shelf).
        vec3 body_color = u_base_color;
        if (u_use_base_tex == 1) {
            body_color *= texture(u_base_tex, uv).rgb;
        }

        // Roughness drives reflection blur. We mix between the dome+sun
        // probe (sharp) and the dome-only probe (soft) so a smooth surface
        // mirrors the sun and a rough one only catches the diffuse sky.
        // The sun_glint lobe below adds the explicit sun-reflection peak
        // back in with its own roughness-dependent sharpness, so smooth
        // water gets *both* a sharp probe sample and a tight glint while
        // rough water dampens both together.
        vec3 R = reflect(-V, N);
        float blur = roughness * roughness;
        vec3 sky_reflect = mix(sample_sky(R), sample_sky_dome(R), blur);

        // Schlick Fresnel with metallic-aware F0. Dielectric water sits at
        // the standard 2% F0, ramping toward 100% at grazing — that
        // fresnel ramp is the whole reason water reads as water. Metallic
        // water (mercury) tints F0 with the body colour so the reflection
        // never goes white at the rim.
        vec3 F0w = mix(vec3(0.02), body_color, metallic);
        vec3 fresnel = F_Schlick(NdV_w, F0w);

        // Body absorption: authored colour modulated by the upper
        // hemisphere of the sky dome along the surface normal. Sampling
        // the dome (instead of using a constant) means an overcast scene
        // gives moody cool water and a sunset gives warm water without
        // the author having to retune `color`. Metallic surfaces don't
        // *have* a diffuse body — for them this term collapses to zero
        // and the visible colour comes entirely from the tinted Fresnel
        // reflection, exactly like a metal in the standard PBR path.
        vec3 body_amb = sample_sky_dome(N) * body_color * 0.9 * (1.0 - metallic);

        // Sun glint: tight Phong-style highlight where the sun's
        // reflection lines up with the wave normal. Exponent drops with
        // roughness so smooth water gets a laser-tight sparkle and rough
        // water gets a wide, soft halo. Strength tapers too so a stormy
        // surface doesn't cumulatively blow out.
        vec3 sun_L = -normalize(u_sun_dir);
        vec3 H_sun = normalize(V + sun_L);
        float NdH_sun = max(dot(N, H_sun), 0.0);
        float glint_exp = mix(360.0, 16.0, roughness);
        float glint_strength = mix(10.0, 2.0, roughness);
        vec3 sun_glint = u_sun_color * pow(NdH_sun, glint_exp) * glint_strength * fresnel;

        // Transmission scales body absorption away: at transmission=1 the
        // surface contributes only its specular reflection, matching the
        // KHR_materials_transmission spec for the "fully clear" extreme.
        vec3 absorbed_water = body_amb * (vec3(1.0) - fresnel) * (1.0 - u_transmission);
        vec3 reflected_water = sky_reflect * fresnel + sun_glint;

        // Same AgX path the standard branch uses, so water blends
        // tonally with the rest of the scene.
        vec3 combined_w = absorbed_water + reflected_water;
        vec3 tm_w = agx_tonemap(combined_w);
        vec3 frac_w = clamp(reflected_water / max(combined_w, vec3(1e-4)), vec3(0.0), vec3(1.0));
        reflected_tm = tm_w * frac_w;
        absorbed_tm = tm_w - reflected_tm;

        // Emissive composited post-tonemap — same trick as the standard
        // branch — so authored hue survives at high strength. Lets water
        // glow (lava, glowing potion, bioluminescent surf). The Fresnel
        // rim boost makes the grazing edge of curved emissive water
        // brighter, which reads as a bloom halo on the silhouette.
        vec3 emissive_water = emissive * (1.0 + 1.5 * pow(1.0 - NdV_w, 2.0));
        absorbed_tm += vec3(1.0) - exp(-emissive_water);

        // Alpha pipeline. Default opaque preserves the previous behaviour
        // (water reads as a fully-covering surface). `Blend` opens the
        // door to translucent water — combined with `transmission` this
        // is how authors get a clear pool with the tile floor visible
        // through it. The Fresnel rim restores opacity at grazing angles
        // so the water silhouette never disappears entirely, matching
        // glTF's transmission-with-alpha convention.
        if (u_alpha_mode == 2 || u_transmission > 0.0) {
            float clarity = (1.0 - u_transmission * 0.85);
            out_alpha = base_sample.a * clarity;
            float fresnel_rim = pow(1.0 - NdV_w, 5.0);
            out_alpha = mix(out_alpha, max(out_alpha, base_sample.a), fresnel_rim);
        } else {
            out_alpha = 1.0;
        }
    }

    // Style overrides. Stylistic modes don't model reflection-vs-transmission,
    // so they collapse everything into `absorbed_tm` and the transparent
    // result fades uniformly. Keep these branches in sync with the matching
    // `shader_mode` values in `preview_shader.rs`. Per-material shaders
    // (water) opt out — the global preview style is for stylistic looks
    // across the whole viewport, but water never makes sense as a CRT or
    // matcap surface, and the toon quantization breaks the wave shading.
    if (u_material_shader == 0 && u_shader_mode == 1) {
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
    } else if (u_material_shader == 0 && u_shader_mode == 2) {
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
    } else if (u_material_shader == 0 && u_shader_mode == 3) {
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
    // Water materials manage their own alpha (always opaque) — skip the
    // transmission ramp so their `out_alpha = 1.0` survives.
    if (u_material_shader == 0 && u_transmission > 0.0) {
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
