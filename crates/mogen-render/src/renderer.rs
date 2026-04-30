use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use glam::{Mat4, Vec3};
use glow::HasContext;
use mogen_core::AlphaMode;

use crate::flatten::{DrawBatch, FlatMesh, SkinPalette, FLOATS_PER_VERTEX, MAX_JOINTS};
use crate::gizmo_gl::GizmoGl;
use crate::gl_util::{bytes_of_f32, bytes_of_u32, compile_program, try_load_texture};
use crate::grid_gl::GridGl;
use crate::shaders::{FS_SRC, VS_SRC};

pub struct Renderer {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    u_viewproj: Option<glow::UniformLocation>,
    u_camera_pos: Option<glow::UniformLocation>,
    u_key_dir: Option<glow::UniformLocation>,
    u_fill_dir: Option<glow::UniformLocation>,
    u_sky_top: Option<glow::UniformLocation>,
    u_sky_horizon: Option<glow::UniformLocation>,
    u_sky_ground: Option<glow::UniformLocation>,
    u_sun_dir: Option<glow::UniformLocation>,
    u_sun_color: Option<glow::UniformLocation>,
    u_base_color: Option<glow::UniformLocation>,
    u_base_color_alpha: Option<glow::UniformLocation>,
    u_metallic: Option<glow::UniformLocation>,
    u_roughness: Option<glow::UniformLocation>,
    u_emissive: Option<glow::UniformLocation>,
    u_emissive_strength: Option<glow::UniformLocation>,
    u_transmission: Option<glow::UniformLocation>,
    u_alpha_mode: Option<glow::UniformLocation>,
    u_alpha_cutoff: Option<glow::UniformLocation>,
    u_use_base_tex: Option<glow::UniformLocation>,
    u_use_mr_tex: Option<glow::UniformLocation>,
    u_use_normal_tex: Option<glow::UniformLocation>,
    u_use_ao_tex: Option<glow::UniformLocation>,
    u_use_emissive_tex: Option<glow::UniformLocation>,
    u_base_tex: Option<glow::UniformLocation>,
    u_mr_tex: Option<glow::UniformLocation>,
    u_normal_tex: Option<glow::UniformLocation>,
    u_ao_tex: Option<glow::UniformLocation>,
    u_emissive_tex: Option<glow::UniformLocation>,
    u_joint_mats: Option<glow::UniformLocation>,
    u_shader_mode: Option<glow::UniformLocation>,
    /// Preview style selector. The fragment shader switches on its integer
    /// value; a second CPU-side effect (polygon mode LINE) drives the
    /// wireframe mode which doesn't need a distinct shader branch.
    shader_mode: i32,
    /// True when the caller wants wireframe rendering. Drives
    /// `glPolygonMode` in `draw`; kept separate from `shader_mode` because
    /// wireframe's shader path is just the standard PBR output.
    wireframe: bool,
    /// Cached uploaded textures keyed by (resolved filesystem path, sRGB flag).
    /// Albedo and emissive maps load as sRGB; normal, MR, and AO load as
    /// linear, so the same PNG used in two roles would need two separate GL
    /// objects. Entries are tagged with the file's mtime at load time so that
    /// regenerating a PNG (e.g. via `Generate Textures`) invalidates the cache
    /// without an explicit refresh button. A `None` `texture` means we tried
    /// and failed to load — kept so we don't re-log the same failure every
    /// frame.
    texture_cache: HashMap<(PathBuf, bool), CachedTexture>,
    vbo_bytes: usize,
    ebo_bytes: usize,
    /// Per-batch draw plan owned by the renderer so the paint callback doesn't
    /// have to re-read the mesh on every frame.
    batches: Vec<DrawBatch>,
    /// Latest per-batch matrix palettes. Indexed by `DrawBatch::palette_id`;
    /// the same array carries both rigid (single-bone-per-vertex) and
    /// skinned palettes — the shader doesn't care which is which. Refreshed
    /// in `upload` alongside the vertex buffer, or by `upload_palettes`
    /// alone when only the pose changed.
    palettes: Vec<SkinPalette>,
    /// Gizmo overlay pipeline. Drawn after the main scene with depth-always
    /// so it's visible through geometry. Kept as a separate program from the
    /// PBR pass because its vertex format (pos + color only) is different.
    gizmo: GizmoGl,
    /// Ground-plane reference grid. Drawn before the main scene so the
    /// scene's depth-test naturally occludes lines behind solid geometry.
    grid: GridGl,
}

