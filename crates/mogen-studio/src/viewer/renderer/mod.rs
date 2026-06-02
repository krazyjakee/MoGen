//! GL forward renderer for the live preview viewport. Owns the main PBR
//! program + its uniform locations, the shadow + light + grid + gizmo
//! sub-pipelines, and the per-frame scratch buffers used by [`draw`].
//!
//! The renderer is split across submodules:
//! - [`textures`]: GL texture cache (load-on-miss, mtime-aware reload).
//! - [`uniforms`]: light / shadow / palette uniform uploads for the main pass.
//! - [`draw`]: the main forward pass — partition, cull, sort, draw.
//! - [`capture`]: offscreen MSAA capture for thumbnails / video.

mod capture;
mod draw;
mod textures;
mod uniforms;

use glam::Vec3;
use glow::HasContext;

use super::environment::{Environment, EnvironmentParams};
use super::flatten::{DrawBatch, FlatMesh, SkinPalette};
use super::colliders_gl::{ColliderInstance, CollidersGl};
use super::gizmo_gl::GizmoGl;
use super::imposter_gl::ImposterGl;
use super::gl_util::{bytes_of_f32, bytes_of_u32, compile_program};
use super::grid_gl::GridGl;
use super::lights::{ResolvedLight, MAX_LIGHTS};
use super::lights_gl::LightsGl;
use super::shaders::{FS_SRC, VS_SRC};
use super::shadows::{ShadowFrame, ShadowQuality, ShadowSystem};

use textures::TextureCache;

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
    u_normal_scale: Option<glow::UniformLocation>,
    u_uv_scale: Option<glow::UniformLocation>,
    u_joint_mats: Option<glow::UniformLocation>,
    u_shader_mode: Option<glow::UniformLocation>,
    u_material_shader: Option<glow::UniformLocation>,
    u_time: Option<glow::UniformLocation>,
    u_num_lights: Option<glow::UniformLocation>,
    u_light_kind: Option<glow::UniformLocation>,
    u_light_pos: Option<glow::UniformLocation>,
    u_light_dir: Option<glow::UniformLocation>,
    u_light_color: Option<glow::UniformLocation>,
    u_light_range: Option<glow::UniformLocation>,
    u_light_cone: Option<glow::UniformLocation>,
    u_shadow_2d: Option<glow::UniformLocation>,
    u_shadow_cube0: Option<glow::UniformLocation>,
    u_shadow_cube1: Option<glow::UniformLocation>,
    u_shadow_2d_viewproj: Option<glow::UniformLocation>,
    u_shadow_cube_pos: Option<glow::UniformLocation>,
    u_shadow_cube_far: Option<glow::UniformLocation>,
    u_light_shadow_2d_idx: Option<glow::UniformLocation>,
    u_light_shadow_cube_idx: Option<glow::UniformLocation>,
    u_shadow_fallback_idx: Option<glow::UniformLocation>,
    u_shadow_bias_const: Option<glow::UniformLocation>,
    u_shadow_bias_slope: Option<glow::UniformLocation>,
    u_shadow_strength: Option<glow::UniformLocation>,
    u_shadow_2d_texel: Option<glow::UniformLocation>,
    u_shadow_pcf_taps: Option<glow::UniformLocation>,
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
    /// Seconds since renderer start, fed to `u_time` each draw. Drives
    /// time-varying per-material shaders (water). The viewer pushes a fresh
    /// value via [`Self::set_frame_time`] before each paint.
    frame_time: f32,
    /// Cached uploaded textures keyed by (resolved filesystem path, sRGB flag).
    /// Albedo and emissive maps load as sRGB; normal, MR, and AO load as
    /// linear, so the same PNG used in two roles would need two separate GL
    /// objects. Entries are tagged with the file's mtime at load time so that
    /// regenerating a PNG (e.g. via `Generate Textures`) invalidates the cache
    /// without an explicit refresh button. A `None` `texture` means we tried
    /// and failed to load — kept so we don't re-log the same failure every
    /// frame.
    texture_cache: TextureCache,
    vbo_bytes: usize,
    ebo_bytes: usize,
    /// Per-batch draw plan owned by the renderer so the paint callback doesn't
    /// have to re-read the mesh on every frame.
    batches: Vec<DrawBatch>,
    /// `(index_start, index_count, node)` for each collider-bearing mesh node,
    /// copied from the uploaded [`FlatMesh`]. The collider overlay redraws
    /// these index ranges in wireframe (reusing the main VBO/EBO) so trimesh
    /// and convex colliders are visible, not just AABB boxes.
    collider_runs: Vec<(u32, u32, mogen_core::NodeId)>,
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
    /// Viewport imposter billboard pipeline. Idle for the normal scene
    /// draw; used by the paint callback when `PreviewLod::Imposter` is
    /// active to replace the mesh draw with a yaw-grid billboard.
    imposter: ImposterGl,
    /// Wireframe overlay for `light` nodes (icon ring + direction arrow /
    /// cone). Always drawn after the main scene; depth-tests against geometry
    /// so a light tucked inside a wall reads as occluded.
    lights_overlay: LightsGl,
    /// Wireframe overlay for AABB colliders. Off by default; toggled from the
    /// viewport context menu. Same depth-test rules as the lights overlay.
    colliders_overlay: CollidersGl,
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
    /// Realtime shadow-mapping subsystem. Lazily allocates GPU resources
    /// when [`Self::set_shadow_quality`] picks a non-`Off` preset; owns its
    /// own depth FBOs, atlas + cubemap textures, and depth-only program.
    shadows: ShadowSystem,
    /// Active shadow-quality preset. Drives both the GPU resource sizing
    /// (via [`ShadowSystem::set_resolution`]) and the per-frame caster cap.
    shadow_quality: ShadowQuality,
    /// Static-pose AABB centre/radius forwarded by the viewer when the
    /// scene loads. Used by the shadow pre-pass to size the directional
    /// ortho frustum and the spot/point far planes — kept in the renderer
    /// so the per-frame draw call can size casters without a borrow back
    /// into the viewer state.
    scene_center: Vec3,
    scene_radius: f32,
    /// Cached shadow plan from the last frame that needed one. The depth
    /// pre-pass is the costliest path in the viewer; rebuilding the plan +
    /// re-rendering the maps when nothing scene-relevant changed is pure
    /// waste. Setters that affect the plan (lights, palettes, env, AABB,
    /// quality, mesh upload) flip [`Self::shadows_dirty`]; the next `draw`
    /// repopulates this and clears the flag.
    shadow_frame: ShadowFrame,
    /// Reusable scratch buffer for `select_casters`'s importance ranking.
    /// Lives on the renderer so the per-frame ranking loop doesn't allocate
    /// a fresh `Vec` every paint.
    shadow_ranked_scratch: Vec<(usize, f32)>,
    /// Set when the cached shadow plan or the depth-map contents are stale.
    /// Camera motion alone does not flip this — shadows are computed in
    /// world space and the maps are camera-independent. Initially `true` so
    /// the first frame populates the maps unconditionally.
    shadows_dirty: bool,
}

