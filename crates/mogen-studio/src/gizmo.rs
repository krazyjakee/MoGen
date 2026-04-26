//! Viewport gizmo: hit-testing and drag math for the translate / rotate /
//! scale handles. The GL rendering of the handles lives in `viewer.rs`; this
//! module is pure math (no GL, no egui widgets) so it can be unit-tested.
//!
//! All handles are drawn in world-space axis-aligned orientation ("global"
//! mode) centred on the selected node's world translation. The handle
//! footprint is sized so it stays roughly constant in pixels regardless of
//! camera distance; we keep the chosen scale around and use it both for
//! rendering and for hit-testing.

use eframe::egui::{Pos2, Rect};
use glam::{Mat4, Vec3, Vec4};

/// Gizmo handle edit mode — T/R/S, exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GizmoMode {
    Translate,
    Rotate,
    Scale,
}

impl Default for GizmoMode {
    fn default() -> Self {
        GizmoMode::Translate
    }
}

/// Which axis the user grabbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    pub fn unit(self) -> Vec3 {
        match self {
            Axis::X => Vec3::X,
            Axis::Y => Vec3::Y,
            Axis::Z => Vec3::Z,
        }
    }

    pub const ALL: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];
}

/// Screen-space size target for the gizmo, in pixels from the origin to the
/// tip of each axis arrow. Tweaked so the handles stay easily grabbable on a
/// 1080p viewport without dominating the scene.
pub const GIZMO_PIXEL_RADIUS: f32 = 96.0;

/// Compute the world-space handle length that projects to roughly
/// [`GIZMO_PIXEL_RADIUS`] pixels at the gizmo origin. Call per-frame so the
/// gizmo re-scales as the user zooms.
pub fn handle_scale(origin: Vec3, camera_eye: Vec3, viewport_height: f32) -> f32 {
    let dist = (origin - camera_eye).length().max(0.001);
    let fov_y: f32 = 45.0_f32.to_radians();
    // `2 * dist * tan(fov/2)` is the world height of the view plane at `dist`.
    let world_per_pixel = 2.0 * dist * (fov_y * 0.5).tan() / viewport_height.max(1.0);
    GIZMO_PIXEL_RADIUS * world_per_pixel
}

/// Convert a screen position + viewport to an ndc + world ray, consistent
/// with the raycaster in [`crate::pick`]. Returned tuple is `(origin, dir)`.
pub fn screen_ray(
    viewproj: Mat4,
    camera_eye: Vec3,
    viewport: Rect,
    cursor: Pos2,
) -> (Vec3, Vec3) {
    let inv = viewproj.inverse();
    let u = (cursor.x - viewport.min.x) / viewport.width().max(1.0);
    let v = (cursor.y - viewport.min.y) / viewport.height().max(1.0);
    let ndc_x = u * 2.0 - 1.0;
    let ndc_y = 1.0 - v * 2.0;
    let far = inv * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
    let far = if far.w.abs() > 1e-6 {
        Vec3::new(far.x / far.w, far.y / far.w, far.z / far.w)
    } else {
        Vec3::new(far.x, far.y, far.z)
    };
    let dir = (far - camera_eye).normalize_or_zero();
    (camera_eye, dir)
}

/// Test a cursor against every gizmo handle and return the axis that was hit
/// (nearest to the camera) along with the parameter along the ray. Only
/// handles that match `mode` are considered.
pub fn hit_axis(
    mode: GizmoMode,
    origin: Vec3,
    scale: f32,
    ray_origin: Vec3,
    ray_dir: Vec3,
) -> Option<Axis> {
    let mut best: Option<(f32, Axis)> = None;
    for axis in Axis::ALL {
        let hit = match mode {
            GizmoMode::Translate => hit_translate_arm(origin, axis, scale, ray_origin, ray_dir),
            GizmoMode::Rotate => hit_rotate_ring(origin, axis, scale, ray_origin, ray_dir),
            GizmoMode::Scale => hit_scale_arm(origin, axis, scale, ray_origin, ray_dir),
        };
        if let Some(t) = hit {
            match best {
                Some((bt, _)) if bt <= t => {}
                _ => best = Some((t, axis)),
            }
        }
    }
    best.map(|(_, a)| a)
}

/// Translate handle: thin cylinder along the axis from `origin` to
/// `origin + axis * scale`. We test against the cylinder's infinite line and
/// gate by a distance threshold scaled with the handle size.
fn hit_translate_arm(
    origin: Vec3,
    axis: Axis,
    scale: f32,
    ro: Vec3,
    rd: Vec3,
) -> Option<f32> {
    let p0 = origin;
    let p1 = origin + axis.unit() * scale;
    let radius = scale * 0.08;
    hit_segment_cylinder(p0, p1, radius, ro, rd)
}