/// One entry in [`Renderer::texture_cache`].
struct CachedTexture {
    /// File mtime captured at load time. Used to detect on-disk changes —
    /// regenerated PNGs will have a newer mtime, which forces a reload.
    mtime: Option<SystemTime>,
    /// The uploaded GL texture, or `None` if loading failed.
    texture: Option<glow::Texture>,
}

impl Renderer {
    pub fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        unsafe {
            let program = compile_program(gl, VS_SRC, FS_SRC)?;

            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow::anyhow!("create_vertex_array: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("create_buffer (vbo): {e}"))?;
            let ebo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("create_buffer (ebo): {e}"))?;

            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));

            let f = std::mem::size_of::<f32>() as i32;
            let stride = (FLOATS_PER_VERTEX as i32) * f;
            // Layout matches FLOATS_PER_VERTEX: pos | normal | uv | joints | weights.
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, stride, 3 * f);
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 2, glow::FLOAT, false, stride, 6 * f);
            gl.enable_vertex_attrib_array(3);
            gl.vertex_attrib_pointer_f32(3, 4, glow::FLOAT, false, stride, 8 * f);
            gl.enable_vertex_attrib_array(4);
            gl.vertex_attrib_pointer_f32(4, 4, glow::FLOAT, false, stride, 12 * f);

            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);

            let u = |n: &str| gl.get_uniform_location(program, n);
            let u_viewproj = u("u_viewproj");
            let u_camera_pos = u("u_camera_pos");
            let u_key_dir = u("u_key_dir");
            let u_fill_dir = u("u_fill_dir");
            let u_sky_top = u("u_sky_top");
            let u_sky_horizon = u("u_sky_horizon");
            let u_sky_ground = u("u_sky_ground");
            let u_sun_dir = u("u_sun_dir");
            let u_sun_color = u("u_sun_color");
            let u_base_color = u("u_base_color");
            let u_base_color_alpha = u("u_base_color_alpha");
            let u_metallic = u("u_metallic");
            let u_roughness = u("u_roughness");
            let u_emissive = u("u_emissive");
            let u_emissive_strength = u("u_emissive_strength");
            let u_transmission = u("u_transmission");
            let u_alpha_mode = u("u_alpha_mode");
            let u_alpha_cutoff = u("u_alpha_cutoff");
            let u_use_base_tex = u("u_use_base_tex");
            let u_use_mr_tex = u("u_use_mr_tex");
            let u_use_normal_tex = u("u_use_normal_tex");
            let u_use_ao_tex = u("u_use_ao_tex");
            let u_use_emissive_tex = u("u_use_emissive_tex");
            let u_base_tex = u("u_base_tex");
            let u_mr_tex = u("u_mr_tex");
            let u_normal_tex = u("u_normal_tex");
            let u_ao_tex = u("u_ao_tex");
            let u_emissive_tex = u("u_emissive_tex");
            let u_joint_mats = u("u_joint_mats[0]");
            let u_shader_mode = u("u_shader_mode");

            let gizmo = GizmoGl::new(gl)?;
            let grid = GridGl::new(gl)?;

            Ok(Self {
                program,
                vao,
                vbo,
                ebo,
                gizmo,
                grid,
                u_viewproj,
                u_camera_pos,
                u_key_dir,
                u_fill_dir,
                u_sky_top,
                u_sky_horizon,
                u_sky_ground,
                u_sun_dir,
                u_sun_color,
                u_base_color,
                u_base_color_alpha,
                u_metallic,
                u_roughness,
                u_emissive,
                u_emissive_strength,
                u_transmission,
                u_alpha_mode,
                u_alpha_cutoff,
                u_use_base_tex,
                u_use_mr_tex,
                u_use_normal_tex,
                u_use_ao_tex,
                u_use_emissive_tex,
                u_base_tex,
                u_mr_tex,
                u_normal_tex,
                u_ao_tex,
                u_emissive_tex,
                u_joint_mats,
                u_shader_mode,
                shader_mode: 0,
                wireframe: false,
                texture_cache: HashMap::new(),
                vbo_bytes: 0,
                ebo_bytes: 0,
                batches: Vec::new(),
                palettes: Vec::new(),
            })
        }
    }

    /// Set the preview style. `shader_mode` is handed straight to the
    /// fragment shader; `wireframe` flips the renderer into polygon-mode
    /// LINE with culling off so back-facing edges remain visible.
    pub fn set_preview(&mut self, shader_mode: i32, wireframe: bool) {
        self.shader_mode = shader_mode;
        self.wireframe = wireframe;
    }

    pub fn upload(&mut self, gl: &glow::Context, mesh: &FlatMesh) {
        unsafe {
            gl.bind_vertex_array(Some(self.vao));
            let vtx_bytes: &[u8] = bytes_of_f32(&mesh.vertices);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            if vtx_bytes.len() > self.vbo_bytes {
                gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, vtx_bytes, glow::DYNAMIC_DRAW);
                self.vbo_bytes = vtx_bytes.len();
            } else {
                gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, vtx_bytes);
            }
            let idx_bytes: &[u8] = bytes_of_u32(&mesh.indices);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.ebo));
            if idx_bytes.len() > self.ebo_bytes {
                gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, idx_bytes, glow::DYNAMIC_DRAW);
                self.ebo_bytes = idx_bytes.len();
            } else {
                gl.buffer_sub_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, 0, idx_bytes);
            }
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
            self.batches = mesh.batches.clone();
            self.palettes = mesh.palettes.clone();
        }
    }

    /// Refresh the per-batch matrix palettes without touching the VBO/EBO.
    /// Called every animation tick + every gizmo drag tick — the whole point
    /// of the rest-pose-baked vertex stream is that this is the only GPU
    /// upload required when the pose moves.
    pub fn upload_palettes(&mut self, palettes: &[SkinPalette]) {
        self.palettes.clear();
        self.palettes.extend_from_slice(palettes);
    }

    /// Look up a texture in the cache, decoding the PNG and uploading on the
    /// first miss. `srgb` selects the GPU storage format: albedo and emissive
    /// maps need sRGB so the GL pipeline linearises them on read; metallic-
    /// roughness, normal, and AO maps store data, not colour, and must be
    /// linear. Re-loads when the file's mtime changes so regenerated PNGs
    /// (e.g. after `Generate Textures`) become visible without restarting.
    /// Returns `None` for paths that fail to load — the caller falls back to
    /// the corresponding scalar uniform.
    fn ensure_texture(
        &mut self,
        gl: &glow::Context,
        path: &Path,
        srgb: bool,
    ) -> Option<glow::Texture> {
        let mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok();
        let key = (path.to_path_buf(), srgb);

        if let Some(cached) = self.texture_cache.get(&key) {
            if cached.mtime == mtime {
                return cached.texture;
            }
            // mtime moved — drop the stale GL texture before reloading.
            if let Some(old) = cached.texture {
                unsafe { gl.delete_texture(old) };
            }
        }

        let texture = match unsafe { try_load_texture(gl, path, srgb) } {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("viewer: texture load failed for {}: {e}", path.display());
                None
            }
        };
        self.texture_cache
            .insert(key, CachedTexture { mtime, texture });
        texture
    }

    /// Upload the matrix palette for `palette_id` into `u_joint_mats`. The
    /// shader applies it unconditionally (rigid batches just have weights
    /// `[1,0,0,0]` and a one-bone palette per node), so there is no
    /// "skinning off" branch to worry about — every batch has a palette.
    fn bind_palette(&self, gl: &glow::Context, palette_id: u32) {
        let Some(palette) = self.palettes.get(palette_id as usize) else {
            return;
        };
        let Some(loc) = &self.u_joint_mats else { return };
        let n = palette.joint_matrices.len().min(MAX_JOINTS);
        if n == 0 {
            return;
        }
        let mut flat = Vec::with_capacity(n * 16);
        for m in &palette.joint_matrices[..n] {
            flat.extend_from_slice(&m.to_cols_array());
        }
        unsafe {
            gl.uniform_matrix_4_f32_slice(Some(loc), false, &flat);
        }
    }

    /// Drop GL textures whose source paths are no longer referenced by any
    /// active batch. Keeps memory usage bounded across scene reloads where
    /// the user has swapped texture files.
    pub fn evict_unused_textures(&mut self, gl: &glow::Context) {
        use std::collections::HashSet;
        let mut alive: HashSet<&PathBuf> = HashSet::new();
        for b in &self.batches {
            for slot in [
                &b.base_color_texture,
                &b.metallic_roughness_texture,
                &b.normal_texture,
                &b.occlusion_texture,
                &b.emissive_texture,
            ] {
                if let Some(p) = slot {
                    alive.insert(p);
                }
            }
        }
        let stale: Vec<(PathBuf, bool)> = self
            .texture_cache
            .keys()
            .filter(|(p, _)| !alive.contains(p))
            .cloned()
            .collect();
        for k in stale {
            if let Some(entry) = self.texture_cache.remove(&k) {
                if let Some(tex) = entry.texture {
                    unsafe { gl.delete_texture(tex) };
                }
            }
        }
    }

    pub fn draw(&mut self, gl: &glow::Context, viewproj: Mat4, camera_pos: Vec3) {
        unsafe {
            // egui_glow only clears color each frame, not depth. Without this,
            // stale depth values from previous frames cause z-fighting artifacts
            // when the camera moves. Scissor is disabled so the clear is bounded
            // to the viewport that egui_glow set for us.
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_depth_f32(1.0);
            gl.clear(glow::DEPTH_BUFFER_BIT);

            if self.batches.is_empty() {
                return;
            }

            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.enable(glow::CULL_FACE);
            gl.cull_face(glow::BACK);
            gl.front_face(glow::CCW);
            // sRGB framebuffer write so the gamma curve we apply in the FS
            // lands on a perceptually correct backbuffer.
            gl.enable(glow::FRAMEBUFFER_SRGB);
            // Wireframe preview: rasterize triangles as lines and drop culling
            // so back-facing edges stay visible. Reverts to FILL + CULL at the
            // end of the function.
            if self.wireframe {
                gl.polygon_mode(glow::FRONT_AND_BACK, glow::LINE);
                gl.disable(glow::CULL_FACE);
            }

            gl.use_program(Some(self.program));
            if let Some(loc) = &self.u_shader_mode {
                gl.uniform_1_i32(Some(loc), self.shader_mode);
            }
            if let Some(loc) = &self.u_viewproj {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &viewproj.to_cols_array());
            }
            if let Some(loc) = &self.u_camera_pos {
                gl.uniform_3_f32(Some(loc), camera_pos.x, camera_pos.y, camera_pos.z);
            }
            if let Some(loc) = &self.u_key_dir {
                // Key from upper-front-left: lights the top-right-back of the model.
                let d = Vec3::new(-0.4, -1.0, -0.3).normalize();
                gl.uniform_3_f32(Some(loc), d.x, d.y, d.z);
            }
            if let Some(loc) = &self.u_fill_dir {
                // Weaker fill from the opposite side to soften the shadow side.
                let d = Vec3::new(0.6, -0.2, 0.5).normalize();
                gl.uniform_3_f32(Some(loc), d.x, d.y, d.z);
            }
            // Sky probe colours. Soft daylight: blue zenith, warm horizon,
            // muted green-grey ground bounce. The same values feed both diffuse
            // ambient and specular reflection in the shader. Scaled down from
            // raw sky radiance because the shader samples the dome directly
            // instead of integrating a proper diffuse irradiance probe — full
            // radiance overstated ambient on flat-colour (untextured) surfaces.
            if let Some(loc) = &self.u_sky_top {
                gl.uniform_3_f32(Some(loc), 0.33, 0.42, 0.57);
            }
            if let Some(loc) = &self.u_sky_horizon {
                gl.uniform_3_f32(Some(loc), 0.51, 0.51, 0.49);
            }
            if let Some(loc) = &self.u_sky_ground {
                gl.uniform_3_f32(Some(loc), 0.11, 0.10, 0.09);
            }
            if let Some(loc) = &self.u_sun_dir {
                // Sun direction (pointing FROM sun, like a directional light).
                let d = Vec3::new(-0.4, -1.0, -0.3).normalize();
                gl.uniform_3_f32(Some(loc), d.x, d.y, d.z);
            }
            if let Some(loc) = &self.u_sun_color {
                gl.uniform_3_f32(Some(loc), 0.66, 0.63, 0.57);
            }
            // Sampler bindings — texture units stay constant for the whole pass.
            if let Some(loc) = &self.u_base_tex {
                gl.uniform_1_i32(Some(loc), 0);
            }
            if let Some(loc) = &self.u_mr_tex {
                gl.uniform_1_i32(Some(loc), 1);
            }
            if let Some(loc) = &self.u_normal_tex {
                gl.uniform_1_i32(Some(loc), 2);
            }
            if let Some(loc) = &self.u_ao_tex {
                gl.uniform_1_i32(Some(loc), 3);
            }
            if let Some(loc) = &self.u_emissive_tex {
                gl.uniform_1_i32(Some(loc), 4);
            }
            gl.bind_vertex_array(Some(self.vao));
        }

        // Take a copy so we can call &mut self methods (ensure_texture) inside
        // the loop without aliasing self.batches.
        let batches = self.batches.clone();
        // Split into opaque/mask (any draw order, depth writes on) and blend
        // (drawn last, depth-sorted back-to-front, depth writes off). Per-
        // batch sorting on every frame is fine for preview-sized scenes —
        // we're talking dozens of batches, not thousands.
        let mut opaque_indices: Vec<usize> = Vec::with_capacity(batches.len());
        let mut blend_indices: Vec<usize> = Vec::new();
        for (i, b) in batches.iter().enumerate() {
            // Anything authored with `transmission > 0` is treated as a blend
            // batch even if its alpha_mode is Opaque: without a real
            // screen-space refraction pass, the FS will have killed the
            // diffuse term, and rendering the result as opaque (black body
            // with floating specular) reads worse than a depth-sorted blend
            // that lets the actual background show through.
            let is_blend = matches!(b.alpha_mode, AlphaMode::Blend) || b.transmission > 0.0;
            if is_blend {
                blend_indices.push(i);
            } else {
                opaque_indices.push(i);
            }
        }
        blend_indices.sort_by(|&a, &b| {
            let da = (batches[a].centroid - camera_pos).length_squared();
            let db = (batches[b].centroid - camera_pos).length_squared();
            db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
        });
        let order: Vec<usize> = opaque_indices.into_iter().chain(blend_indices).collect();

        // Tracks which palette is currently on the GPU, so consecutive batches
        // sharing one (e.g. an opaque chain that all live in the same skin)
        // avoid a redundant reupload of the matrix array.
        let mut current_palette: Option<u32> = None;
        // Per-batch GL state we shadow to avoid redundant calls when many
        // adjacent batches share the same alpha/cull configuration.
        let mut current_blend = false;
        let mut current_depth_write = true;
        let mut current_cull = true;
        for &idx in &order {
            let b = &batches[idx];
            if current_palette != Some(b.palette_id) {
                self.bind_palette(gl, b.palette_id);
                current_palette = Some(b.palette_id);
            }

            // glTF: `Blend` materials use premultiplied-alpha over the
            // framebuffer with depth writes off so transparents behind
            // transparents still pass the depth test against opaques. `Mask`
            // and `Opaque` keep the default (depth-write on, no blending) —
            // `Mask` discards in the FS instead, which is friendlier to
            // early-Z than blending would be. The fragment shader emits
            // premultiplied colour (`absorbed * alpha + reflected`, see
            // `shaders.rs`) so surface reflections land on the framebuffer
            // at full strength even when the surface is mostly see-through.
            let want_blend = matches!(b.alpha_mode, AlphaMode::Blend) || b.transmission > 0.0;
            let want_depth_write = !want_blend;
            let want_cull = !b.double_sided;
            unsafe {
                if want_blend != current_blend {
                    if want_blend {
                        gl.enable(glow::BLEND);
                        gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                    } else {
                        gl.disable(glow::BLEND);
                    }
                    current_blend = want_blend;
                }
                if want_depth_write != current_depth_write {
                    gl.depth_mask(want_depth_write);
                    current_depth_write = want_depth_write;
                }
                if want_cull != current_cull {
                    if want_cull {
                        gl.enable(glow::CULL_FACE);
                    } else {
                        gl.disable(glow::CULL_FACE);
                    }
                    current_cull = want_cull;
                }
            }

            // Upload material scalars and bind whatever subset of the texture
            // slots this batch declares. Texture units that aren't used keep
            // whatever was previously bound — the FS guards reads with the
            // matching `u_use_*_tex` flag, so undefined samples can't sneak
            // into the result.
            let base_tex = b
                .base_color_texture
                .as_ref()
                .and_then(|p| self.ensure_texture(gl, p, true));
            let mr_tex = b
                .metallic_roughness_texture
                .as_ref()
                .and_then(|p| self.ensure_texture(gl, p, false));
            let normal_tex = b
                .normal_texture
                .as_ref()
                .and_then(|p| self.ensure_texture(gl, p, false));
            let ao_tex = b
                .occlusion_texture
                .as_ref()
                .and_then(|p| self.ensure_texture(gl, p, false));
            let emissive_tex = b
                .emissive_texture
                .as_ref()
                .and_then(|p| self.ensure_texture(gl, p, true));

            unsafe {
                if let Some(loc) = &self.u_base_color {
                    gl.uniform_3_f32(
                        Some(loc),
                        b.base_color[0],
                        b.base_color[1],
                        b.base_color[2],
                    );
                }
                if let Some(loc) = &self.u_base_color_alpha {
                    gl.uniform_1_f32(Some(loc), b.base_color_alpha);
                }
                if let Some(loc) = &self.u_alpha_mode {
                    let mode = match b.alpha_mode {
                        AlphaMode::Opaque => 0,
                        AlphaMode::Mask => 1,
                        AlphaMode::Blend => 2,
                    };
                    gl.uniform_1_i32(Some(loc), mode);
                }
                if let Some(loc) = &self.u_alpha_cutoff {
                    gl.uniform_1_f32(Some(loc), b.alpha_cutoff);
                }
                if let Some(loc) = &self.u_metallic {
                    gl.uniform_1_f32(Some(loc), b.metallic);
                }
                if let Some(loc) = &self.u_roughness {
                    gl.uniform_1_f32(Some(loc), b.roughness);
                }
                if let Some(loc) = &self.u_emissive {
                    gl.uniform_3_f32(Some(loc), b.emissive[0], b.emissive[1], b.emissive[2]);
                }
                if let Some(loc) = &self.u_emissive_strength {
                    gl.uniform_1_f32(Some(loc), b.emissive_strength);
                }
                if let Some(loc) = &self.u_transmission {
                    gl.uniform_1_f32(Some(loc), b.transmission);
                }
                if let Some(loc) = &self.u_use_base_tex {
                    gl.uniform_1_i32(Some(loc), base_tex.is_some() as i32);
                }
                if let Some(loc) = &self.u_use_mr_tex {
                    gl.uniform_1_i32(Some(loc), mr_tex.is_some() as i32);
                }
                if let Some(loc) = &self.u_use_normal_tex {
                    gl.uniform_1_i32(Some(loc), normal_tex.is_some() as i32);
                }
                if let Some(loc) = &self.u_use_ao_tex {
                    gl.uniform_1_i32(Some(loc), ao_tex.is_some() as i32);
                }
                if let Some(loc) = &self.u_use_emissive_tex {
                    gl.uniform_1_i32(Some(loc), emissive_tex.is_some() as i32);
                }
                for (unit, tex) in [base_tex, mr_tex, normal_tex, ao_tex, emissive_tex]
                    .iter()
                    .enumerate()
                {
                    if let Some(t) = tex {
                        gl.active_texture(glow::TEXTURE0 + unit as u32);
                        gl.bind_texture(glow::TEXTURE_2D, Some(*t));
                    }
                }
                let byte_offset = (b.index_start as i32) * std::mem::size_of::<u32>() as i32;
                gl.draw_elements(
                    glow::TRIANGLES,
                    b.index_count as i32,
                    glow::UNSIGNED_INT,
                    byte_offset,
                );
            }
        }

        unsafe {
            for unit in 0..5 {
                gl.active_texture(glow::TEXTURE0 + unit as u32);
                gl.bind_texture(glow::TEXTURE_2D, None);
            }
            gl.bind_vertex_array(None);
            gl.use_program(None);
            // Restore the GL state egui expects on entry — anything we
            // toggled per-batch must be back to the renderer-default before
            // the egui pass paints over us.
            gl.disable(glow::BLEND);
            gl.depth_mask(true);
            gl.enable(glow::CULL_FACE);
            gl.disable(glow::FRAMEBUFFER_SRGB);
            if self.wireframe {
                gl.polygon_mode(glow::FRONT_AND_BACK, glow::FILL);
            }
        }
    }

    /// Render the current scene at `size × size` into a fresh offscreen
    /// framebuffer and read back the pixels as 8-bit RGBA. Used by the
    /// thumbnail / video capture path — independent of the visible viewport
    /// so the output size stays fixed regardless of the user's window
    /// dimensions. The FBO + attachments are minted and destroyed per call;
    /// at typical capture cadences (one click per minute) the cost of GL
    /// object churn is invisible. Caller is responsible for restoring any
    /// FBO it had bound before invoking this — we leave framebuffer 0 active
    /// on return so the egui paint loop continues painting to the screen.
    ///
    /// Renders into a 4× MSAA color+depth renderbuffer pair, then resolves
    /// to a single-sample texture via `blit_framebuffer` so the read-back
    /// pixels are antialiased — matches the 4× MSAA the on-screen eframe
    /// surface uses. The grid is intentionally never drawn here; capture
    /// output is a clean view of the model.
    pub fn render_to_pixels(
        &mut self,
        gl: &glow::Context,
        size: u32,
        viewproj: Mat4,
        eye: Vec3,
        bg: [u8; 3],
    ) -> anyhow::Result<Vec<u8>> {
        // Save what we need to restore. Viewport is the only state egui_glow
        // sets per-callback that our offscreen pass clobbers; the scissor
        // box, framebuffer binding, etc. are handled explicitly below.
        let mut prev_viewport = [0i32; 4];
        unsafe { gl.get_parameter_i32_slice(glow::VIEWPORT, &mut prev_viewport) };
        let prev_fbo = unsafe { gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) };

        let w = size as i32;
        let h = size as i32;
        // Cap to the driver's supported sample count so we don't request a
        // mode the GL refuses to allocate (some drivers max out below 4).
        let max_samples = unsafe { gl.get_parameter_i32(glow::MAX_SAMPLES) };
        let samples = max_samples.min(4).max(1);
        let result = unsafe {
            // Multisample color renderbuffer in sRGB so the renderer's
            // existing FRAMEBUFFER_SRGB enable produces gamma-correct
            // resolved pixels.
            let ms_color_rb = gl
                .create_renderbuffer()
                .map_err(|e| anyhow::anyhow!("offscreen ms color rb: {e}"))?;
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(ms_color_rb));
            gl.renderbuffer_storage_multisample(
                glow::RENDERBUFFER,
                samples,
                glow::SRGB8_ALPHA8,
                w,
                h,
            );

            let ms_depth_rb = gl
                .create_renderbuffer()
                .map_err(|e| anyhow::anyhow!("offscreen ms depth rb: {e}"))?;
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(ms_depth_rb));
            gl.renderbuffer_storage_multisample(
                glow::RENDERBUFFER,
                samples,
                glow::DEPTH_COMPONENT24,
                w,
                h,
            );
            gl.bind_renderbuffer(glow::RENDERBUFFER, None);

            let ms_fbo = gl
                .create_framebuffer()
                .map_err(|e| anyhow::anyhow!("offscreen ms fbo: {e}"))?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(ms_fbo));
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::RENDERBUFFER,
                Some(ms_color_rb),
            );
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::DEPTH_ATTACHMENT,
                glow::RENDERBUFFER,
                Some(ms_depth_rb),
            );
            let ms_status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if ms_status != glow::FRAMEBUFFER_COMPLETE {
                gl.delete_framebuffer(ms_fbo);
                gl.delete_renderbuffer(ms_color_rb);
                gl.delete_renderbuffer(ms_depth_rb);
                return Err(anyhow::anyhow!(
                    "offscreen ms framebuffer incomplete (status=0x{ms_status:x})"
                ));
            }

            // Single-sample resolve target the MSAA buffer blits into. Read
            // back happens from this FBO so the pixels we hand to PNG / mp4
            // encoding are already resolved.
            let resolve_tex = gl
                .create_texture()
                .map_err(|e| anyhow::anyhow!("offscreen resolve tex: {e}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(resolve_tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::SRGB8_ALPHA8 as i32,
                w,
                h,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                None,
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

            let resolve_fbo = gl
                .create_framebuffer()
                .map_err(|e| anyhow::anyhow!("offscreen resolve fbo: {e}"))?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(resolve_fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(resolve_tex),
                0,
            );
            let res_status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if res_status != glow::FRAMEBUFFER_COMPLETE {
                gl.delete_framebuffer(ms_fbo);
                gl.delete_renderbuffer(ms_color_rb);
                gl.delete_renderbuffer(ms_depth_rb);
                gl.delete_framebuffer(resolve_fbo);
                gl.delete_texture(resolve_tex);
                return Err(anyhow::anyhow!(
                    "offscreen resolve framebuffer incomplete (status=0x{res_status:x})"
                ));
            }

            // Bind the MSAA target for the draw pass.
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(ms_fbo));
            gl.viewport(0, 0, w, h);
            gl.disable(glow::SCISSOR_TEST);
            gl.enable(glow::MULTISAMPLE);
            // Background fill. With FRAMEBUFFER_SRGB disabled, glClearColor
            // values are written directly into the framebuffer with no
            // sRGB conversion — so passing `byte / 255.0` lands the same
            // sRGB-encoded byte we pulled from the user's settings into the
            // on-disk PNG. (The renderer's main pass re-enables
            // FRAMEBUFFER_SRGB itself before drawing geometry.)
            gl.disable(glow::FRAMEBUFFER_SRGB);
            gl.clear_color(
                bg[0] as f32 / 255.0,
                bg[1] as f32 / 255.0,
                bg[2] as f32 / 255.0,
                1.0,
            );
            gl.clear_depth_f32(1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);

            self.draw(gl, viewproj, eye);

            // Resolve MSAA → single-sample so read_pixels produces an
            // antialiased image.
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(ms_fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(resolve_fbo));
            gl.blit_framebuffer(
                0,
                0,
                w,
                h,
                0,
                0,
                w,
                h,
                glow::COLOR_BUFFER_BIT,
                glow::LINEAR,
            );

            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(resolve_fbo));
            let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
            gl.read_pixels(
                0,
                0,
                w,
                h,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(&mut pixels),
            );

            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_framebuffer(ms_fbo);
            gl.delete_renderbuffer(ms_color_rb);
            gl.delete_renderbuffer(ms_depth_rb);
            gl.delete_framebuffer(resolve_fbo);
            gl.delete_texture(resolve_tex);

            // OpenGL's origin is bottom-left, PNG / image crate's is top-left.
            // Flip rows so the saved image lands right-side-up.
            let stride = (w as usize) * 4;
            let mut flipped = vec![0u8; pixels.len()];
            for row in 0..h as usize {
                let src = row * stride;
                let dst = ((h as usize) - 1 - row) * stride;
                flipped[dst..dst + stride].copy_from_slice(&pixels[src..src + stride]);
            }
            Ok(flipped)
        };

        // Restore the bound FBO + viewport so egui_glow continues painting
        // to whatever surface it had set up before our paint callback ran.
        unsafe {
            gl.bind_framebuffer(
                glow::DRAW_FRAMEBUFFER,
                if prev_fbo == 0 {
                    None
                } else {
                    Some(glow::NativeFramebuffer(
                        std::num::NonZeroU32::new(prev_fbo as u32)
                            .expect("non-zero prev FBO"),
                    ))
                },
            );
            gl.viewport(
                prev_viewport[0],
                prev_viewport[1],
                prev_viewport[2],
                prev_viewport[3],
            );
        }
        result
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
            gl.delete_buffer(self.ebo);
            for entry in self.texture_cache.values() {
                if let Some(t) = entry.texture {
                    gl.delete_texture(t);
                }
            }
        }
        self.gizmo.destroy(gl);
        self.grid.destroy(gl);
    }

    /// Draw the ground reference grid. Meant to be called immediately after
    /// the depth clear, before the main scene pass, so the scene's depth
    /// test naturally occludes lines behind solid geometry.
    pub fn draw_grid(&self, gl: &glow::Context, viewproj: Mat4, camera_pos: Vec3) {
        self.grid.draw(gl, viewproj, camera_pos);
    }

    /// Draw the gizmo overlay. Called from the paint callback after the
    /// scene pass finishes.
    pub fn draw_gizmo(
        &self,
        gl: &glow::Context,
        viewproj: Mat4,
        origin: Vec3,
        scale: f32,
        mode: crate::GizmoMode,
    ) {
        self.gizmo.draw(gl, viewproj, origin, scale, mode);
    }
}
