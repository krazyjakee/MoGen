//! Realtime shadow mapping for the viewport.
//!
//! Renders a small fixed-size atlas of depth maps before the main pass, then
//! the main fragment shader samples it to darken occluded fragments. Three
//! light kinds are supported, all packed into the same atlas plumbing:
//!
//! - **Directional / spot** → orthographic or perspective depth render into
//!   a slice of a single `GL_TEXTURE_2D_ARRAY` shadow texture (`sampler2D
//!   ArrayShadow` in the FS — hardware PCF with one tap).
//! - **Point** → perspective depth render across the six faces of a
//!   per-caster `GL_TEXTURE_CUBE_MAP` (`samplerCubeShadow` in the FS). The
//!   FS measures world-space distance to the light and compares against
//!   linearised depth written via `gl_FragDepth` so cone-test math doesn't
//!   leak in.
//!
//! Caster count and resolution are driven by [`ShadowQuality`]:
//!
//! | Quality | Map size | 2D casters | Cube casters |
//! |---------|---------:|-----------:|-------------:|
//! | Off     |        — |          0 |            0 |
//! | Low     |      512 |          2 |            1 |
//! | Medium  |     1024 |          3 |            1 |
//! | High    |     2048 |          4 |            2 |
//!
//! The caps stay small on purpose: each 2D caster is one extra depth pass
//! over every opaque skinned/rigid batch, and each cube caster is *six*. The
//! ranking pass in [`select_casters`] picks the most influential lights so
//! the budget lands where the user is most likely to notice.

use glam::{Mat4, Vec3};
use glow::HasContext;
use mogen_core::LightKind;

use super::environment::EnvironmentParams;
use super::flatten::{DrawBatch, SkinPalette, MAX_JOINTS};
use super::gl_util::{compile_program, FrustumPlanes};
use super::lights::ResolvedLight;
use super::shaders::{SHADOW_DIR_FS, SHADOW_DIR_VS, SHADOW_POINT_FS, SHADOW_POINT_VS};

/// Hard cap on the number of 2D shadow slices the atlas allocates regardless
/// of [`ShadowQuality::caster_count_2d`]. Mirrors the GLSL `MAX_SHADOW_2D`
/// constant in the main fragment shader; updating one without the other
/// produces an out-of-bounds slice on the GPU.
pub const MAX_SHADOW_2D: usize = 4;

/// Hard cap on the number of point-light cubemap shadow casters. Six render
/// passes per caster makes this the most expensive slot to fill — the GLSL
/// sampling path unrolls these as a sequence of `if (idx == k)` branches so
/// the shader cost scales linearly. Mirrors `MAX_SHADOW_CUBE` in the FS.
pub const MAX_SHADOW_CUBE: usize = 2;

/// Quality presets selectable from the viewport overlay. `Off` skips the
/// shadow pre-pass entirely so users with older GPUs can dodge the work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShadowQuality {
    Off,
    Low,
    Medium,
    High,
}

pub const DEFAULT_SHADOW_QUALITY: ShadowQuality = ShadowQuality::Off;

pub const SHADOW_QUALITIES: [ShadowQuality; 4] = [
    ShadowQuality::Off,
    ShadowQuality::Low,
    ShadowQuality::Medium,
    ShadowQuality::High,
];

impl ShadowQuality {
    /// Per-slice shadow map resolution. Returned as `0` for `Off` so callers
    /// can use it unconditionally as the FBO width/height — the renderer
    /// short-circuits to skip allocation when `is_on()` is false.
    pub fn resolution(self) -> i32 {
        match self {
            ShadowQuality::Off => 0,
            ShadowQuality::Low => 512,
            ShadowQuality::Medium => 1024,
            ShadowQuality::High => 2048,
        }
    }

    /// How many directional + spot casters the atlas reserves slices for.
    /// Capped by [`MAX_SHADOW_2D`] independent of this value so the GLSL
    /// sampler array size stays a compile-time constant.
    pub fn caster_count_2d(self) -> usize {
        match self {
            ShadowQuality::Off => 0,
            ShadowQuality::Low => 2,
            ShadowQuality::Medium => 3,
            ShadowQuality::High => 4,
        }
    }

    /// How many point-light cubemap casters the renderer keeps live. Each
    /// adds 6 depth passes to the pre-pass cost, so this stays small.
    pub fn caster_count_cube(self) -> usize {
        match self {
            ShadowQuality::Off => 0,
            ShadowQuality::Low => 1,
            ShadowQuality::Medium => 1,
            ShadowQuality::High => 2,
        }
    }

