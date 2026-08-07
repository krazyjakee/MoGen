// A user-authored fragment snippet. Bound to a material by
// examples/features/custom_shader.mog.
//
// The ABI: this file is a *body*, not a whole program. It is injected into the
// viewport's fragment program and runs against the prelude that program already
// defines:
//
//   varyings   v_world_pos, v_normal, v_uv, v_color
//   frame      u_time, u_camera_pos
//   material   u_base_color, u_roughness, u_metallic, u_emissive, …
//   helpers    sample_sky(dir), water_turbulence(uv), agx_tonemap(rgb)
//
// It must define `vec4 fragment()` returning the final RGBA. That is the
// "replace" contract — the snippet does its own lighting and standard PBR is
// bypassed entirely, rather than being layered on top.
//
// Declared `param`s arrive under their bare names. The assembler namespaces
// them per shader id (`u_sh2_speed`) and `#define`s them back, so two shaders
// can each declare a `speed` without colliding.

vec4 fragment() {
    vec3 N = normalize(v_normal);
    vec3 V = normalize(u_camera_pos - v_world_pos);

    // Concentric rings travelling outward from the world origin. Using world
    // position rather than v_uv means the rings stay continuous across separate
    // meshes instead of restarting per-object.
    float dist = length(v_world_pos.xz);
    float rings = sin(dist * frequency - u_time * speed) * 0.5 + 0.5;

    // Sharpen the crests so the bands read as distinct ripples rather than a
    // smooth gradient.
    rings = pow(rings, 3.0);

    // Crests take the tint; troughs keep the material's own base colour, so
    // editing `color` on the material still does something visible.
    vec3 albedo = mix(u_base_color, tint, rings);

    // Wrapped diffuse from a fixed key direction. Half-Lambert keeps the
    // shaded side readable instead of crushing it to black.
    float ndl = dot(N, normalize(vec3(0.4, 0.8, 0.3))) * 0.5 + 0.5;

    // Schlick-style rim: grazing angles pick up the sky, which is what stops a
    // flat slab reading as a sticker.
    float rim = pow(1.0 - max(dot(N, V), 0.0), 5.0);
    vec3 sky = sample_sky(reflect(-V, N));

    return vec4(albedo * ndl + sky * rim * 0.6, 1.0);
}
