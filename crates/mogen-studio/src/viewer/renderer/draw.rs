//! The main forward pass: prepare per-batch texture handles, partition
//! into opaque/blend buckets, frustum-cull, sort blends back-to-front, and
//! issue draw calls with material-equality and palette/state-change skips
//! between adjacent batches.

use glam::{Mat4, Vec3};
use glow::HasContext;
use mogen_core::AlphaMode;

use super::super::gl_util::FrustumPlanes;
use super::super::shadows::select_casters;
use super::Renderer;

/// Wraps a batch's `material_id` so the per-batch loop can compare materials
/// with `PartialEq` without dragging the whole `DrawBatch` into the
/// comparison. Two batches with the same key share PBR uniforms + texture
/// bindings, so the loop can skip those uploads on every batch after the
/// first in a chain.
#[derive(Copy, Clone, PartialEq, Eq)]
struct MaterialKey(Option<u32>);

impl Renderer {
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
        }

        // Shadow pre-pass: only re-rank casters and re-render depth maps
        // when something scene-relevant changed (light/palette/env/AABB/
        // mesh/quality). Camera motion alone does not invalidate the maps —
        // they live in world space — so static scenes get a no-op pass on
        // every redraw after the first. The cached `ShadowFrame`'s caster
        // matrices are still uploaded to the main FS each frame regardless.
        let mut prev_viewport = [0i32; 4];
        unsafe {
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut prev_viewport);
        }
        if self.shadows_dirty {
            select_casters(
                self.shadow_quality,
                &self.lights,
                &self.environment,
                self.scene_center,
                self.scene_radius,
                &mut self.shadow_frame,
                &mut self.shadow_ranked_scratch,
            );
            if !self.shadow_frame.is_empty() {
                self.shadows.render(
                    gl,
                    &self.shadow_frame,
                    self.vao,
                    &self.batches,
                    &self.palettes,
                    prev_viewport[0],
                    prev_viewport[1],
                    prev_viewport[2],
                    prev_viewport[3],
                );
            }
            self.shadows_dirty = false;
        }

        unsafe {
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
            // Shadow sampler bindings. Units 5..=7 are reserved for the
            // shadow atlas + the two cubemap slots; the FS sampler uniforms
            // address those units even when shadows are off so the bind
            // doesn't have to swap when toggling quality at runtime.
            if let Some(loc) = &self.u_shadow_2d {
                gl.uniform_1_i32(Some(loc), 5);
            }
            if let Some(loc) = &self.u_shadow_cube0 {
                gl.uniform_1_i32(Some(loc), 6);
            }
            if let Some(loc) = &self.u_shadow_cube1 {
                gl.uniform_1_i32(Some(loc), 7);
            }
            // Per-frame shadow uniforms. Pack the caster light-space matrices,
            // per-light index lookups, fallback-rig flag, bias scalars, and
            // the strength factor that lerps a fully-shadowed fragment back
            // toward neutral. Index uniforms default to -1 (no shadow) when
            // the caster slot is empty so the FS sampling helpers cleanly
            // skip work.
            self.upload_shadow_uniforms(gl);
            self.shadows.bind_for_main_pass(gl, 5, 6);
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
            // Same idea for the shadow sampler units — leave nothing
            // bound so a later pass that reuses these slots isn't fed the
            // depth atlas by accident.
            self.shadows.unbind(gl, 5, 6);
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
}