    pub fn is_on(self) -> bool {
        !matches!(self, ShadowQuality::Off)
    }

    pub fn label(self) -> &'static str {
        match self {
            ShadowQuality::Off => "Off",
            ShadowQuality::Low => "Low (512)",
            ShadowQuality::Medium => "Medium (1024)",
            ShadowQuality::High => "High (2048)",
        }
    }

    /// Number of PCF taps the main FS evaluates per shadow lookup. Hardware
    /// `sampler*Shadow` already does a 2×2 PCF inside each tap, so a single-
    /// tap lookup is already smoother than nearest. Higher quality presets
    /// fan out to a small Poisson disk for softer penumbras.
    pub fn pcf_taps(self) -> i32 {
        match self {
            ShadowQuality::Off => 1,
            ShadowQuality::Low => 1,
            ShadowQuality::Medium => 5,
            ShadowQuality::High => 9,
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            ShadowQuality::Off => "Off",
            ShadowQuality::Low => "Low",
            ShadowQuality::Medium => "Med",
            ShadowQuality::High => "High",
        }
    }
}

impl Default for ShadowQuality {
    fn default() -> Self {
        DEFAULT_SHADOW_QUALITY
    }
}

pub fn shadow_quality_key(q: ShadowQuality) -> &'static str {
    match q {
        ShadowQuality::Off => "off",
        ShadowQuality::Low => "low",
        ShadowQuality::Medium => "medium",
        ShadowQuality::High => "high",
    }
}

pub fn parse_shadow_quality(s: &str) -> Option<ShadowQuality> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" | "none" | "" => Some(ShadowQuality::Off),
        "low" => Some(ShadowQuality::Low),
        "medium" | "med" => Some(ShadowQuality::Medium),
        "high" => Some(ShadowQuality::High),
        _ => None,
    }
}

/// One shadow caster prepared for a frame. The renderer fills these in via
/// [`select_casters`] each draw before invoking the depth pre-pass.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum ShadowCaster {
    /// Directional or spot: a single light-space matrix is enough.
    /// `light_index_in_lights` encodes which entry of the resolved light list
    /// this caster shadows; -1 is the "synthetic env-fallback sun" case where
    /// the FS direct-lighting falls back to its analytic key/fill rig.
    Directional {
        view_proj: Mat4,
        /// Direction the light points along (used by the depth-pass front-
        /// face cull bias to avoid peter-panning on slabs).
        direction: Vec3,
        /// Index in the FS `u_light_*` arrays this caster lights, or -1 to
        /// shadow the analytic key/fill rig.
        light_index: i32,
    },
    Spot {
        view_proj: Mat4,
        position: Vec3,
        direction: Vec3,
        light_index: i32,
    },
    /// Point: 6 light-space matrices, one per cube face, plus the world
    /// position and far plane the FS uses to reconstruct linear depth.
    Point {
        face_view_projs: [Mat4; 6],
        position: Vec3,
        far_plane: f32,
        light_index: i32,
    },
}

/// Plan describing what to render this frame. `casters_2d[i]` lights the
/// `i`-th 2D atlas slice; `casters_cube[i]` lights the `i`-th cube map.
#[derive(Default)]
pub struct ShadowFrame {
    pub casters_2d: Vec<ShadowCaster>,
    pub casters_cube: Vec<ShadowCaster>,
}

impl ShadowFrame {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.casters_2d.clear();
        self.casters_cube.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.casters_2d.is_empty() && self.casters_cube.is_empty()
    }
}

