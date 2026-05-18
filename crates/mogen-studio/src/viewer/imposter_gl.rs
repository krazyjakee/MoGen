//! Viewport imposter billboard. Renders the yaw-grid spritesheet the
//! `bundle_lods_and_imposter` export embeds as a single camera-facing quad,
//! sampling the cell that matches the current camera yaw. This is the
//! in-Studio equivalent of the godot-mog runtime's imposter shader — it
//! lets the user orbit and *see* the billboard cell-swap behaviour the
//! shipped GLB will exhibit, instead of just inspecting the raw atlas.
//!
//! Owns its own quad VBO + shader. The atlas texture is uploaded
//! externally (by [`ViewerState`](super::state::ViewerState) when an
//! imposter bake completes) and handed in by reference each draw.

use glam::{Mat4, Vec3};
use glow::HasContext;

use super::gl_util::{bytes_of_f32, compile_program};
use super::shaders::{IMPOSTER_FS, IMPOSTER_VS};

pub(super) struct ImposterGl {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    u_viewproj: Option<glow::UniformLocation>,
    u_center: Option<glow::UniformLocation>,
    u_half_width: Option<glow::UniformLocation>,
    u_half_height: Option<glow::UniformLocation>,
    u_camera_pos: Option<glow::UniformLocation>,
    u_cell_offset_x: Option<glow::UniformLocation>,
    u_cell_scale_x: Option<glow::UniformLocation>,
    u_uv_y_top: Option<glow::UniformLocation>,
    u_uv_y_bottom: Option<glow::UniformLocation>,
    u_atlas: Option<glow::UniformLocation>,
}

impl ImposterGl {
    pub(super) fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        unsafe {
            let program = compile_program(gl, IMPOSTER_VS, IMPOSTER_FS)?;
            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow::anyhow!("imposter vao: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("imposter vbo: {e}"))?;

            // Two triangles, 6 verts. Interleaved (corner.xy, uv.xy):
            // corner ∈ {-1, +1}^2 — VS scales by `u_half` to size the quad.
            // uv.x spans [0, 1] — VS rewrites into the selected atlas cell.
            // uv.y flipped so (0,0) UV reads the top of the atlas (the bake
            // is top-left origin, matching the texture upload below).
            let quad: [f32; 24] = [
                // bottom-left
                -1.0, -1.0, 0.0, 1.0, // bottom-right
                1.0, -1.0, 1.0, 1.0, // top-right
                1.0, 1.0, 1.0, 0.0, // bottom-left
                -1.0, -1.0, 0.0, 1.0, // top-right
                1.0, 1.0, 1.0, 0.0, // top-left
                -1.0, 1.0, 0.0, 0.0,
            ];

            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes_of_f32(&quad), glow::STATIC_DRAW);
            let stride = 4 * std::mem::size_of::<f32>() as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(
                1,
                2,
                glow::FLOAT,
                false,
                stride,
                (2 * std::mem::size_of::<f32>()) as i32,
            );
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);

