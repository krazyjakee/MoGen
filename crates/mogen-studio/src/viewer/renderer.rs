use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use glam::{Mat4, Vec3, Vec4};
use glow::HasContext;
use mogen_core::AlphaMode;

use super::environment::{Environment, EnvironmentParams};
use super::flatten::{DrawBatch, FlatMesh, SkinPalette, FLOATS_PER_VERTEX, MAX_JOINTS};
use super::gizmo_gl::GizmoGl;
use super::gl_util::{bytes_of_f32, bytes_of_u32, compile_program, try_load_texture};
use super::grid_gl::GridGl;
use super::lights::{kind_to_int, ResolvedLight, MAX_LIGHTS};
use super::lights_gl::LightsGl;
use super::shaders::{FS_SRC, VS_SRC};

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
    u_num_lights: Option<glow::UniformLocation>,
    u_light_kind: Option<glow::UniformLocation>,
    u_light_pos: Option<glow::UniformLocation>,
    u_light_dir: Option<glow::UniformLocation>,
    u_light_color: Option<glow::UniformLocation>,
    u_light_range: Option<glow::UniformLocation>,
    u_light_cone: Option<glow::UniformLocation>,
    /// Latest resolved punctual lights uploaded to the shader. Refreshed by
    /// [`Self::set_lights`] each paint; empty falls back to the analytic
    /// key/fill rig hard-coded in the fragment shader.
    lights: Vec<ResolvedLight>,
    /// Active environment-lighting preset's resolved parameters. Pushed into
    /// the sky-probe + key/fill uniforms each `draw`. Updated via
    /// [`Self::set_environment`]; defaults to the historical Studio preset
    /// so a fresh renderer with no UI wiring still produces the original
    /// hardcoded look.
    environment: EnvironmentParams,
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
    /// Wireframe overlay for `light` nodes (icon ring + direction arrow /
    /// cone). Always drawn after the main scene; depth-tests against geometry
    /// so a light tucked inside a wall reads as occluded.
    pub(super) lights_overlay: LightsGl,
    /// Per-batch resolved texture handles, parallel to [`Self::batches`].
    /// Refreshed on the first draw of each frame and reused across the
    /// per-batch loop so the loop can iterate `&self.batches` without the
    /// `&mut self` reborrow that would otherwise force a per-frame clone of
    /// the batches vec. Slot order matches sampler units 0..=4 (base, mr,
    /// normal, ao, emissive).
    draw_textures: Vec<[Option<glow::Texture>; 5]>,
    /// Reusable scratch vec — opaque batch indices accumulated each draw.
    /// Parked on the renderer so the per-frame draw loop doesn't allocate.
    opaque_indices: Vec<usize>,
    /// Reusable scratch vec — blend batch indices accumulated each draw,
    /// then sorted back-to-front before being merged into `draw_order`.
    blend_indices: Vec<usize>,
    /// Reusable scratch vec — final draw order (opaque then sorted blend).
    /// Iterated by the per-batch loop.
    draw_order: Vec<usize>,
    /// Reusable scratch — flattened palette `mat4` columns uploaded to
    /// `u_joint_mats`. Cleared between batches; capacity grows to fit the
    /// largest palette and stays parked across frames.
    palette_scratch: Vec<f32>,
}

/// Wraps a batch's `material_id` so the per-batch loop can compare materials
/// with `PartialEq` without dragging the whole `DrawBatch` into the
/// comparison. Two batches with the same key share PBR uniforms + texture
/// bindings, so the loop can skip those uploads on every batch after the
/// first in a chain.
#[derive(Copy, Clone, PartialEq, Eq)]
struct MaterialKey(Option<u32>);

/// Six view-frustum planes extracted from a `view_proj` matrix using the
/// Gribb–Hartmann method. Stored as `Vec4` `(a, b, c, d)` with each plane
/// normalised so the signed distance from a point `p` to the plane is
/// `dot(plane.xyz, p) + plane.w`. Inside the frustum is the positive half-
/// space; a sphere whose centre lies more than `-radius` away from any plane
/// is fully outside and can be culled.
struct FrustumPlanes([Vec4; 6]);