/// Pick which lights cast shadows this frame and build their light-space
/// matrices. Always preserves order across frames within a stable scene so
/// the atlas slices don't churn.
///
/// `lights` is the resolved list passed to the main shader; `env` is the
/// environment preset used as the fallback rig when `lights` is empty.
/// `scene_center` / `scene_radius` come from the current FlatMesh AABB —
/// they size the directional ortho frustum so the shadow map covers all
/// visible geometry without wasting resolution on empty space.
pub fn select_casters(
    quality: ShadowQuality,
    lights: &[ResolvedLight],
    env: &EnvironmentParams,
    scene_center: Vec3,
    scene_radius: f32,
    out: &mut ShadowFrame,
    ranked: &mut Vec<(usize, f32)>,
) {
    out.clear();
    ranked.clear();

    if !quality.is_on() {
        return;
    }

    let cap_2d = quality.caster_count_2d().min(MAX_SHADOW_2D);
    let cap_cube = quality.caster_count_cube().min(MAX_SHADOW_CUBE);

    // Floor on the framing radius so a one-vertex / empty scene still picks
    // sane ortho extents — without this, `radius * 1.5` collapses to 0 and
    // the directional ortho frustum has zero volume, producing an all-white
    // depth map and a fully-shadowed scene.
    let radius = scene_radius.max(1.0);

    if lights.is_empty() {
        // No DSL lights — the FS uses the analytic key/fill rig, so the
        // single shadow caster is the env preset's key direction. We only
        // bother when the sun has any colour to contribute; an `Indoor` /
        // `Overcast` preset (sun_color = 0) wouldn't cast a shadow disc.
        if cap_2d > 0 {
            let dir = env.key_dir.normalize_or_zero();
            if dir != Vec3::ZERO {
                let vp = directional_light_space(dir, scene_center, radius);
                out.casters_2d.push(ShadowCaster::Directional {
                    view_proj: vp,
                    direction: dir,
                    light_index: -1,
                });
            }
        }
        return;
    }

    // Rank DSL lights by importance: brightness × inverse-distance for
    // point/spot, raw brightness for directional. Stable, deterministic,
    // and matches the gut feel of "the brightest nearby light".
    ranked.reserve(lights.len());
    for (i, l) in lights.iter().enumerate() {
        ranked.push((i, light_importance(l, scene_center, scene_radius)));
    }
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    for &(idx, _) in ranked.iter() {
        let l = &lights[idx];
        match l.kind {
            LightKind::Directional => {
                if out.casters_2d.len() >= cap_2d {
                    continue;
                }
                let dir = l.direction.normalize_or_zero();
                if dir == Vec3::ZERO {
                    continue;
                }
                let vp = directional_light_space(dir, scene_center, radius);
                out.casters_2d.push(ShadowCaster::Directional {
                    view_proj: vp,
                    direction: dir,
                    light_index: idx as i32,
                });
            }
            LightKind::Spot => {
                if out.casters_2d.len() >= cap_2d {
                    continue;
                }
                let dir = l.direction.normalize_or_zero();
                if dir == Vec3::ZERO {
                    continue;
                }
                let vp = spot_light_space(l, scene_center, radius);
                out.casters_2d.push(ShadowCaster::Spot {
                    view_proj: vp,
                    position: l.position,
                    direction: dir,
                    light_index: idx as i32,
                });
            }
            LightKind::Point => {
                if out.casters_cube.len() >= cap_cube {
                    continue;
                }
                let far = point_far_plane(l, scene_center, scene_radius);
                let face_vps = cube_face_view_projs(l.position, far);
                out.casters_cube.push(ShadowCaster::Point {
                    face_view_projs: face_vps,
                    position: l.position,
                    far_plane: far,
                    light_index: idx as i32,
                });
            }
        }
        if out.casters_2d.len() >= cap_2d && out.casters_cube.len() >= cap_cube {
            break;
        }
    }
}

/// Heuristic importance score for caster ranking. Bright lights win;
/// distant point/spot lights with steep falloff lose. Used only for sorting
/// — the absolute scale doesn't matter.
fn light_importance(light: &ResolvedLight, scene_center: Vec3, scene_radius: f32) -> f32 {
    let lum =
        0.2126 * light.color[0] + 0.7152 * light.color[1] + 0.0722 * light.color[2];
    match light.kind {
        LightKind::Directional => lum,
        LightKind::Point | LightKind::Spot => {
            let d = (light.position - scene_center).length();
            let reach = scene_radius.max(0.1);
            // Lights near the model contribute much more than ones an order
            // of magnitude further out. The `1 / (d/reach + 1)^2` falloff
            // is just for ranking; the FS uses the proper KHR window curve.
            let falloff = 1.0 / ((d / reach + 1.0).powi(2));
            lum * falloff
        }
    }
}

