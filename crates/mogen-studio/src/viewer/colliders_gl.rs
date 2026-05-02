//! Wireframe overlay for nodes carrying an AABB collider. Draws the 12 edges
//! of the node's local AABB transformed into world space so the user can see
//! collider bounds without a physics simulation. Off by default; toggled by
//! the viewport context menu's "Show Colliders" checkbox.

use glam::{Mat4, Quat};
use glow::HasContext;

use super::gl_util::{bytes_of_f32, compile_program};
use mogen_core::{Aabb, NodeId};

const VS: &str = r#"#version 330 core
layout (location = 0) in vec3 a_pos;
uniform mat4 u_viewproj;
uniform mat4 u_model;
void main() {
    gl_Position = u_viewproj * u_model * vec4(a_pos, 1.0);
}
"#;

const FS: &str = r#"#version 330 core
uniform vec3 u_color;
out vec4 frag;
void main() {
    frag = vec4(u_color, 1.0);
}
"#;

/// One AABB collider draw entry: the world-space transform of the node that
/// owns the collider, the local-space AABB, and whether the node is currently
/// selected (so the overlay can highlight it).
pub(crate) struct ColliderInstance {
    pub world: Mat4,
    pub aabb: Aabb,
    pub selected: bool,
}

pub(super) struct CollidersGl {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    u_viewproj: Option<glow::UniformLocation>,
    u_model: Option<glow::UniformLocation>,
    u_color: Option<glow::UniformLocation>,
    line_count: i32,
}

impl CollidersGl {
    pub(super) fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        unsafe {
            let program = compile_program(gl, VS, FS)?;
            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow::anyhow!("colliders vao: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("colliders vbo: {e}"))?;

            // 12 edges of a unit cube in local space, half-extent 1 around the
            // origin. The per-instance model matrix scales by the AABB's
            // half-extents and translates to its centre.
            let mut data: Vec<f32> = Vec::with_capacity(12 * 2 * 3);
            let lo = -1.0;
            let hi = 1.0;
            let corners = [
                [lo, lo, lo], [hi, lo, lo], [hi, lo, hi], [lo, lo, hi],
                [lo, hi, lo], [hi, hi, lo], [hi, hi, hi], [lo, hi, hi],
            ];
            // Bottom rectangle (y = lo).
            push_line(&mut data, corners[0], corners[1]);
            push_line(&mut data, corners[1], corners[2]);
            push_line(&mut data, corners[2], corners[3]);
            push_line(&mut data, corners[3], corners[0]);
            // Top rectangle (y = hi).
            push_line(&mut data, corners[4], corners[5]);
            push_line(&mut data, corners[5], corners[6]);
            push_line(&mut data, corners[6], corners[7]);
            push_line(&mut data, corners[7], corners[4]);
            // Verticals.
            push_line(&mut data, corners[0], corners[4]);
            push_line(&mut data, corners[1], corners[5]);
            push_line(&mut data, corners[2], corners[6]);
            push_line(&mut data, corners[3], corners[7]);

            let line_count = (data.len() / 3) as i32;

            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytes_of_f32(&data),
                glow::STATIC_DRAW,
            );
            let stride = 3 * std::mem::size_of::<f32>() as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);

            Ok(Self {
                program,
                vao,
                vbo,
                u_viewproj: gl.get_uniform_location(program, "u_viewproj"),
                u_model: gl.get_uniform_location(program, "u_model"),
                u_color: gl.get_uniform_location(program, "u_color"),
                line_count,
            })
        }
    }

    pub(super) fn draw(
        &self,
        gl: &glow::Context,
        viewproj: Mat4,
        instances: &[ColliderInstance],
    ) {
        if instances.is_empty() {
            return;
        }
        unsafe {
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(false);
            gl.disable(glow::CULL_FACE);
            gl.line_width(1.5);

            if let Some(loc) = &self.u_viewproj {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &viewproj.to_cols_array());
            }

            for inst in instances {
                let extent = (inst.aabb.max - inst.aabb.min) * 0.5;
                if extent.length_squared() < 1e-12 {
                    continue;
                }
                let center = (inst.aabb.min + inst.aabb.max) * 0.5;
                let local = Mat4::from_scale_rotation_translation(extent, Quat::IDENTITY, center);
                let model = inst.world * local;
                if let Some(loc) = &self.u_model {
                    gl.uniform_matrix_4_f32_slice(Some(loc), false, &model.to_cols_array());
                }
                let color = if inst.selected {
                    [1.0, 0.85, 0.0]
                } else {
                    [0.20, 0.90, 0.40]
                };
                if let Some(loc) = &self.u_color {
                    gl.uniform_3_f32(Some(loc), color[0], color[1], color[2]);
                }
                gl.draw_arrays(glow::LINES, 0, self.line_count);
            }

            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.depth_mask(true);
            gl.enable(glow::CULL_FACE);
        }
    }

    pub(super) fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
        }
    }
}

fn push_line(buf: &mut Vec<f32>, a: [f32; 3], b: [f32; 3]) {
    buf.extend_from_slice(&a);
    buf.extend_from_slice(&b);
}

/// Build a list of [`ColliderInstance`]s from the scene's per-node world
/// transforms. Walks every node carrying a [`mogen_core::SceneNode::collider`]
/// and pairs it with the matching world matrix + selection state.
pub(super) fn collect(
    scene: &mogen_core::SceneGraph,
    worlds: &[Mat4],
    selected: &[NodeId],
) -> Vec<ColliderInstance> {
    let mut out = Vec::new();
    for (i, node) in scene.nodes.iter().enumerate() {
        let Some(aabb) = node.collider else { continue };
        let world = worlds.get(i).copied().unwrap_or(Mat4::IDENTITY);
        let id = NodeId(i as u32);
        out.push(ColliderInstance {
            world,
            aabb,
            selected: selected.contains(&id),
        });
    }
    out
}