impl FrustumPlanes {
    fn from_view_proj(vp: Mat4) -> Self {
        // Glam exposes rows directly; combining row3 ± row{0,1,2} produces
        // the six plane equations in clip-space derived form. Normalising
        // by the xyz length lets the distance test below work in world
        // units even though the planes were derived in clip space.
        let r0 = vp.row(0);
        let r1 = vp.row(1);
        let r2 = vp.row(2);
        let r3 = vp.row(3);
        let raw = [
            r3 + r0, // left
            r3 - r0, // right
            r3 + r1, // bottom
            r3 - r1, // top
            r3 + r2, // near
            r3 - r2, // far
        ];
        let mut out = [Vec4::ZERO; 6];
        for (i, p) in raw.iter().enumerate() {
            let n = p.truncate();
            let len = n.length();
            out[i] = if len > 0.0 { *p / len } else { *p };
        }
        FrustumPlanes(out)
    }

    /// Conservative sphere-vs-frustum test. Returns false only when the
    /// sphere is fully outside at least one plane — partial overlap and
    /// fully-inside both return true so the renderer never drops a batch
    /// that should still rasterise.
    fn sphere_visible(&self, centre: Vec3, radius: f32) -> bool {
        for plane in &self.0 {
            let d = plane.x * centre.x + plane.y * centre.y + plane.z * centre.z + plane.w;
            if d < -radius {
                return false;
            }
        }
        true
    }
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
            let u_num_lights = u("u_num_lights");
            let u_light_kind = u("u_light_kind[0]");
            let u_light_pos = u("u_light_pos[0]");
            let u_light_dir = u("u_light_dir[0]");
            let u_light_color = u("u_light_color[0]");
            let u_light_range = u("u_light_range[0]");
            let u_light_cone = u("u_light_cone[0]");

            let gizmo = GizmoGl::new(gl)?;
            let grid = GridGl::new(gl)?;
            let lights_overlay = LightsGl::new(gl)?;