/// Scale handle: little cube sitting at the tip of each axis arrow. We
/// approximate with a small AABB centred on the tip.
fn hit_scale_arm(origin: Vec3, axis: Axis, scale: f32, ro: Vec3, rd: Vec3) -> Option<f32> {
    // First the shaft — same as translate, slightly thinner.
    let p0 = origin;
    let p1 = origin + axis.unit() * scale;
    let shaft_r = scale * 0.05;
    let shaft = hit_segment_cylinder(p0, p1, shaft_r, ro, rd);
    // Then the cube cap.
    let half = scale * 0.1;
    let tip = p1;
    let cube_min = tip - Vec3::splat(half);
    let cube_max = tip + Vec3::splat(half);
    let cube = intersect_aabb(ro, rd, cube_min, cube_max);
    match (shaft, cube) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// Rotate handle: a torus around `axis` with a modest tube radius. We
/// approximate by testing the ring's infinite-thin circle plus a tolerance:
/// the cursor ray hits the plane perpendicular to `axis`, we check the
/// distance from that hit to the ring.
fn hit_rotate_ring(
    origin: Vec3,
    axis: Axis,
    scale: f32,
    ro: Vec3,
    rd: Vec3,
) -> Option<f32> {
    let n = axis.unit();
    let denom = rd.dot(n);
    if denom.abs() < 1e-5 {
        return None; // ray parallel to the ring plane
    }
    let t = (origin - ro).dot(n) / denom;
    if t <= 1e-5 {
        return None;
    }
    let hit = ro + rd * t;
    let offset = hit - origin;
    // Distance from ring: |‖offset‖ - R| < tolerance.
    let r = offset.length();
    let ring_r = scale;
    let tol = scale * 0.08;
    if (r - ring_r).abs() <= tol {
        Some(t)
    } else {
        None
    }
}

/// Intersect a ray with a finite cylinder (capsule-less: just the infinite
/// shaft clipped to [p0, p1]). Fast enough for 3 handles.
fn hit_segment_cylinder(p0: Vec3, p1: Vec3, radius: f32, ro: Vec3, rd: Vec3) -> Option<f32> {
    let ca = (p1 - p0).normalize_or_zero();
    if ca.length_squared() < 1e-6 {
        return None;
    }
    let oc = ro - p0;
    let card = ca.dot(rd);
    let caoc = ca.dot(oc);
    let a = 1.0 - card * card;
    let b = oc.dot(rd) - caoc * card;
    let c = oc.dot(oc) - caoc * caoc - radius * radius;
    let disc = b * b - a * c;
    if disc < 0.0 || a.abs() < 1e-6 {
        return None;
    }
    let sq = disc.sqrt();
    let t = (-b - sq) / a;
    if t <= 1e-4 {
        return None;
    }
    // Check the hit lies within [p0, p1] along `ca`.
    let hit = ro + rd * t;
    let along = (hit - p0).dot(ca);
    let length = (p1 - p0).length();
    if along < 0.0 || along > length {
        return None;
    }
    Some(t)
}

fn intersect_aabb(ro: Vec3, rd: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let mut tmin = f32::NEG_INFINITY;
    let mut tmax = f32::INFINITY;
    for i in 0..3 {
        let o = match i { 0 => ro.x, 1 => ro.y, _ => ro.z };
        let d = match i { 0 => rd.x, 1 => rd.y, _ => rd.z };
        let lo = match i { 0 => min.x, 1 => min.y, _ => min.z };
        let hi = match i { 0 => max.x, 1 => max.y, _ => max.z };
        if d.abs() < 1e-8 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let t1 = (lo - o) / d;
        let t2 = (hi - o) / d;
        let (t1, t2) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
        tmin = tmin.max(t1);
        tmax = tmax.min(t2);
        if tmin > tmax {
            return None;
        }
    }
    if tmax < 0.0 { None } else { Some(tmin.max(0.0)) }
}

/// Signed delta to apply along `axis` given a translate drag. Projects the
/// start and current ray to the nearest point on the axis line (through
/// `origin`) and returns the axial component of their difference.
pub fn translate_delta(
    origin: Vec3,
    axis: Axis,
    start_ro: Vec3,
    start_rd: Vec3,
    cur_ro: Vec3,
    cur_rd: Vec3,
) -> f32 {
    let a = axis.unit();
    let start = closest_point_on_line_to_ray(origin, a, start_ro, start_rd);
    let cur = closest_point_on_line_to_ray(origin, a, cur_ro, cur_rd);
    (cur - start).dot(a)
}

/// Angle delta in radians from `start` → `cur` when rotating around `axis`
/// centred at `origin`. Uses the rays' intersection with the ring plane.
pub fn rotate_delta(
    origin: Vec3,
    axis: Axis,
    start_ro: Vec3,
    start_rd: Vec3,
    cur_ro: Vec3,
    cur_rd: Vec3,
) -> f32 {
    let n = axis.unit();
    let start = plane_hit(origin, n, start_ro, start_rd);
    let cur = plane_hit(origin, n, cur_ro, cur_rd);
    let (Some(s), Some(c)) = (start, cur) else {
        return 0.0;
    };
    let s = (s - origin).normalize_or_zero();
    let c = (c - origin).normalize_or_zero();
    if s.length_squared() < 1e-6 || c.length_squared() < 1e-6 {
        return 0.0;
    }
    let dot = s.dot(c).clamp(-1.0, 1.0);
    let cross = s.cross(c).dot(n);
    dot.acos() * cross.signum().copysign(1.0)
        * if cross < 0.0 { -1.0 } else { 1.0 }
}

/// Multiplicative scale factor along `axis` for a scale drag. Built from the
/// ratio of projected distances so near the gizmo origin we don't explode.
pub fn scale_factor(
    origin: Vec3,
    axis: Axis,
    start_ro: Vec3,
    start_rd: Vec3,
    cur_ro: Vec3,
    cur_rd: Vec3,
) -> f32 {
    let a = axis.unit();
    let s = (closest_point_on_line_to_ray(origin, a, start_ro, start_rd) - origin).dot(a);
    let c = (closest_point_on_line_to_ray(origin, a, cur_ro, cur_rd) - origin).dot(a);
    // Clamp denominator so the sign stays meaningful even when the start drag
    // was right at the origin.
    let s_safe = if s.abs() < 1e-4 { 1e-4 * s.signum().max(1.0) } else { s };
    (c / s_safe).max(0.05).min(20.0)
}

fn closest_point_on_line_to_ray(line_origin: Vec3, line_dir: Vec3, ro: Vec3, rd: Vec3) -> Vec3 {
    // Implements the standard two-line closest-point formula. Degenerate case
    // (parallel lines) falls back to projecting the ray origin onto the line.
    let w = line_origin - ro;
    let a = line_dir.dot(line_dir);
    let b = line_dir.dot(rd);
    let c = rd.dot(rd);
    let d = line_dir.dot(w);
    let e = rd.dot(w);
    let denom = a * c - b * b;
    if denom.abs() < 1e-6 {
        return line_origin + line_dir * (-d / a.max(1e-6));
    }
    let t_line = (b * e - c * d) / denom;
    line_origin + line_dir * t_line
}

fn plane_hit(origin: Vec3, normal: Vec3, ro: Vec3, rd: Vec3) -> Option<Vec3> {
    let denom = rd.dot(normal);
    if denom.abs() < 1e-5 {
        return None;
    }
    let t = (origin - ro).dot(normal) / denom;
    if t <= 1e-5 {
        return None;
    }
    Some(ro + rd * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_arm_hit_is_detected_along_each_axis() {
        let origin = Vec3::ZERO;
        let scale = 1.0;
        // Aim straight at a point halfway along +X (y=z=0) — the shaft runs
        // 0..1 on X with radius 0.08, so a ray through (0.5, 0, 0) hits it.
        let got = hit_axis(
            GizmoMode::Translate,
            origin,
            scale,
            Vec3::new(0.5, 0.0, -1.0),
            Vec3::new(0.0, 0.0, 1.0),
        );
        assert_eq!(got, Some(Axis::X));
    }

    #[test]
    fn translate_delta_along_x_is_positive_when_cursor_moves_right() {
        // Start: aim at +X arm at x=0. Cur: aim slightly farther in +X.
        let origin = Vec3::ZERO;
        let ro = Vec3::new(0.0, 0.2, -2.0);
        let start_rd = Vec3::new(0.0, 0.0, 1.0);
        let cur_rd = Vec3::new(0.2, 0.0, 1.0).normalize();
        let d = translate_delta(origin, Axis::X, ro, start_rd, ro, cur_rd);
        assert!(d > 0.0, "expected positive x drag delta, got {d}");
    }

    #[test]
    fn rotate_delta_is_zero_for_identical_rays() {
        let origin = Vec3::ZERO;
        let ro = Vec3::new(0.0, 0.2, -2.0);
        let rd = Vec3::new(0.0, 0.0, 1.0);
        let d = rotate_delta(origin, Axis::Y, ro, rd, ro, rd);
        assert!(d.abs() < 1e-4, "expected zero rotation, got {d}");
    }

    #[test]
    fn handle_scale_grows_with_distance() {
        let origin = Vec3::ZERO;
        let near = handle_scale(origin, Vec3::new(0.0, 0.0, 2.0), 720.0);
        let far = handle_scale(origin, Vec3::new(0.0, 0.0, 10.0), 720.0);
        assert!(far > near, "gizmo should get bigger in world units when farther");
    }
}