/// Build the directional light-space `viewproj` whose ortho frustum exactly
/// covers the bounding sphere `(center, radius)`. The view direction is
/// `light_dir`; the light "position" is placed one radius behind the centre
/// along `-light_dir` so the near plane sits flush with the sphere front.
pub fn directional_light_space(light_dir: Vec3, center: Vec3, radius: f32) -> Mat4 {
    let dir = light_dir.normalize_or_zero();
    if dir == Vec3::ZERO {
        return Mat4::IDENTITY;
    }
    let r = radius.max(1.0);
    // Pad the frustum a touch so blockers just outside the visible AABB still
    // contribute (e.g. a pillar whose base is in-frame but whose top is just
    // above the radius sphere). 1.05 is the smallest pad that still covers
    // the corner-of-AABB case for a sphere-derived radius without wasting
    // texels — the previous 1.2 multiplier shed ~31% of resolution to empty
    // space around the scene.
    let pad = r * 1.05;
    let eye = center - dir * (r * 2.0);
    let up = pick_up_for(dir);
    let view = Mat4::look_at_rh(eye, center, up);
    let proj = Mat4::orthographic_rh_gl(-pad, pad, -pad, pad, 0.1, r * 4.0);
    proj * view
}

/// Build a spot-light perspective light-space `viewproj`. FOV comes from
/// the outer cone; near is a small constant; far is `range` if set,
/// otherwise a scene-radius derived fallback so unbounded spots still have a
/// finite frustum.
pub fn spot_light_space(light: &ResolvedLight, scene_center: Vec3, scene_radius: f32) -> Mat4 {
    let dir = light.direction.normalize_or_zero();
    if dir == Vec3::ZERO {
        return Mat4::IDENTITY;
    }
    // outer_cos = cos(outer_half_angle); recover the angle for the projection.
    let outer_cos = light.outer_cos.clamp(-1.0, 1.0);
    let outer_half = outer_cos.acos();
    let fov_y = (outer_half * 2.0).clamp(0.05, std::f32::consts::PI - 0.05);
    let near = 0.05;
    let far = if light.range > 0.0 {
        light.range.max(near + 0.1)
    } else {
        // Reach to the far edge of the scene from the light's position so a
        // rangeless spot still has bounded depth precision.
        let to_center = (scene_center - light.position).length();
        (to_center + scene_radius * 1.5).max(near + 0.1)
    };
    let up = pick_up_for(dir);
    let view = Mat4::look_at_rh(light.position, light.position + dir, up);
    let proj = Mat4::perspective_rh_gl(fov_y, 1.0, near, far);
    proj * view
}

/// Bound on the per-axis far plane for a point light. Uses the declared
/// range if any; otherwise the distance to the far edge of the scene.
pub fn point_far_plane(light: &ResolvedLight, scene_center: Vec3, scene_radius: f32) -> f32 {
    if light.range > 0.0 {
        light.range.max(0.5)
    } else {
        let to_center = (scene_center - light.position).length();
        (to_center + scene_radius * 1.5).max(0.5)
    }
}

/// The six perspective view-projections for an omnidirectional cubemap
/// shadow render. Face order matches the OpenGL cubemap target order
/// (`POSITIVE_X`, `NEGATIVE_X`, `POSITIVE_Y`, `NEGATIVE_Y`, `POSITIVE_Z`,
/// `NEGATIVE_Z`).
pub fn cube_face_view_projs(pos: Vec3, far: f32) -> [Mat4; 6] {
    let near = 0.05;
    let proj = Mat4::perspective_rh_gl(std::f32::consts::FRAC_PI_2, 1.0, near, far);
    // Glam's `look_at_rh` follows GL conventions; the cubemap face look-
    // direction / up-vector pairs are the standard set used by Lengyel /
    // the GL spec, adjusted to the right-handed convention.
    let dirs_ups: [(Vec3, Vec3); 6] = [
        (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, -1.0, 0.0)),
        (Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, -1.0, 0.0)),
        (Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0)),
        (Vec3::new(0.0, -1.0, 0.0), Vec3::new(0.0, 0.0, -1.0)),
        (Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, -1.0, 0.0)),
        (Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, -1.0, 0.0)),
    ];
    let mut out = [Mat4::IDENTITY; 6];
    for (i, (d, up)) in dirs_ups.iter().enumerate() {
        let view = Mat4::look_at_rh(pos, pos + *d, *up);
        out[i] = proj * view;
    }
    out
}

/// Pick a stable "up" vector for the light view that's never collinear with
/// the light direction. Falls back to world-Z when the light shines straight
/// up or down so `look_at_rh` doesn't degenerate.
fn pick_up_for(dir: Vec3) -> Vec3 {
    if dir.y.abs() > 0.999 {
        Vec3::Z
    } else {
        Vec3::Y
    }
}