            Ok(Self {
                program,
                vao,
                vbo,
                u_viewproj: gl.get_uniform_location(program, "u_viewproj"),
                u_center: gl.get_uniform_location(program, "u_center"),
                u_half_width: gl.get_uniform_location(program, "u_half_width"),
                u_half_height: gl.get_uniform_location(program, "u_half_height"),
                u_camera_pos: gl.get_uniform_location(program, "u_camera_pos"),
                u_cell_offset_x: gl.get_uniform_location(program, "u_cell_offset_x"),
                u_cell_scale_x: gl.get_uniform_location(program, "u_cell_scale_x"),
                u_uv_y_top: gl.get_uniform_location(program, "u_uv_y_top"),
                u_uv_y_bottom: gl.get_uniform_location(program, "u_uv_y_bottom"),
                u_atlas: gl.get_uniform_location(program, "u_atlas"),
            })
        }
    }

    /// Upload an atlas as an RGBA8 texture in CLAMP_TO_EDGE/LINEAR mode —
    /// same sampler the writer emits on the exported imposter material, so
    /// cell-border bleed matches between this preview and the runtime.
    /// Returns the GL texture handle; the caller is responsible for
    /// `destroy_texture`ing it when the cached atlas is replaced.
    pub(super) fn upload_atlas(
        gl: &glow::Context,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> anyhow::Result<glow::Texture> {
        unsafe {
            let tex = gl
                .create_texture()
                .map_err(|e| anyhow::anyhow!("imposter texture: {e}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::SRGB8_ALPHA8 as i32,
                width as i32,
                height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                Some(rgba),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
            Ok(tex)
        }
    }

    pub(super) fn destroy_texture(gl: &glow::Context, texture: glow::Texture) {
        unsafe { gl.delete_texture(texture) };
    }

    /// Draw the billboard. `center` is the AABB midpoint of the source
    /// model; `half_width` / `half_height` are the quad's half-extents in
    /// world coords (sized to match the model). `view_count` is the
    /// atlas's horizontal cell count; the shader picks the cell from the
    /// camera's XZ-plane yaw relative to `center`. `uv_y_top` /
    /// `uv_y_bottom` are the V-coordinate bounds inside one cell that
    /// contain the silhouette — the shader remaps the quad's UV.y onto
    /// this range so transparent cell margins crop out and the
    /// silhouette stretches across the quad.
    pub(super) fn draw(
        &self,
        gl: &glow::Context,
        viewproj: Mat4,
        camera_pos: Vec3,
        center: Vec3,
        half_width: f32,
        half_height: f32,
        view_count: u32,
        uv_y_top: f32,
        uv_y_bottom: f32,
        texture: glow::Texture,
    ) {
        let view_count = view_count.max(1);
        // Cell selection: camera yaw relative to center maps to a cell
        // index; matches the bake's convention (v=0 → camera at +Z, then
        // counter-clockwise around Y by `TAU / view_count` per step).
        let dx = camera_pos.x - center.x;
        let dz = camera_pos.z - center.z;
        let yaw = dx.atan2(dz);
        let tau = std::f32::consts::TAU;
        let norm = yaw.rem_euclid(tau) / tau;
        let cell = (norm * view_count as f32).round() as i32;
        let cell = cell.rem_euclid(view_count as i32) as u32;
        let cell_scale = 1.0 / view_count as f32;
        let cell_offset = cell as f32 * cell_scale;

        unsafe {
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(true);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
            // The atlas was uploaded sRGB; the renderer's main pass leaves
            // FRAMEBUFFER_SRGB enabled, but the grid pass toggles it off
            // before returning. Re-enable here so the imposter linearises
            // on sample and the resolved framebuffer matches the rest of
            // the scene.
            gl.enable(glow::FRAMEBUFFER_SRGB);

            if let Some(loc) = &self.u_viewproj {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &viewproj.to_cols_array());
            }
            if let Some(loc) = &self.u_center {
                gl.uniform_3_f32(Some(loc), center.x, center.y, center.z);
            }
            if let Some(loc) = &self.u_half_width {
                gl.uniform_1_f32(Some(loc), half_width);
            }
            if let Some(loc) = &self.u_half_height {
                gl.uniform_1_f32(Some(loc), half_height);
            }
            if let Some(loc) = &self.u_camera_pos {
                gl.uniform_3_f32(Some(loc), camera_pos.x, camera_pos.y, camera_pos.z);
            }
            if let Some(loc) = &self.u_cell_offset_x {
                gl.uniform_1_f32(Some(loc), cell_offset);
            }
            if let Some(loc) = &self.u_cell_scale_x {
                gl.uniform_1_f32(Some(loc), cell_scale);
            }
            if let Some(loc) = &self.u_uv_y_top {
                gl.uniform_1_f32(Some(loc), uv_y_top);
            }
            if let Some(loc) = &self.u_uv_y_bottom {
                gl.uniform_1_f32(Some(loc), uv_y_bottom);
            }
            if let Some(loc) = &self.u_atlas {
                gl.uniform_1_i32(Some(loc), 0);
            }
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));

            gl.draw_arrays(glow::TRIANGLES, 0, 6);

            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_vertex_array(None);
            gl.use_program(None);
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
