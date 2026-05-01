//! Wireframe overlay for `light` nodes. Draws a small kind-specific glyph at
//! each light's world pose so users can see — and click on — lights even
//! though the lights themselves carry no mesh:
//!
//! - Directional: arrow pointing along the light direction.
//! - Point:       three orthogonal great-circle rings forming a wire sphere.
//! - Spot:        a cone outline opened to the outer cone half-angle, with
//!                a tip ring the same size as the point glyph.
//!
//! Geometry is uploaded once at startup as a single VBO laid out
//! `[arrow | sphere | cone-frame | cone-rim]`; the per-kind sub-range is drawn
//! once per light with a per-light model matrix that places + orients the
//! glyph in world space. A separate "halo" ring drawn billboard-style around
//! every light gives a consistent click target regardless of camera angle —
//! the viewport picker tests against it.

use glam::{Mat4, Quat, Vec3};
use glow::HasContext;

use super::gl_util::{bytes_of_f32, compile_program};
use super::lights::ResolvedLight;
use mogen_core::{LightKind, NodeId};

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

pub(super) struct LightsGl {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    u_viewproj: Option<glow::UniformLocation>,
    u_model: Option<glow::UniformLocation>,
    u_color: Option<glow::UniformLocation>,
    arrow_start: i32,
    arrow_count: i32,
    sphere_start: i32,
    sphere_count: i32,
    cone_start: i32,
    cone_count: i32,
    halo_start: i32,
    halo_count: i32,
}

impl LightsGl {
    pub(super) fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        unsafe {
            let program = compile_program(gl, VS, FS)?;
            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow::anyhow!("lights vao: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("lights vbo: {e}"))?;

            let mut data: Vec<f32> = Vec::new();

            // --- Arrow (directional). Shaft along +Z to a tip at (0,0,1.0),
            //     plus four "feathers" coming off the tip back to (±0.15, 0,
            //     0.7) and (0, ±0.15, 0.7). All in the glyph's local space —
            //     the per-light model matrix rotates this so the +Z axis lies
            //     along the light's direction in world space.
            let arrow_start = (data.len() / 3) as i32;
            push_line(&mut data, [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
            push_line(&mut data, [0.0, 0.0, 1.0], [ 0.15, 0.0, 0.7]);
            push_line(&mut data, [0.0, 0.0, 1.0], [-0.15, 0.0, 0.7]);
            push_line(&mut data, [0.0, 0.0, 1.0], [0.0,  0.15, 0.7]);
            push_line(&mut data, [0.0, 0.0, 1.0], [0.0, -0.15, 0.7]);
            let arrow_count = (data.len() / 3) as i32 - arrow_start;

            // --- Sphere (point). Three great circles in XY/XZ/YZ.
            let sphere_start = (data.len() / 3) as i32;
            let segments = 24usize;
            let push_circle = |data: &mut Vec<f32>, plane: usize| {
                for s in 0..segments {
                    let a0 = std::f32::consts::TAU * (s as f32) / (segments as f32);
                    let a1 = std::f32::consts::TAU * ((s + 1) as f32) / (segments as f32);
                    let mut p0 = [0.0f32; 3];
                    let mut p1 = [0.0f32; 3];
                    let (u, v) = match plane {
                        0 => (0, 1), // XY
                        1 => (0, 2), // XZ
                        _ => (1, 2), // YZ
                    };
                    p0[u] = a0.cos();
                    p0[v] = a0.sin();
                    p1[u] = a1.cos();
                    p1[v] = a1.sin();
                    push_line(data, p0, p1);
                }
            };
            push_circle(&mut data, 0);
            push_circle(&mut data, 1);
            push_circle(&mut data, 2);
            let sphere_count = (data.len() / 3) as i32 - sphere_start;

            // --- Cone frame (spot). Built around the +Z axis pointing along
            //     the light direction. Apex at origin; the rim sits one unit
            //     forward at radius 1. Per-light model matrix scales x/y by
            //     `tan(outer_cone)` and z by `range` (clamped to a sensible
            //     visible length) so the wire opens to the right cone.
            let cone_start = (data.len() / 3) as i32;
            // Four "ribs" running apex to rim — 90° apart on the rim circle.
            for i in 0..4 {
                let a = std::f32::consts::TAU * (i as f32) / 4.0;
                push_line(&mut data, [0.0, 0.0, 0.0], [a.cos(), a.sin(), 1.0]);
            }
            // Rim circle at z=1.
            for s in 0..segments {
                let a0 = std::f32::consts::TAU * (s as f32) / (segments as f32);
                let a1 = std::f32::consts::TAU * ((s + 1) as f32) / (segments as f32);
                push_line(
                    &mut data,
                    [a0.cos(), a0.sin(), 1.0],
                    [a1.cos(), a1.sin(), 1.0],
                );
            }
            let cone_count = (data.len() / 3) as i32 - cone_start;

            // --- Halo ring. View-aligned circle of radius 1 in the XY plane —
            //     the per-light model matrix rotates the ring's local Z axis
            //     to face the camera so the ring always reads as a circle.
            //     This is what the picker hit-tests so the click target stays
            //     constant regardless of the glyph kind.
            let halo_start = (data.len() / 3) as i32;
            for s in 0..segments {
                let a0 = std::f32::consts::TAU * (s as f32) / (segments as f32);
                let a1 = std::f32::consts::TAU * ((s + 1) as f32) / (segments as f32);
                push_line(
                    &mut data,
                    [a0.cos(), a0.sin(), 0.0],
                    [a1.cos(), a1.sin(), 0.0],
                );
            }
            let halo_count = (data.len() / 3) as i32 - halo_start;

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
                arrow_start,
                arrow_count,
                sphere_start,
                sphere_count,
                cone_start,
                cone_count,
                halo_start,
                halo_count,
            })
        }
    }

