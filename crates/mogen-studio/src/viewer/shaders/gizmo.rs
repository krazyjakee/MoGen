pub(crate) const GIZMO_VS: &str = r#"#version 330 core
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

pub(crate) const GIZMO_FS: &str = r#"#version 330 core
in vec3 v_color;
out vec4 frag;
void main() {
    frag = vec4(v_color, 1.0);
}
"#;