/// GL state owned by the shadow pre-pass.
pub struct ShadowSystem {
    /// Active resolution. `0` when shadows are off.
    resolution: i32,
    /// 2D array depth texture used for directional + spot casters. One slice
    /// per caster, sized at [`MAX_SHADOW_2D`] regardless of quality so the
    /// sampler binding never has to swap when the user toggles quality.
    atlas_2d: Option<glow::Texture>,
    /// One FBO; the active draw slice is rebound per pass via
    /// `framebuffer_texture_layer`. Single FBO is fine because we never
    /// overlap the writes between casters.
    fbo_2d: Option<glow::Framebuffer>,
    /// One cubemap depth texture per cube caster slot. Sized at
    /// [`MAX_SHADOW_CUBE`] for the same reason as `atlas_2d`.
    cubes: [Option<glow::Texture>; MAX_SHADOW_CUBE],
    fbo_cube: Option<glow::Framebuffer>,
    /// Depth-only program reused across every caster pass. Re-uses the main
    /// VBO format and skinning palette path.
    program: glow::Program,
    u_dir_viewproj: Option<glow::UniformLocation>,
    u_dir_joint_mats: Option<glow::UniformLocation>,
    /// Point program. Same VS as directional but its FS writes linear
    /// world-space distance to `gl_FragDepth` so the cubemap stores radial
    /// depth that the main FS can compare against directly.
    point_program: glow::Program,
    u_point_viewproj: Option<glow::UniformLocation>,
    u_point_joint_mats: Option<glow::UniformLocation>,
    u_point_light_pos: Option<glow::UniformLocation>,
    u_point_far: Option<glow::UniformLocation>,
    /// Reusable flatten buffer for the joint-mat palette uniform upload.
    /// One per shadow system because the depth pass runs before the main
    /// pass — sharing with the main renderer's scratch would be fine but
    /// adds a borrow-checker annoyance for no win.
    palette_scratch: Vec<f32>,
}