    pub(super) fn draw(
        &self,
        gl: &glow::Context,
        viewproj: Mat4,
        eye: Vec3,
        viewport_height: f32,
        lights: &[ResolvedLight],
        selected: Option<NodeId>,
    ) {
        unsafe {
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            // Standard depth test (so a light hidden behind a wall reads as
            // occluded), depth writes off (the overlay shouldn't shadow real
            // geometry on later passes), no culling (lines are 1D), constant
            // line width.
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(false);
            gl.disable(glow::CULL_FACE);
            gl.line_width(1.5);

            if let Some(loc) = &self.u_viewproj {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &viewproj.to_cols_array());
            }

            for l in lights {
                // Screen-space size constant: keep the glyph the same apparent
                // size regardless of camera distance. `handle_scale` in the
                // gizmo module uses the same heuristic — borrow it here so
                // light glyphs and the transform gizmo read at the same
                // weight on screen.
                let halo_size =
                    crate::gizmo::handle_scale(l.position, eye, viewport_height) * 0.35;
                let is_selected = selected.map(|s| s == l.node).unwrap_or(false);
                // Tint the glyph with the light's authored color so the user
                // can read which light is which at a glance, but lift it
                // toward white so dim/dark colors stay readable. Selection
                // overrides with a pure yellow so it pops out.
                let color = if is_selected {
                    [1.0, 0.85, 0.0]
                } else {
                    glyph_color(l.color)
                };
                if let Some(loc) = &self.u_color {
                    gl.uniform_3_f32(Some(loc), color[0], color[1], color[2]);
                }

                // Per-kind glyph: directional → arrow rotated to dir,
                // point → sphere, spot → cone scaled to outer cone.
                match l.kind {
                    LightKind::Directional => {
                        let model = directional_model(l.position, l.direction, halo_size * 1.6);
                        self.set_model(gl, model);
                        gl.draw_arrays(glow::LINES, self.arrow_start, self.arrow_count);
                    }
                    LightKind::Point => {
                        let model = uniform_model(l.position, halo_size);
                        self.set_model(gl, model);
                        gl.draw_arrays(glow::LINES, self.sphere_start, self.sphere_count);
                    }
                    LightKind::Spot => {
                        let cone_len = cone_length(l.range, halo_size);
                        let radius = (l.outer_cos.acos()).tan().max(0.01) * cone_len;
                        let model = spot_model(l.position, l.direction, radius, cone_len);
                        self.set_model(gl, model);
                        gl.draw_arrays(glow::LINES, self.cone_start, self.cone_count);
                    }
                }

                // Halo ring: always drawn, billboard-aligned, screen-space
                // sized. Doubles as the picker hit target.
                let halo_model = halo_model(l.position, eye, halo_size);
                self.set_model(gl, halo_model);
                gl.draw_arrays(glow::LINES, self.halo_start, self.halo_count);
            }

            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.depth_mask(true);
            gl.enable(glow::CULL_FACE);
        }
    }

    fn set_model(&self, gl: &glow::Context, model: Mat4) {
        unsafe {
            if let Some(loc) = &self.u_model {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &model.to_cols_array());
            }
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

/// Lift a possibly-dim authored colour into a guaranteed-readable glyph
/// tint. Lights with `intensity = 0` or near-black tint would otherwise
/// vanish on screen; clamping the minimum component to 0.6 keeps the
/// indicator legible while preserving the colour identity.
fn glyph_color(c: [f32; 3]) -> [f32; 3] {
    let m = c[0].max(c[1]).max(c[2]).max(1e-4);
    let scale = 0.9 / m;
    [
        (c[0] * scale).max(0.45),
        (c[1] * scale).max(0.45),
        (c[2] * scale).max(0.45),
    ]
}

/// Pick a sensible visible cone length: clamp the user's `range` (which can
/// be 0 for unlimited or much larger than the scene) into [4, 25] × the
/// halo size so the glyph reads regardless of how the user authored the
/// light. The halo's apparent size is camera-distance-stable, so anchoring
/// on it keeps the cone proportional to the rest of the indicator.
fn cone_length(range: f32, halo_size: f32) -> f32 {
    let min_len = halo_size * 4.0;
    let max_len = halo_size * 25.0;
    if range > 0.0 {
        range.clamp(min_len, max_len)
    } else {
        max_len
    }
}

/// `+Z`-facing arrow at `pos` pointing along `dir`. Length is fixed in
/// world units so a directional light reads as roughly the same size on
/// screen as the other glyphs (whose model matrices already incorporate
/// the screen-space halo size).
fn directional_model(pos: Vec3, dir: Vec3, length: f32) -> Mat4 {
    let dir = dir.normalize_or_zero();
    let dir = if dir.length_squared() < 1e-8 {
        Vec3::NEG_Y
    } else {
        dir
    };
    let rot = Quat::from_rotation_arc(Vec3::Z, dir);
    Mat4::from_scale_rotation_translation(Vec3::splat(length), rot, pos)
}

/// Uniform-scaled glyph centred on `pos`. Used for point lights — orientation
/// of the wire sphere doesn't matter so we hand back identity rotation.
fn uniform_model(pos: Vec3, scale: f32) -> Mat4 {
    Mat4::from_scale_rotation_translation(Vec3::splat(scale), Quat::IDENTITY, pos)
}

/// Cone glyph for a spotlight: open along `dir`, rim radius `radius`, length
/// `length`. The base geometry is centred on the apex with rim at z=1; we
/// scale (x,y) by `radius` and z by `length` then rotate +Z onto `dir`.
fn spot_model(pos: Vec3, dir: Vec3, radius: f32, length: f32) -> Mat4 {
    let dir = dir.normalize_or_zero();
    let dir = if dir.length_squared() < 1e-8 {
        Vec3::NEG_Y
    } else {
        dir
    };
    let rot = Quat::from_rotation_arc(Vec3::Z, dir);
    Mat4::from_scale_rotation_translation(Vec3::new(radius, radius, length), rot, pos)
}

/// View-aligned ring at `pos` so the halo always reads as a circle. The
/// rotation maps the ring's local Z axis (its surface normal) onto the
/// view direction; falls back to identity for the degenerate case where
/// the light sits exactly at the eye.
fn halo_model(pos: Vec3, eye: Vec3, scale: f32) -> Mat4 {
    let view = (eye - pos).normalize_or_zero();
    let rot = if view.length_squared() < 1e-8 {
        Quat::IDENTITY
    } else {
        Quat::from_rotation_arc(Vec3::Z, view)
    };
    Mat4::from_scale_rotation_translation(Vec3::splat(scale), rot, pos)
}
