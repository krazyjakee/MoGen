use glam::{Mat4, Vec3};
use glow::HasContext;

use super::gl_util::{bytes_of_f32, compile_program};
use super::shaders::{GRID_FS, GRID_VS};

/// Infinite ground-plane grid. Renders a single fullscreen quad and
/// reconstructs each fragment's world-space ray from the inverse view-
/// projection, then intersects that ray with the Y = 0 plane in the FS.
/// The visible grid therefore always extends to the camera's actual horizon
/// — there's no fixed VBO extent to betray the trick when the camera pulls
/// back. Per-fragment depth is written so the scene occludes the grid the
/// same way it would a real ground plane.
pub(super) struct GridGl {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    u_inv_viewproj: Option<glow::UniformLocation>,
    u_viewproj: Option<glow::UniformLocation>,
    u_camera_pos: Option<glow::UniformLocation>,
}

impl GridGl {
    pub(super) fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        unsafe {
            let program = compile_program(gl, GRID_VS, GRID_FS)?;
            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow::anyhow!("grid vao: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("grid vbo: {e}"))?;

            // Two triangles covering NDC [-1, 1]^2 — the rest of the work
            // happens per-fragment.
            let quad: [f32; 12] = [
                -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0,
            ];

            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes_of_f32(&quad), glow::STATIC_DRAW);
            let stride = 2 * std::mem::size_of::<f32>() as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);

            Ok(Self {
                program,
                vao,
                vbo,
                u_inv_viewproj: gl.get_uniform_location(program, "u_inv_viewproj"),
                u_viewproj: gl.get_uniform_location(program, "u_viewproj"),
                u_camera_pos: gl.get_uniform_location(program, "u_camera_pos"),
            })
        }
    }

    pub(super) fn draw(&self, gl: &glow::Context, viewproj: Mat4, camera_pos: Vec3) {
        let inv_viewproj = viewproj.inverse();
        unsafe {
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            // Grid writes per-fragment depth via gl_FragDepth so the depth
            // test against the scene works correctly, but we keep depth-mask
            // off so the grid never shadows transparent batches that draw
            // afterwards.
            gl.depth_mask(false);
            gl.disable(glow::CULL_FACE);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            // Match the scene pass's sRGB encode so the grid's grey doesn't
            // look washed-out next to the tonemapped model.
            gl.enable(glow::FRAMEBUFFER_SRGB);

            if let Some(loc) = &self.u_inv_viewproj {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &inv_viewproj.to_cols_array());
            }
            if let Some(loc) = &self.u_viewproj {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &viewproj.to_cols_array());
            }
            if let Some(loc) = &self.u_camera_pos {
                gl.uniform_3_f32(Some(loc), camera_pos.x, camera_pos.y, camera_pos.z);
            }

            gl.draw_arrays(glow::TRIANGLES, 0, 6);

            gl.bind_vertex_array(None);
            gl.use_program(None);
            // Restore the state the main pass expects on entry.
            gl.depth_mask(true);
            gl.disable(glow::BLEND);
            gl.enable(glow::CULL_FACE);
            gl.disable(glow::FRAMEBUFFER_SRGB);
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
