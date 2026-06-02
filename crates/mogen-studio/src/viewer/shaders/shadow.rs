/// Vertex shader for the directional / spot shadow depth pre-pass. Re-uses
/// the main mesh's interleaved VBO format and skinning palette so animated
/// rigs deform identically to the lit pass — the only difference is that the
/// camera matrix is the light-space `viewproj`. Output is depth-only; the
/// fragment shader writes nothing and `gl_Position` is all the rasterizer
/// needs to fill the depth buffer.
pub(crate) const SHADOW_DIR_VS: &str = r#"#version 330 core
layout (location = 0) in vec3 a_pos;
layout (location = 1) in vec3 a_normal;
layout (location = 2) in vec2 a_uv;
layout (location = 3) in vec4 a_joints;
layout (location = 4) in vec4 a_weights;

uniform mat4 u_light_viewproj;
uniform mat4 u_joint_mats[128];

void main() {
    ivec4 ji = clamp(ivec4(a_joints), ivec4(0), ivec4(127));
    mat4 palette = u_joint_mats[ji.x] * a_weights.x
                 + u_joint_mats[ji.y] * a_weights.y
                 + u_joint_mats[ji.z] * a_weights.z
                 + u_joint_mats[ji.w] * a_weights.w;
    vec4 pos4 = palette * vec4(a_pos, 1.0);
    gl_Position = u_light_viewproj * pos4;
}
"#;

/// Fragment shader for the directional / spot depth pre-pass. Empty body —
/// the depth buffer is written automatically from `gl_Position`. Kept
/// declared so the program object validates with a colour-mask-disabled
/// configuration on every driver.
pub(crate) const SHADOW_DIR_FS: &str = r#"#version 330 core
void main() {}
"#;

/// Vertex shader for the point-light cubemap depth pre-pass. Same skinning
/// path as the directional VS but additionally forwards the world-space
/// vertex position so the FS can compute linear depth.
pub(crate) const SHADOW_POINT_VS: &str = r#"#version 330 core
layout (location = 0) in vec3 a_pos;
layout (location = 1) in vec3 a_normal;
layout (location = 2) in vec2 a_uv;
layout (location = 3) in vec4 a_joints;
layout (location = 4) in vec4 a_weights;

uniform mat4 u_light_viewproj;
uniform mat4 u_joint_mats[128];

out vec3 v_world;

void main() {
    ivec4 ji = clamp(ivec4(a_joints), ivec4(0), ivec4(127));
    mat4 palette = u_joint_mats[ji.x] * a_weights.x
                 + u_joint_mats[ji.y] * a_weights.y
                 + u_joint_mats[ji.z] * a_weights.z
                 + u_joint_mats[ji.w] * a_weights.w;
    vec4 pos4 = palette * vec4(a_pos, 1.0);
    v_world = pos4.xyz;
    gl_Position = u_light_viewproj * pos4;
}
"#;

/// Fragment shader for the point-light depth pass. Writes linear distance to
/// the light, normalised by `u_far_plane`, into `gl_FragDepth` so the main
/// FS can compare against `length(world - light_pos) / far_plane` directly.
/// Hardware perspective depth would work too, but linear depth keeps PCF
/// kernel error uniform across the cube faces.
pub(crate) const SHADOW_POINT_FS: &str = r#"#version 330 core
in vec3 v_world;
uniform vec3 u_light_pos;
uniform float u_far_plane;
void main() {
    float d = length(v_world - u_light_pos) / max(u_far_plane, 1e-4);
    gl_FragDepth = clamp(d, 0.0, 1.0);
}
"#;