/// Cheap structural equality for two `ResolvedLight`s. Used to skip a
/// shadow-pass rebuild when [`Renderer::set_lights`] is called every paint
/// with a list that hasn't actually changed (the common case for a static
/// scene). `ResolvedLight` itself isn't `PartialEq` because `LightKind` lives
/// in `mogen-core` and we don't want a cross-crate derive here.
fn light_eq(a: &ResolvedLight, b: &ResolvedLight) -> bool {
    use mogen_core::LightKind;
    let kinds_eq = matches!(
        (a.kind, b.kind),
        (LightKind::Directional, LightKind::Directional)
            | (LightKind::Point, LightKind::Point)
            | (LightKind::Spot, LightKind::Spot)
    );
    kinds_eq
        && a.position == b.position
        && a.direction == b.direction
        && a.color == b.color
        && a.range == b.range
        && a.inner_cos == b.inner_cos
        && a.outer_cos == b.outer_cos
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
            let stride = (super::flatten::FLOATS_PER_VERTEX as i32) * f;
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
            let u_normal_scale = u("u_normal_scale");
            let u_uv_scale = u("u_uv_scale");
            let u_joint_mats = u("u_joint_mats[0]");
            let u_shader_mode = u("u_shader_mode");
            let u_material_shader = u("u_material_shader");
            let u_time = u("u_time");
            let u_num_lights = u("u_num_lights");
            let u_light_kind = u("u_light_kind[0]");
            let u_light_pos = u("u_light_pos[0]");
            let u_light_dir = u("u_light_dir[0]");
            let u_light_color = u("u_light_color[0]");
            let u_light_range = u("u_light_range[0]");
            let u_light_cone = u("u_light_cone[0]");
            let u_shadow_2d = u("u_shadow_2d");
            let u_shadow_cube0 = u("u_shadow_cube0");
            let u_shadow_cube1 = u("u_shadow_cube1");
            let u_shadow_2d_viewproj = u("u_shadow_2d_viewproj[0]");
            let u_shadow_cube_pos = u("u_shadow_cube_pos[0]");
            let u_shadow_cube_far = u("u_shadow_cube_far[0]");
            let u_light_shadow_2d_idx = u("u_light_shadow_2d_idx[0]");
            let u_light_shadow_cube_idx = u("u_light_shadow_cube_idx[0]");
            let u_shadow_fallback_idx = u("u_shadow_fallback_idx");
            let u_shadow_bias_const = u("u_shadow_bias_const");
            let u_shadow_bias_slope = u("u_shadow_bias_slope");
            let u_shadow_strength = u("u_shadow_strength");
            let u_shadow_2d_texel = u("u_shadow_2d_texel");
            let u_shadow_pcf_taps = u("u_shadow_pcf_taps");

            let gizmo = GizmoGl::new(gl)?;
            let grid = GridGl::new(gl)?;
            let imposter = ImposterGl::new(gl)?;
            let lights_overlay = LightsGl::new(gl)?;
            let colliders_overlay = CollidersGl::new(gl)?;
            let shadows = ShadowSystem::new(gl)?;

            Ok(Self {
                program,
                vao,
                vbo,
                ebo,
                gizmo,
                grid,
                imposter,
                lights_overlay,
                colliders_overlay,
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
                u_normal_scale,
                u_uv_scale,
                u_joint_mats,
                u_shader_mode,
                u_material_shader,
                u_time,
                u_num_lights,
                u_light_kind,
                u_light_pos,
                u_light_dir,
                u_light_color,
                u_light_range,
                u_light_cone,
                u_shadow_2d,
                u_shadow_cube0,
                u_shadow_cube1,
                u_shadow_2d_viewproj,
                u_shadow_cube_pos,
                u_shadow_cube_far,
                u_light_shadow_2d_idx,
                u_light_shadow_cube_idx,
                u_shadow_fallback_idx,
                u_shadow_bias_const,
                u_shadow_bias_slope,
                u_shadow_strength,
                u_shadow_2d_texel,
                u_shadow_pcf_taps,
                shadows,
                shadow_quality: ShadowQuality::Off,
                scene_center: Vec3::ZERO,
                scene_radius: 0.0,
                shadow_frame: ShadowFrame::new(),
                shadow_ranked_scratch: Vec::new(),
                shadows_dirty: true,
                lights: Vec::new(),
                environment: Environment::default().params(),
                shader_mode: 0,
                wireframe: false,
                frame_time: 0.0,
                texture_cache: TextureCache::new(),
                vbo_bytes: 0,
                ebo_bytes: 0,
                batches: Vec::new(),
                collider_runs: Vec::new(),
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

    /// Hand the renderer the latest monotonic frame time (seconds) for
    /// time-varying material shaders. Cheap — just stores the value; the
    /// next `draw` uploads it to `u_time`.
    pub fn set_frame_time(&mut self, secs: f32) {
        self.frame_time = secs;
    }

    /// Hand the renderer the latest resolved punctual lights for the active
    /// scene. The slice is cached locally and uploaded as part of the next
    /// `draw` call, so a per-frame call from the paint callback is cheap —
    /// just a Vec clone of up to MAX_LIGHTS entries. Pass an empty slice to
    /// fall back to the shader's hard-coded key/fill rig.
    pub fn set_lights(&mut self, lights: &[ResolvedLight]) {
        let n = lights.len().min(MAX_LIGHTS);
        if self.lights.len() != n
            || self
                .lights
                .iter()
                .zip(lights[..n].iter())
                .any(|(a, b)| !light_eq(a, b))
        {
            self.shadows_dirty = true;
        }
        self.lights.clear();
        self.lights.extend_from_slice(&lights[..n]);
    }

    /// Swap in a fresh environment-lighting preset. Cheap — just stores the
    /// params; the next `draw` uploads them.
    pub fn set_environment(&mut self, params: EnvironmentParams) {
        if self.environment != params {
            // Only the analytic key direction actually feeds the shadow
            // caster (and only when no DSL lights are present), but
            // dirtying on any env change keeps the logic simple — env
            // swaps are rare relative to per-frame paints.
            self.shadows_dirty = true;
        }
        self.environment = params;
    }

    /// Hand the renderer the scene's static-pose AABB so the shadow pre-pass
    /// can size its directional ortho frustum + spot/point far planes
    /// correctly. Cheap — just stashes the values; the next `draw` reads
    /// them when picking shadow casters.
    pub fn set_scene_aabb(&mut self, center: Vec3, radius: f32) {
        if self.scene_center != center || self.scene_radius != radius {
            self.shadows_dirty = true;
        }
        self.scene_center = center;
        self.scene_radius = radius;
    }

    /// Update the shadow-quality preset. When the resolution changes the
    /// underlying GPU resources (depth atlas + cubemap textures) are
    /// reallocated; otherwise this is just a flag flip the next `draw`
    /// honours when ranking casters.
    pub fn set_shadow_quality(&mut self, gl: &glow::Context, quality: ShadowQuality) {
        if self.shadow_quality == quality {
            return;
        }
        self.shadow_quality = quality;
        self.shadows.set_resolution(gl, quality.resolution());
        self.shadows_dirty = true;
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
            self.collider_runs = mesh.collider_index_runs.clone();
            self.palettes = mesh.palettes.clone();
            // Mesh changed — depth maps may now contain stale silhouettes.
            self.shadows_dirty = true;
        }
    }

    /// Refresh the per-batch matrix palettes without touching the VBO/EBO.
    /// Called every animation tick + every gizmo drag tick — the whole point
    /// of the rest-pose-baked vertex stream is that this is the only GPU
    /// upload required when the pose moves.
    pub fn upload_palettes(&mut self, palettes: &[SkinPalette]) {
        self.palettes.clear();
        self.palettes.extend_from_slice(palettes);
        // Pose changed — skinned silhouettes in the depth maps are now
        // stale. Rigid-only scenes pay one redundant rebuild per gizmo
        // drag tick, which is cheap relative to picking apart "did any
        // matrix actually change" here.
        self.shadows_dirty = true;
    }

    pub fn destroy(&mut self, gl: &glow::Context) {
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
        self.imposter.destroy(gl);
        self.lights_overlay.destroy(gl);
        self.colliders_overlay.destroy(gl);
        self.shadows.destroy(gl);
    }

    /// Render the viewport imposter billboard. Driven by the paint
    /// callback when `PreviewLod::Imposter` is active and an atlas texture
    /// is bound. `center` is the AABB midpoint; `half_width` /
    /// `half_height` come from the bake's framing so the quad occupies
    /// the model's own AABB extent (no float-above-the-model artefacts).
    /// `uv_y_top` / `uv_y_bottom` crop the cell's silhouette region.
    pub fn draw_imposter(
        &self,
        gl: &glow::Context,
        viewproj: glam::Mat4,
        camera_pos: glam::Vec3,
        center: glam::Vec3,
        half_width: f32,
        half_height: f32,
        view_count: u32,
        uv_y_top: f32,
        uv_y_bottom: f32,
        texture: glow::Texture,
    ) {
        self.imposter.draw(
            gl,
            viewproj,
            camera_pos,
            center,
            half_width,
            half_height,
            view_count,
            uv_y_top,
            uv_y_bottom,
            texture,
        );
    }

    /// Upload a new atlas as a GL texture. Re-export of [`ImposterGl::upload_atlas`]
    /// so the paint callback can hand the bake's RGBA bytes through the
    /// renderer instead of reaching into the overlay module directly.
    pub fn upload_imposter_atlas(
        gl: &glow::Context,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> anyhow::Result<glow::Texture> {
        ImposterGl::upload_atlas(gl, rgba, width, height)
    }

    pub fn destroy_imposter_texture(gl: &glow::Context, texture: glow::Texture) {
        ImposterGl::destroy_texture(gl, texture);
    }

    /// Draw the ground reference grid. Meant to be called immediately after
    /// the depth clear, before the main scene pass, so the scene's depth
    /// test naturally occludes lines behind solid geometry.
    pub fn draw_grid(&self, gl: &glow::Context, viewproj: glam::Mat4, camera_pos: Vec3) {
        self.grid.draw(gl, viewproj, camera_pos);
    }

    /// Draw the gizmo overlay. Called from the paint callback after the
    /// scene pass finishes.
    pub fn draw_gizmo(
        &self,
        gl: &glow::Context,
        viewproj: glam::Mat4,
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
        viewproj: glam::Mat4,
        eye: Vec3,
        viewport_height: f32,
        selected: &[mogen_core::NodeId],
    ) {
        if self.lights.is_empty() {
            return;
        }
        self.lights_overlay
            .draw(gl, viewproj, eye, viewport_height, &self.lights, selected);
    }

    /// Draw the collider overlay. AABB colliders come in as
    /// [`ColliderInstance`]s (built by [`super::colliders_gl::collect`]);
    /// trimesh/convex colliders are drawn by re-rendering the node's mesh in
    /// wireframe from [`Self::collider_runs`], reusing the main scene VAO.
    /// `selected` highlights the matching runs in gold.
    pub fn draw_colliders_overlay(
        &self,
        gl: &glow::Context,
        viewproj: glam::Mat4,
        instances: &[ColliderInstance],
        selected: &[mogen_core::NodeId],
    ) {
        self.colliders_overlay.draw(gl, viewproj, instances);
        if !self.collider_runs.is_empty() {
            let runs: Vec<(u32, u32, bool)> = self
                .collider_runs
                .iter()
                .map(|&(start, count, node)| (start, count, selected.contains(&node)))
                .collect();
            self.colliders_overlay
                .draw_trimesh_runs(gl, viewproj, self.vao, &runs);
        }
    }
}
