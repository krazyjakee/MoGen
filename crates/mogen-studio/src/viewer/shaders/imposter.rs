/// Vertex shader for the viewport imposter billboard. Builds a Y-axis-
/// billboarded quad in world space from a per-vertex corner attribute
/// (`a_corner` ∈ {-1, +1}^2), the quad centre, and the camera's XZ-plane
/// position. Outputs the atlas UV mapped to a single cell selected by the
/// caller's `u_cell_offset_x` / `u_cell_scale_x` uniforms, matching the
/// godot-mog runtime convention (one row of N cells, atlas filled
/// left-to-right with increasing yaw).
pub(crate) const IMPOSTER_VS: &str = r#"#version 330 core
layout (location = 0) in vec2 a_corner;
layout (location = 1) in vec2 a_uv;
uniform mat4 u_viewproj;
uniform vec3 u_center;
uniform float u_half_width;
uniform float u_half_height;
uniform vec3 u_camera_pos;
uniform float u_cell_offset_x;
uniform float u_cell_scale_x;
uniform float u_uv_y_top;
uniform float u_uv_y_bottom;
out vec2 v_uv;
void main() {
    // Y-axis billboard: face the camera in the XZ plane, keep the quad
    // vertical (+Y up). The runtime imposter is a yaw-grid, so pitch is
    // baked into each cell — we don't tilt the quad. Quad width and
    // height come from the source model's AABB so the billboard
    // occupies the same volume the original mesh did (no stretching
    // square cells over the model).
    vec3 forward = vec3(u_camera_pos.x - u_center.x, 0.0, u_camera_pos.z - u_center.z);
    float len = length(forward);
    forward = len > 1e-4 ? forward / len : vec3(0.0, 0.0, 1.0);
    vec3 right = vec3(forward.z, 0.0, -forward.x);
    vec3 up = vec3(0.0, 1.0, 0.0);
    vec3 world = u_center
        + right * (a_corner.x * u_half_width)
        + up * (a_corner.y * u_half_height);
    gl_Position = u_viewproj * vec4(world, 1.0);
    // UV.x picks the cell column; UV.y is remapped from [0, 1] onto
    // the silhouette's V-range inside that cell so transparent margins
    // (above the model's apex, below its base) get cropped out.
    float v = mix(u_uv_y_top, u_uv_y_bottom, a_uv.y);
    v_uv = vec2(u_cell_offset_x + a_uv.x * u_cell_scale_x, v);
}
"#;

/// Fragment shader for the viewport imposter billboard. Samples the atlas
/// with the UV the VS rewrote into the selected cell's column and discards
/// fully-transparent fragments so the silhouette of the baked model
/// renders against the scene's background without a square border.
pub(crate) const IMPOSTER_FS: &str = r#"#version 330 core
in vec2 v_uv;
out vec4 frag;
uniform sampler2D u_atlas;
void main() {
    vec4 c = texture(u_atlas, v_uv);
    // Hard alpha cutoff — matches the writer's `alphaCutoff: 0.1` on the
    // exported imposter material, so the in-Studio preview shows the same
    // silhouette the godot-mog runtime / any glTF viewer renders.
    if (c.a < 0.1) discard;
    frag = c;
}
"#;