impl ShadowSystem {
    pub fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        unsafe {
            let program = compile_program(gl, SHADOW_DIR_VS, SHADOW_DIR_FS)?;
            let u_dir_viewproj = gl.get_uniform_location(program, "u_light_viewproj");
            let u_dir_joint_mats = gl.get_uniform_location(program, "u_joint_mats[0]");

            let point_program = compile_program(gl, SHADOW_POINT_VS, SHADOW_POINT_FS)?;
            let u_point_viewproj = gl.get_uniform_location(point_program, "u_light_viewproj");
            let u_point_joint_mats = gl.get_uniform_location(point_program, "u_joint_mats[0]");
            let u_point_light_pos = gl.get_uniform_location(point_program, "u_light_pos");
            let u_point_far = gl.get_uniform_location(point_program, "u_far_plane");

            Ok(Self {
                resolution: 0,
                atlas_2d: None,
                fbo_2d: None,
                cubes: [None; MAX_SHADOW_CUBE],
                fbo_cube: None,
                program,
                u_dir_viewproj,
                u_dir_joint_mats,
                point_program,
                u_point_viewproj,
                u_point_joint_mats,
                u_point_light_pos,
                u_point_far,
                palette_scratch: Vec::new(),
            })
        }
    }

    /// Allocate (or reallocate) the depth textures for the given resolution.
    /// `0` releases existing GPU resources and disables further passes —
    /// matches the [`ShadowQuality::Off`] state.
    pub fn set_resolution(&mut self, gl: &glow::Context, resolution: i32) {
        if resolution == self.resolution {
            return;
        }
        unsafe {
            self.release_textures(gl);
            self.resolution = resolution;
            if resolution <= 0 {
                return;
            }
            // 2D array depth texture.
            let tex = match gl.create_texture() {
                Ok(t) => t,
                Err(_) => return,
            };
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(tex));
            gl.tex_image_3d(
                glow::TEXTURE_2D_ARRAY,
                0,
                glow::DEPTH_COMPONENT24 as i32,
                resolution,
                resolution,
                MAX_SHADOW_2D as i32,
                0,
                glow::DEPTH_COMPONENT,
                glow::FLOAT,
                None,
            );
            // Hardware PCF: tell the sampler to compare R against the depth
            // texel instead of fetching it raw, and to filter linearly across
            // the 2x2 neighbourhood. The FS reads through `sampler2DArray
            // Shadow` to get one free PCF tap per `texture()` call.
            gl.tex_parameter_i32(
                glow::TEXTURE_2D_ARRAY,
                glow::TEXTURE_COMPARE_MODE,
                glow::COMPARE_REF_TO_TEXTURE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D_ARRAY,
                glow::TEXTURE_COMPARE_FUNC,
                glow::LEQUAL as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D_ARRAY,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D_ARRAY,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            // Sampling outside the slice should report "no shadow" — clamp
            // to white-ish edge with `CLAMP_TO_BORDER` and a 1.0 border.
            gl.tex_parameter_i32(
                glow::TEXTURE_2D_ARRAY,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_BORDER as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D_ARRAY,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_BORDER as i32,
            );
            gl.tex_parameter_f32_slice(
                glow::TEXTURE_2D_ARRAY,
                glow::TEXTURE_BORDER_COLOR,
                &[1.0, 1.0, 1.0, 1.0],
            );
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, None);
            self.atlas_2d = Some(tex);

            let fbo = match gl.create_framebuffer() {
                Ok(f) => f,
                Err(_) => return,
            };
            self.fbo_2d = Some(fbo);

            // Cubemaps. One per slot; layered rendering via
            // `framebuffer_texture` would need GL 4.x or a geometry shader,
            // so we do six face-render passes per caster the simple way.
            for slot in 0..MAX_SHADOW_CUBE {
                let cube = match gl.create_texture() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                gl.bind_texture(glow::TEXTURE_CUBE_MAP, Some(cube));
                for face in 0..6 {
                    gl.tex_image_2d(
                        glow::TEXTURE_CUBE_MAP_POSITIVE_X + face as u32,
                        0,
                        glow::DEPTH_COMPONENT24 as i32,
                        resolution,
                        resolution,
                        0,
                        glow::DEPTH_COMPONENT,
                        glow::FLOAT,
                        None,
                    );
                }
                gl.tex_parameter_i32(
                    glow::TEXTURE_CUBE_MAP,
                    glow::TEXTURE_COMPARE_MODE,
                    glow::COMPARE_REF_TO_TEXTURE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_CUBE_MAP,
                    glow::TEXTURE_COMPARE_FUNC,
                    glow::LEQUAL as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_CUBE_MAP,
                    glow::TEXTURE_MIN_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_CUBE_MAP,
                    glow::TEXTURE_MAG_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_CUBE_MAP,
                    glow::TEXTURE_WRAP_S,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_CUBE_MAP,
                    glow::TEXTURE_WRAP_T,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_CUBE_MAP,
                    glow::TEXTURE_WRAP_R,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.bind_texture(glow::TEXTURE_CUBE_MAP, None);
                self.cubes[slot] = Some(cube);
            }
            if self.fbo_cube.is_none() {
                if let Ok(f) = gl.create_framebuffer() {
                    self.fbo_cube = Some(f);
                }
            }
        }
    }

    /// Release the depth textures + FBOs without dropping the programs.
    /// Called on `set_resolution(0)` and on context destroy.
    unsafe fn release_textures(&mut self, gl: &glow::Context) {
        if let Some(t) = self.atlas_2d.take() {
            gl.delete_texture(t);
        }
        if let Some(f) = self.fbo_2d.take() {
            gl.delete_framebuffer(f);
        }
        for slot in 0..MAX_SHADOW_CUBE {
            if let Some(t) = self.cubes[slot].take() {
                gl.delete_texture(t);
            }
        }
        if let Some(f) = self.fbo_cube.take() {
            gl.delete_framebuffer(f);
        }
    }

    pub fn destroy(&mut self, gl: &glow::Context) {
        unsafe {
            self.release_textures(gl);
            gl.delete_program(self.program);
            gl.delete_program(self.point_program);
        }
    }

    /// Render the depth pre-pass for every caster in `frame`. Pass `vao` /
    /// `mesh` from the main renderer so we re-use the same VBO / EBO bind.
    /// Caller is responsible for restoring the bound FBO + viewport
    /// afterwards — this function leaves the default framebuffer bound and
    /// resets the viewport back to `(viewport_x, viewport_y, viewport_w,
    /// viewport_h)` before returning.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        gl: &glow::Context,
        frame: &ShadowFrame,
        vao: glow::VertexArray,
        batches: &[DrawBatch],
        palettes: &[SkinPalette],
        viewport_x: i32,
        viewport_y: i32,
        viewport_w: i32,
        viewport_h: i32,
    ) {
        if self.resolution <= 0
            || self.atlas_2d.is_none()
            || self.fbo_2d.is_none()
            || (frame.casters_2d.is_empty() && frame.casters_cube.is_empty())
        {
            return;
        }

        unsafe {
            // Common state for every depth pass: depth test on with LESS,
            // depth-write on, no colour writes, front-face cull (peter-
            // panning mitigation), no scissor.
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::FRAMEBUFFER_SRGB);
            gl.disable(glow::BLEND);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.depth_mask(true);
            gl.color_mask(false, false, false, false);
            gl.enable(glow::CULL_FACE);
            gl.cull_face(glow::FRONT);
            gl.front_face(glow::CCW);
            // Slope-scaled polygon offset to push depth values away from
            // the camera, masking acne on near-tangent surfaces. Constants
            // tuned for the 512–2048 maps the quality presets allocate; a
            // smaller bias keeps contact shadows from drifting away from the
            // caster's feet.
            gl.enable(glow::POLYGON_OFFSET_FILL);
            gl.polygon_offset(1.5, 2.5);
            gl.viewport(0, 0, self.resolution, self.resolution);
            gl.bind_vertex_array(Some(vao));

            // ---- 2D casters ----
            if !frame.casters_2d.is_empty() {
                if let (Some(tex), Some(fbo)) = (self.atlas_2d, self.fbo_2d) {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                    gl.use_program(Some(self.program));
                    let u_joint_mats = self.u_dir_joint_mats.clone();
                    let u_viewproj = self.u_dir_viewproj.clone();
                    for (slice, caster) in frame.casters_2d.iter().enumerate() {
                        gl.framebuffer_texture_layer(
                            glow::FRAMEBUFFER,
                            glow::DEPTH_ATTACHMENT,
                            Some(tex),
                            0,
                            slice as i32,
                        );
                        // No colour attachment — explicit DRAW_BUFFER NONE
                        // so drivers don't complain on framebuffer-status
                        // check on stricter implementations.
                        gl.draw_buffer(glow::NONE);
                        gl.read_buffer(glow::NONE);
                        gl.clear_depth_f32(1.0);
                        gl.clear(glow::DEPTH_BUFFER_BIT);
                        let vp = match caster {
                            ShadowCaster::Directional { view_proj, .. }
                            | ShadowCaster::Spot { view_proj, .. } => *view_proj,
                            // Point casters never land in casters_2d, but
                            // be defensive.
                            ShadowCaster::Point { .. } => Mat4::IDENTITY,
                        };
                        if let Some(loc) = &u_viewproj {
                            gl.uniform_matrix_4_f32_slice(
                                Some(loc),
                                false,
                                &vp.to_cols_array(),
                            );
                        }
                        let frustum = FrustumPlanes::from_view_proj(vp);
                        self.draw_opaque_with_palettes(
                            gl,
                            batches,
                            palettes,
                            u_joint_mats.as_ref(),
                            &frustum,
                        );
                    }
                }
            }

            // ---- Cube casters ----
            if !frame.casters_cube.is_empty() {
                if let Some(fbo) = self.fbo_cube {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                    gl.use_program(Some(self.point_program));
                    let u_joint_mats = self.u_point_joint_mats.clone();
                    let u_viewproj = self.u_point_viewproj.clone();
                    let u_light_pos = self.u_point_light_pos.clone();
                    let u_far = self.u_point_far.clone();
                    for (slot, caster) in frame.casters_cube.iter().enumerate() {
                        let Some(cube) = self.cubes.get(slot).copied().flatten() else {
                            continue;
                        };
                        let ShadowCaster::Point {
                            face_view_projs,
                            position,
                            far_plane,
                            ..
                        } = caster
                        else {
                            continue;
                        };
                        if let Some(loc) = &u_light_pos {
                            gl.uniform_3_f32(Some(loc), position.x, position.y, position.z);
                        }
                        if let Some(loc) = &u_far {
                            gl.uniform_1_f32(Some(loc), *far_plane);
                        }
                        for face in 0..6 {
                            gl.framebuffer_texture_2d(
                                glow::FRAMEBUFFER,
                                glow::DEPTH_ATTACHMENT,
                                glow::TEXTURE_CUBE_MAP_POSITIVE_X + face as u32,
                                Some(cube),
                                0,
                            );
                            gl.draw_buffer(glow::NONE);
                            gl.read_buffer(glow::NONE);
                            // Clear to depth=1.0 (fully lit) so a face whose
                            // 90° frustum contains no batches reads as "no
                            // shadow" in the main FS — covers the case where
                            // every batch culls out below.
                            gl.clear_depth_f32(1.0);
                            gl.clear(glow::DEPTH_BUFFER_BIT);
                            if let Some(loc) = &u_viewproj {
                                gl.uniform_matrix_4_f32_slice(
                                    Some(loc),
                                    false,
                                    &face_view_projs[face].to_cols_array(),
                                );
                            }
                            let frustum =
                                FrustumPlanes::from_view_proj(face_view_projs[face]);
                            self.draw_opaque_with_palettes(
                                gl,
                                batches,
                                palettes,
                                u_joint_mats.as_ref(),
                                &frustum,
                            );
                        }
                    }
                }
            }

            // Restore state for the main pass.
            gl.disable(glow::POLYGON_OFFSET_FILL);
            gl.color_mask(true, true, true, true);
            gl.cull_face(glow::BACK);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(viewport_x, viewport_y, viewport_w, viewport_h);
            gl.bind_vertex_array(None);
            gl.use_program(None);
        }
    }

    /// Bind the atlas + cubemap textures to the texture units the main FS
    /// expects (`unit_2d` for the 2D array, `unit_cube_base..unit_cube_base
    /// + MAX_SHADOW_CUBE` for the cubemap samplers). Safe to call even when
    /// the system is in the off state — emits an unbind so leftover units
    /// from a previous frame don't leak in.
    pub fn bind_for_main_pass(
        &self,
        gl: &glow::Context,
        unit_2d: u32,
        unit_cube_base: u32,
    ) {
        unsafe {
            gl.active_texture(glow::TEXTURE0 + unit_2d);
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, self.atlas_2d);
            for slot in 0..MAX_SHADOW_CUBE {
                gl.active_texture(glow::TEXTURE0 + unit_cube_base + slot as u32);
                gl.bind_texture(glow::TEXTURE_CUBE_MAP, self.cubes[slot]);
            }
        }
    }

    pub fn unbind(&self, gl: &glow::Context, unit_2d: u32, unit_cube_base: u32) {
        unsafe {
            gl.active_texture(glow::TEXTURE0 + unit_2d);
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, None);
            for slot in 0..MAX_SHADOW_CUBE {
                gl.active_texture(glow::TEXTURE0 + unit_cube_base + slot as u32);
                gl.bind_texture(glow::TEXTURE_CUBE_MAP, None);
            }
        }
    }

    /// Draw every opaque batch in `mesh`, switching the joint-mat palette
    /// uniform per-batch via `u_joint_mats`. Mirrors the main pass's
    /// material-skip loop minus the colour state — depth-only, no PBR
    /// uniforms or texture binds. `frustum` culls batches whose bounding
    /// sphere lies fully outside the caster's frustum, saving both CPU
    /// submission and GPU vertex work. Skinned batches inflate their cull
    /// radius at flatten time so the test stays conservative under the
    /// runtime pose.
    fn draw_opaque_with_palettes(
        &mut self,
        gl: &glow::Context,
        batches: &[DrawBatch],
        palettes: &[SkinPalette],
        u_joint_mats: Option<&glow::UniformLocation>,
        frustum: &FrustumPlanes,
    ) {
        use mogen_core::AlphaMode;
        unsafe {
            let mut current_palette: Option<u32> = None;
            for b in batches {
                if matches!(b.alpha_mode, AlphaMode::Blend) || b.transmission > 0.0 {
                    continue;
                }
                if !frustum.sphere_visible(b.centroid, b.radius) {
                    continue;
                }
                if current_palette != Some(b.palette_id) {
                    if let Some(loc) = u_joint_mats {
                        let Some(palette) = palettes.get(b.palette_id as usize) else {
                            continue;
                        };
                        let n = palette.joint_matrices.len().min(MAX_JOINTS);
                        if n > 0 {
                            self.palette_scratch.clear();
                            self.palette_scratch.reserve(n * 16);
                            for m in &palette.joint_matrices[..n] {
                                self.palette_scratch
                                    .extend_from_slice(&m.to_cols_array());
                            }
                            gl.uniform_matrix_4_f32_slice(
                                Some(loc),
                                false,
                                &self.palette_scratch,
                            );
                        }
                    }
                    current_palette = Some(b.palette_id);
                }
                let byte_offset =
                    (b.index_start as i32) * std::mem::size_of::<u32>() as i32;
                gl.draw_elements(
                    glow::TRIANGLES,
                    b.index_count as i32,
                    glow::UNSIGNED_INT,
                    byte_offset,
                );
            }
        }
    }
}