            Ok(Self {
                program,
                vao,
                vbo,
                ebo,
                gizmo,
                grid,
                lights_overlay,
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
                u_num_lights,
                u_light_kind,
                u_light_pos,
                u_light_dir,
                u_light_color,
                u_light_range,
                u_light_cone,
                lights: Vec::new(),
                environment: Environment::default().params(),
                shader_mode: 0,
                wireframe: false,
                texture_cache: HashMap::new(),
                vbo_bytes: 0,
                ebo_bytes: 0,
                batches: Vec::new(),
                palettes: Vec::new(),
                draw_textures: Vec::new(),
                opaque_indices: Vec::new(),
                blend_indices: Vec::new(),
                draw_order: Vec::new(),
                palette_scratch: Vec::new(),
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

    /// Hand the renderer the latest resolved punctual lights for the active
    /// scene. The slice is cached locally and uploaded as part of the next
    /// `draw` call, so a per-frame call from the paint callback is cheap —
    /// just a Vec clone of up to MAX_LIGHTS entries. Pass an empty slice to
    /// fall back to the shader's hard-coded key/fill rig.
    pub fn set_lights(&mut self, lights: &[ResolvedLight]) {
        let n = lights.len().min(MAX_LIGHTS);
        self.lights.clear();
        self.lights.extend_from_slice(&lights[..n]);
    }

    /// Swap in a fresh environment-lighting preset. Cheap — just stores the
    /// params; the next `draw` uploads them.
    pub fn set_environment(&mut self, params: EnvironmentParams) {
        self.environment = params;
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

    /// Push the cached light list to the active shader program. Pads the
    /// trailing slots with zeroes so the shader's `i < u_num_lights` guard
    /// is the only thing protecting us from reading garbage — uniforms left
    /// from a previous draw with a longer light list would otherwise
    /// re-enter the loop the next time the count grew. Caller must have
    /// already bound `self.program`.
    fn upload_lights(&self, gl: &glow::Context) {
        let mut kinds = [0i32; MAX_LIGHTS];
        let mut pos = [0.0f32; MAX_LIGHTS * 3];
        let mut dir = [0.0f32; MAX_LIGHTS * 3];
        let mut color = [0.0f32; MAX_LIGHTS * 3];
        let mut range = [0.0f32; MAX_LIGHTS];
        let mut cone = [0.0f32; MAX_LIGHTS * 2];
        for (i, l) in self.lights.iter().enumerate() {
            kinds[i] = kind_to_int(l.kind);
            pos[i * 3] = l.position.x;
            pos[i * 3 + 1] = l.position.y;
            pos[i * 3 + 2] = l.position.z;
            dir[i * 3] = l.direction.x;
            dir[i * 3 + 1] = l.direction.y;
            dir[i * 3 + 2] = l.direction.z;
            color[i * 3] = l.color[0];
            color[i * 3 + 1] = l.color[1];
            color[i * 3 + 2] = l.color[2];
            range[i] = l.range;
            cone[i * 2] = l.inner_cos;
            cone[i * 2 + 1] = l.outer_cos;
        }
        unsafe {
            if let Some(loc) = &self.u_num_lights {
                gl.uniform_1_i32(Some(loc), self.lights.len() as i32);
            }
            if let Some(loc) = &self.u_light_kind {
                gl.uniform_1_i32_slice(Some(loc), &kinds);
            }
            if let Some(loc) = &self.u_light_pos {
                gl.uniform_3_f32_slice(Some(loc), &pos);
            }
            if let Some(loc) = &self.u_light_dir {
                gl.uniform_3_f32_slice(Some(loc), &dir);
            }
            if let Some(loc) = &self.u_light_color {
                gl.uniform_3_f32_slice(Some(loc), &color);
            }
            if let Some(loc) = &self.u_light_range {
                gl.uniform_1_f32_slice(Some(loc), &range);
            }
            if let Some(loc) = &self.u_light_cone {
                gl.uniform_2_f32_slice(Some(loc), &cone);
            }
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
    /// Uses [`Self::palette_scratch`] as the flatten buffer so consecutive
    /// frames don't reallocate.
    fn bind_palette(&mut self, gl: &glow::Context, palette_id: u32) {
        let Some(palette) = self.palettes.get(palette_id as usize) else {
            return;
        };
        let Some(loc) = self.u_joint_mats.clone() else {
            return;
        };
        let n = palette.joint_matrices.len().min(MAX_JOINTS);
        if n == 0 {
            return;
        }
        self.palette_scratch.clear();
        self.palette_scratch.reserve(n * 16);
        for m in &palette.joint_matrices[..n] {
            self.palette_scratch.extend_from_slice(&m.to_cols_array());
        }
        unsafe {
            gl.uniform_matrix_4_f32_slice(Some(&loc), false, &self.palette_scratch);
        }
    }

    /// Drop GL textures whose source paths are no longer referenced by any
    /// active batch. Keeps memory usage bounded across scene reloads where
    /// the user has swapped texture files.
    pub(super) fn evict_unused_textures(&mut self, gl: &glow::Context) {
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
            // Sky probe + analytic key/fill rig come from the active
            // [`Environment`] preset (see `viewer/environment.rs`). Direction
            // vectors are normalised on upload so the table can store the
            // pre-normalised values verbatim. The same values feed both
            // diffuse ambient and specular reflection in the shader; raw sky
            // radiance is scaled down because the shader samples the dome
            // directly instead of integrating a proper diffuse irradiance
            // probe — full radiance overstated ambient on flat-colour
            // (untextured) surfaces.
            let env = self.environment;
            if let Some(loc) = &self.u_key_dir {
                let d = env.key_dir.normalize_or_zero();
                gl.uniform_3_f32(Some(loc), d.x, d.y, d.z);
            }
            if let Some(loc) = &self.u_fill_dir {
                let d = env.fill_dir.normalize_or_zero();
                gl.uniform_3_f32(Some(loc), d.x, d.y, d.z);
            }
            if let Some(loc) = &self.u_sky_top {
                gl.uniform_3_f32(Some(loc), env.sky_top.x, env.sky_top.y, env.sky_top.z);
            }
            if let Some(loc) = &self.u_sky_horizon {
                gl.uniform_3_f32(
                    Some(loc),
                    env.sky_horizon.x,
                    env.sky_horizon.y,
                    env.sky_horizon.z,
                );
            }
            if let Some(loc) = &self.u_sky_ground {
                gl.uniform_3_f32(
                    Some(loc),
                    env.sky_ground.x,
                    env.sky_ground.y,
                    env.sky_ground.z,
                );
            }
            if let Some(loc) = &self.u_sun_dir {
                let d = env.sun_dir.normalize_or_zero();
                gl.uniform_3_f32(Some(loc), d.x, d.y, d.z);
            }
            if let Some(loc) = &self.u_sun_color {
                gl.uniform_3_f32(Some(loc), env.sun_color.x, env.sun_color.y, env.sun_color.z);
            }
            // Punctual lights uploaded as parallel arrays. Sized to MAX_LIGHTS
            // on both sides (shader + Rust); `u_num_lights` caps the inner
            // loop so unused entries don't contribute. When the count is 0
            // the FS falls back to the analytic key/fill rig defined inline
            // there — important for blank scenes that haven't authored any
            // `light` nodes yet.
            self.upload_lights(gl);
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

        // Phase 1 — prepare. Resolve every batch's textures into a parallel
        // `draw_textures` vec so the main draw loop can iterate
        // `&self.batches` without the `&mut self` alias the inline
        // `ensure_texture` call used to require (the previous version cloned
        // the entire batches vec each frame to dodge that). Textures are
        // cached by mtime, so the resolution pass is a series of HashMap
        // lookups and almost free in steady state.
        self.draw_textures.clear();
        self.draw_textures.reserve(self.batches.len());
        for i in 0..self.batches.len() {
            let base = self.batches[i]
                .base_color_texture
                .clone()
                .and_then(|p| self.ensure_texture(gl, &p, true));
            let mr = self.batches[i]
                .metallic_roughness_texture
                .clone()
                .and_then(|p| self.ensure_texture(gl, &p, false));
            let normal = self.batches[i]
                .normal_texture
                .clone()
                .and_then(|p| self.ensure_texture(gl, &p, false));
            let ao = self.batches[i]
                .occlusion_texture
                .clone()
                .and_then(|p| self.ensure_texture(gl, &p, false));
            let emissive = self.batches[i]
                .emissive_texture
                .clone()
                .and_then(|p| self.ensure_texture(gl, &p, true));
            self.draw_textures.push([base, mr, normal, ao, emissive]);
        }

        // Phase 2 — partition + frustum cull. Split into opaque/mask (any
        // draw order, depth writes on) and blend (drawn last, depth-sorted
        // back-to-front, depth writes off), and drop batches whose
        // bounding sphere lies fully outside the view frustum so they pay
        // neither draw call nor fragment cost.
        let frustum = FrustumPlanes::from_view_proj(viewproj);
        self.opaque_indices.clear();
        self.blend_indices.clear();
        for (i, b) in self.batches.iter().enumerate() {
            if !frustum.sphere_visible(b.centroid, b.radius) {
                continue;
            }
            // Anything authored with `transmission > 0` is treated as a
            // blend batch even if its alpha_mode is Opaque: without a real
            // screen-space refraction pass, the FS will have killed the
            // diffuse term, and rendering the result as opaque (black body
            // with floating specular) reads worse than a depth-sorted
            // blend that lets the actual background show through.
            let is_blend = matches!(b.alpha_mode, AlphaMode::Blend) || b.transmission > 0.0;
            if is_blend {
                self.blend_indices.push(i);
            } else {
                self.opaque_indices.push(i);
            }
        }
        // Sort blend back-to-front by centroid distance so transparents
        // composite correctly. Opaque keeps its natural order — flatten
        // groups by `(skin, material)` first, so adjacent opaque batches
        // already tend to share material and the per-batch material-skip
        // optimisation below kicks in for free.
        let batches_ref = &self.batches;
        self.blend_indices.sort_by(|&a, &b| {
            let da = (batches_ref[a].centroid - camera_pos).length_squared();
            let db = (batches_ref[b].centroid - camera_pos).length_squared();
            db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
        });
        self.draw_order.clear();
        self.draw_order.extend(self.opaque_indices.iter().copied());
        self.draw_order.extend(self.blend_indices.iter().copied());

        // Phase 3 — draw. Track previously-uploaded state so adjacent
        // batches sharing a material / palette / GL toggle don't pay for a
        // redundant uniform or state change. The big win is the
        // material-equality skip: ~15 PBR uniforms + up to 5 texture
        // bindings are deferred when the next batch's `material_id`
        // matches, which is the common case for chunk-split rigid groups.
        let mut current_palette: Option<u32> = None;
        let mut current_material: Option<MaterialKey> = None;
        let mut current_blend = false;
        let mut current_depth_write = true;
        let mut current_cull = true;
        let order_len = self.draw_order.len();
        for k in 0..order_len {
            let idx = self.draw_order[k];
            // Pull every Copy field off the batch up-front so the loop body
            // doesn't hold a borrow of `self.batches` across the
            // `&mut self` `bind_palette` call.
            let b = &self.batches[idx];
            let palette_id = b.palette_id;
            let material_id = b.material_id;
            let alpha_mode = b.alpha_mode;
            let transmission = b.transmission;
            let double_sided = b.double_sided;
            let alpha_cutoff = b.alpha_cutoff;
            let base_color = b.base_color;
            let base_color_alpha = b.base_color_alpha;
            let metallic = b.metallic;
            let roughness = b.roughness;
            let emissive = b.emissive;
            let emissive_strength = b.emissive_strength;
            let index_start = b.index_start;
            let index_count = b.index_count;
            let textures = self.draw_textures[idx];

            if current_palette != Some(palette_id) {
                self.bind_palette(gl, palette_id);
                current_palette = Some(palette_id);
            }

            // glTF: `Blend` materials use premultiplied-alpha over the
            // framebuffer with depth writes off so transparents behind
            // transparents still pass the depth test against opaques.
            // `Mask` and `Opaque` keep the default (depth-write on, no
            // blending) — `Mask` discards in the FS instead, which is
            // friendlier to early-Z than blending would be. The fragment
            // shader emits premultiplied colour (`absorbed * alpha +
            // reflected`, see `shaders.rs`) so surface reflections land on
            // the framebuffer at full strength even when the surface is
            // mostly see-through.
            let want_blend = matches!(alpha_mode, AlphaMode::Blend) || transmission > 0.0;
            let want_depth_write = !want_blend;
            let want_cull = !double_sided;
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

            // Material-equality fast path. `material_id == None` is the
            // implicit-default material; treat it as its own key so two
            // mat-less batches still skip uniforms after the first.
            let material_key = MaterialKey(material_id);
            let material_changed = current_material != Some(material_key);
            if material_changed {
                unsafe {
                    if let Some(loc) = &self.u_base_color {
                        gl.uniform_3_f32(Some(loc), base_color[0], base_color[1], base_color[2]);
                    }
                    if let Some(loc) = &self.u_base_color_alpha {
                        gl.uniform_1_f32(Some(loc), base_color_alpha);
                    }
                    if let Some(loc) = &self.u_alpha_mode {
                        let mode = match alpha_mode {
                            AlphaMode::Opaque => 0,
                            AlphaMode::Mask => 1,
                            AlphaMode::Blend => 2,
                        };
                        gl.uniform_1_i32(Some(loc), mode);
                    }
                    if let Some(loc) = &self.u_alpha_cutoff {
                        gl.uniform_1_f32(Some(loc), alpha_cutoff);
                    }
                    if let Some(loc) = &self.u_metallic {
                        gl.uniform_1_f32(Some(loc), metallic);
                    }
                    if let Some(loc) = &self.u_roughness {
                        gl.uniform_1_f32(Some(loc), roughness);
                    }
                    if let Some(loc) = &self.u_emissive {
                        gl.uniform_3_f32(Some(loc), emissive[0], emissive[1], emissive[2]);
                    }
                    if let Some(loc) = &self.u_emissive_strength {
                        gl.uniform_1_f32(Some(loc), emissive_strength);
                    }
                    if let Some(loc) = &self.u_transmission {
                        gl.uniform_1_f32(Some(loc), transmission);
                    }
                    if let Some(loc) = &self.u_use_base_tex {
                        gl.uniform_1_i32(Some(loc), textures[0].is_some() as i32);
                    }
                    if let Some(loc) = &self.u_use_mr_tex {
                        gl.uniform_1_i32(Some(loc), textures[1].is_some() as i32);
                    }
                    if let Some(loc) = &self.u_use_normal_tex {
                        gl.uniform_1_i32(Some(loc), textures[2].is_some() as i32);
                    }
                    if let Some(loc) = &self.u_use_ao_tex {
                        gl.uniform_1_i32(Some(loc), textures[3].is_some() as i32);
                    }
                    if let Some(loc) = &self.u_use_emissive_tex {
                        gl.uniform_1_i32(Some(loc), textures[4].is_some() as i32);
                    }
                    for (unit, tex) in textures.iter().enumerate() {
                        if let Some(t) = tex {
                            gl.active_texture(glow::TEXTURE0 + unit as u32);
                            gl.bind_texture(glow::TEXTURE_2D, Some(*t));
                        }
                    }
                }
                current_material = Some(material_key);
            }

            unsafe {
                let byte_offset = (index_start as i32) * std::mem::size_of::<u32>() as i32;
                gl.draw_elements(
                    glow::TRIANGLES,
                    index_count as i32,
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
        self.lights_overlay.destroy(gl);
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
        mode: crate::gizmo::GizmoMode,
    ) {
        self.gizmo.draw(gl, viewproj, origin, scale, mode);
    }

    /// Draw the light-overlay pass: a small wireframe indicator (point sphere
    /// / spot cone / directional arrow) anchored at each light's world pose,
    /// plus a highlight ring around `selected` if it carries a light.
    pub fn draw_lights_overlay(
        &self,
        gl: &glow::Context,
        viewproj: Mat4,
        eye: Vec3,
        viewport_height: f32,
        selected: Option<mogen_core::NodeId>,
    ) {
        if self.lights.is_empty() {
            return;
        }
        self.lights_overlay
            .draw(gl, viewproj, eye, viewport_height, &self.lights, selected);
    }
}
