use glam::{Mat4, Vec3};
use glow::HasContext;

use super::gl_util::{bytes_of_f32, compile_program};
use super::shaders::{GIZMO_FS, GIZMO_VS};

pub(super) struct GizmoGl {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    u_viewproj: Option<glow::UniformLocation>,
    u_origin: Option<glow::UniformLocation>,
    u_scale: Option<glow::UniformLocation>,
    /// Vertex count for each of the three sub-passes. The VBO lays them out
    /// contiguously: [translate ... | rotate ... | scale ...]. Drawn as
    /// GL_LINES (translate/rotate) or GL_TRIANGLES (scale cubes) — tracked
    /// per-range since mixing primitives in one draw call isn't allowed.
    translate_start: i32,
    translate_count: i32,
    rotate_start: i32,
    rotate_count: i32,
    scale_lines_start: i32,
    scale_lines_count: i32,
    scale_tris_start: i32,
    scale_tris_count: i32,
}

impl GizmoGl {
    pub(super) fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        unsafe {
            let program = compile_program(gl, GIZMO_VS, GIZMO_FS)?;
            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow::anyhow!("gizmo vao: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("gizmo vbo: {e}"))?;

            // Build static vertex data: 3 axis shafts, 3 rotate rings, 3 scale
            // shafts + cubes. Layout: [px,py,pz, r,g,b] per vertex.
            let mut data: Vec<f32> = Vec::new();
            let colors = [
                [1.0f32, 0.25, 0.25], // X: red
                [0.25, 1.0, 0.25],    // Y: green
                [0.25, 0.25, 1.0],    // Z: blue
            ];
            let axis_vec = |i: usize| match i {
                0 => [1.0f32, 0.0, 0.0],
                1 => [0.0, 1.0, 0.0],
                _ => [0.0, 0.0, 1.0],
            };

            // --- Translate shafts (GL_LINES).
            let translate_start = (data.len() / 6) as i32;
            for i in 0..3 {
                let a = axis_vec(i);
                data.extend_from_slice(&[0.0, 0.0, 0.0]);
                data.extend_from_slice(&colors[i]);
                data.extend_from_slice(&a);
                data.extend_from_slice(&colors[i]);
            }
            let translate_count = (data.len() / 6) as i32 - translate_start;

            // --- Rotate rings (GL_LINES), 48 segments so a full loop reads
            //     as a circle not a polygon.
            let rotate_start = (data.len() / 6) as i32;
            let segments = 48usize;
            for i in 0..3 {
                let c = colors[i];
                // For axis i, the ring lies in the plane perpendicular to it.
                let (u_axis, v_axis) = match i {
                    0 => (1usize, 2usize), // X: Y-Z plane
                    1 => (0usize, 2usize), // Y: X-Z plane
                    _ => (0usize, 1usize), // Z: X-Y plane
                };
                for s in 0..segments {
                    let a0 = std::f32::consts::TAU * (s as f32) / (segments as f32);
                    let a1 = std::f32::consts::TAU * ((s + 1) as f32) / (segments as f32);
                    let mut p0 = [0.0f32; 3];
                    let mut p1 = [0.0f32; 3];
                    p0[u_axis] = a0.cos();
                    p0[v_axis] = a0.sin();
                    p1[u_axis] = a1.cos();
                    p1[v_axis] = a1.sin();
                    data.extend_from_slice(&p0);
                    data.extend_from_slice(&c);
                    data.extend_from_slice(&p1);
                    data.extend_from_slice(&c);
                }
            }
            let rotate_count = (data.len() / 6) as i32 - rotate_start;

            // --- Scale shafts (GL_LINES).
            let scale_lines_start = (data.len() / 6) as i32;
            for i in 0..3 {
                let a = axis_vec(i);
                data.extend_from_slice(&[0.0, 0.0, 0.0]);
                data.extend_from_slice(&colors[i]);
                data.extend_from_slice(&a);
                data.extend_from_slice(&colors[i]);
            }
            let scale_lines_count = (data.len() / 6) as i32 - scale_lines_start;

            // --- Scale tip cubes (GL_TRIANGLES). Cube half-extent 0.1,
            //     centred on (±1, 0, 0) etc. 12 triangles per cube = 36 verts.
            let scale_tris_start = (data.len() / 6) as i32;
            let half = 0.1f32;
            for i in 0..3 {
                let mut centre = [0.0f32; 3];
                centre[i] = 1.0;
                let c = colors[i];
                let corners = [
                    [-half, -half, -half],
                    [ half, -half, -half],
                    [ half,  half, -half],
                    [-half,  half, -half],
                    [-half, -half,  half],
                    [ half, -half,  half],
                    [ half,  half,  half],
                    [-half,  half,  half],
                ];
                // 6 faces × 2 triangles × 3 verts = 36.
                let faces: [[usize; 6]; 6] = [
                    [0, 2, 1, 0, 3, 2],
                    [4, 5, 6, 4, 6, 7],
                    [0, 1, 5, 0, 5, 4],
                    [3, 7, 6, 3, 6, 2],
                    [1, 2, 6, 1, 6, 5],
                    [0, 4, 7, 0, 7, 3],
                ];
                for face in faces {
                    for idx in face {
                        let p = corners[idx];
                        data.extend_from_slice(&[
                            centre[0] + p[0],
                            centre[1] + p[1],
                            centre[2] + p[2],
                        ]);
                        data.extend_from_slice(&c);
                    }
                }
            }
            let scale_tris_count = (data.len() / 6) as i32 - scale_tris_start;

            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytes_of_f32(&data),
                glow::STATIC_DRAW,
            );
            let f = std::mem::size_of::<f32>() as i32;
            let stride = 6 * f;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, stride, 3 * f);
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);

            Ok(Self {
                program,
                vao,
                vbo,
                u_viewproj: gl.get_uniform_location(program, "u_viewproj"),
                u_origin: gl.get_uniform_location(program, "u_origin"),
                u_scale: gl.get_uniform_location(program, "u_scale"),
                translate_start,
                translate_count,
                rotate_start,
                rotate_count,
                scale_lines_start,
                scale_lines_count,
                scale_tris_start,
                scale_tris_count,
            })
        }
    }

    pub(super) fn draw(
        &self,
        gl: &glow::Context,
        viewproj: Mat4,
        origin: Vec3,
        scale: f32,
        mode: crate::gizmo::GizmoMode,
    ) {
        use crate::gizmo::GizmoMode;
        unsafe {
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            // Keep the gizmo on top: depth-always, no depth writes, no culling.
            gl.depth_func(glow::ALWAYS);
            gl.depth_mask(false);
            gl.disable(glow::CULL_FACE);
            gl.line_width(2.0);
            if let Some(loc) = &self.u_viewproj {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &viewproj.to_cols_array());
            }
            if let Some(loc) = &self.u_origin {
                gl.uniform_3_f32(Some(loc), origin.x, origin.y, origin.z);
            }
            if let Some(loc) = &self.u_scale {
                gl.uniform_1_f32(Some(loc), scale);
            }
            match mode {
                GizmoMode::Translate => {
                    gl.draw_arrays(glow::LINES, self.translate_start, self.translate_count);
                }
                GizmoMode::Rotate => {
                    gl.draw_arrays(glow::LINES, self.rotate_start, self.rotate_count);
                }
                GizmoMode::Scale => {
                    gl.draw_arrays(
                        glow::LINES,
                        self.scale_lines_start,
                        self.scale_lines_count,
                    );
                    gl.draw_arrays(
                        glow::TRIANGLES,
                        self.scale_tris_start,
                        self.scale_tris_count,
                    );
                }
            }
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.depth_func(glow::LESS);
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
