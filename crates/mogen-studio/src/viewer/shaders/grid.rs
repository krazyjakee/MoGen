pub(crate) const GRID_VS: &str = r#"#version 330 core
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

pub(crate) const GRID_FS: &str = r#"#version 330 core
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
