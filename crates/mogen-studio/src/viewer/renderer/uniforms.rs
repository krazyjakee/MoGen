//! Per-frame uniform uploads for the main forward pass: punctual lights,
//! shadow caster matrices + per-light index lookups, and the joint matrix
//! palette for the currently-bound batch. Caller must have already bound
//! [`Renderer::program`].

use glam::Mat4;
use glow::HasContext;

use super::super::flatten::MAX_JOINTS;
use super::super::lights::{kind_to_int, MAX_LIGHTS};
use super::super::shadows::{ShadowCaster, MAX_SHADOW_2D, MAX_SHADOW_CUBE};
use super::Renderer;

impl Renderer {
    /// Push the cached light list to the active shader program. Pads the
    /// trailing slots with zeroes so the shader's `i < u_num_lights` guard
    /// is the only thing protecting us from reading garbage — uniforms left
    /// from a previous draw with a longer light list would otherwise
    /// re-enter the loop the next time the count grew. Caller must have
    /// already bound `self.program`.
    pub(super) fn upload_lights(&self, gl: &glow::Context) {
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

    /// Push the shadow caster uniforms (light-space matrices, per-light
    /// shadow indices, fallback rig flag, bias scalars) to the bound main
    /// program. Pads unused slots with -1 / identity so the FS sampling
    /// helpers cleanly skip work even when a lighting topology change
    /// shrinks the caster set.
    pub(super) fn upload_shadow_uniforms(&self, gl: &glow::Context) {
        let frame = &self.shadow_frame;
        // Pre-fill arrays with default "no shadow" sentinels.
        let mut viewproj = [0.0f32; MAX_SHADOW_2D * 16];
        for slice in 0..MAX_SHADOW_2D {
            let mat = Mat4::IDENTITY.to_cols_array();
            viewproj[slice * 16..(slice + 1) * 16].copy_from_slice(&mat);
        }
        let mut cube_pos = [0.0f32; MAX_SHADOW_CUBE * 3];
        let mut cube_far = [0.0f32; MAX_SHADOW_CUBE];
        let mut light_idx_2d = [-1i32; MAX_LIGHTS];
        let mut light_idx_cube = [-1i32; MAX_LIGHTS];
        let mut fallback_idx: i32 = -1;

        // Populate 2D casters. `light_index = -1` is the synthetic
        // env-fallback sun (drawn when the scene has no DSL lights), routed
        // through `u_shadow_fallback_idx` instead of any per-light slot.
        for (slice, caster) in frame.casters_2d.iter().enumerate().take(MAX_SHADOW_2D) {
            let (vp, light_index) = match caster {
                ShadowCaster::Directional {
                    view_proj,
                    light_index,
                    ..
                }
                | ShadowCaster::Spot {
                    view_proj,
                    light_index,
                    ..
                } => (*view_proj, *light_index),
                ShadowCaster::Point { .. } => (Mat4::IDENTITY, -1),
            };
            viewproj[slice * 16..(slice + 1) * 16].copy_from_slice(&vp.to_cols_array());
            if light_index < 0 {
                fallback_idx = slice as i32;
            } else if (light_index as usize) < MAX_LIGHTS {
                light_idx_2d[light_index as usize] = slice as i32;
            }
        }
        // Populate cube casters.
        for (slot, caster) in frame.casters_cube.iter().enumerate().take(MAX_SHADOW_CUBE) {
            if let ShadowCaster::Point {
                position,
                far_plane,
                light_index,
                ..
            } = caster
            {
                cube_pos[slot * 3] = position.x;
                cube_pos[slot * 3 + 1] = position.y;
                cube_pos[slot * 3 + 2] = position.z;
                cube_far[slot] = *far_plane;
                if (*light_index as usize) < MAX_LIGHTS && *light_index >= 0 {
                    light_idx_cube[*light_index as usize] = slot as i32;
                }
            }
        }

        unsafe {
            if let Some(loc) = &self.u_shadow_2d_viewproj {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &viewproj);
            }
            if let Some(loc) = &self.u_shadow_cube_pos {
                gl.uniform_3_f32_slice(Some(loc), &cube_pos);
            }
            if let Some(loc) = &self.u_shadow_cube_far {
                gl.uniform_1_f32_slice(Some(loc), &cube_far);
            }
            if let Some(loc) = &self.u_light_shadow_2d_idx {
                gl.uniform_1_i32_slice(Some(loc), &light_idx_2d);
            }
            if let Some(loc) = &self.u_light_shadow_cube_idx {
                gl.uniform_1_i32_slice(Some(loc), &light_idx_cube);
            }
            if let Some(loc) = &self.u_shadow_fallback_idx {
                gl.uniform_1_i32(Some(loc), fallback_idx);
            }
            if let Some(loc) = &self.u_shadow_bias_const {
                gl.uniform_1_f32(Some(loc), 0.0008);
            }
            if let Some(loc) = &self.u_shadow_bias_slope {
                gl.uniform_1_f32(Some(loc), 0.004);
            }
            if let Some(loc) = &self.u_shadow_strength {
                gl.uniform_1_f32(Some(loc), 0.7);
            }
            if let Some(loc) = &self.u_shadow_2d_texel {
                let res = self.shadow_quality.resolution().max(1) as f32;
                gl.uniform_1_f32(Some(loc), 1.0 / res);
            }
            if let Some(loc) = &self.u_shadow_pcf_taps {
                gl.uniform_1_i32(Some(loc), self.shadow_quality.pcf_taps());
            }
        }
    }

    /// Upload the matrix palette for `palette_id` into `u_joint_mats`. The
    /// shader applies it unconditionally (rigid batches just have weights
    /// `[1,0,0,0]` and a one-bone palette per node), so there is no
    /// "skinning off" branch to worry about — every batch has a palette.
    /// Uses [`Self::palette_scratch`] as the flatten buffer so consecutive
    /// frames don't reallocate.
    pub(super) fn bind_palette(&mut self, gl: &glow::Context, palette_id: u32) {
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
}
