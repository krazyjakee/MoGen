//! Path sweep: extrude a 2D profile along a 3D Catmull–Rom path. Generalises
//! [`spline_tube_mesh`] (which always uses a circular cross-section) and
//! [`spline_ribbon_mesh`] (which uses a width-driven flat strip) to arbitrary
//! closed profiles.
//!
//! The profile lives in the local XY plane of each frame; X spans the
//! profile's "width" axis, Y its "height" axis. The path's tangent (T) becomes
//! the local +Z, the parallel-transported normal becomes local +X, and the
//! binormal becomes local +Y. This matches the convention used by
//! `spline_tube` for its cross-section so the two primitives compose
//! cleanly.
//!
//! Per-sample roll and scale modulators let one declaration span e.g. a
//! tapered pipe with an opening flare or a moulding that twists between
//! corners.

use std::f32::consts::TAU;

use glam::Vec3;
use mogen_core::{Mesh, UvMode};

use crate::cleanup::recompute_normals;

use super::extrude::Contour;

/// Per-sample modulation — independent vectors so callers can supply only
/// what they need. Unsupplied samples fall back to defaults (roll=0,
/// scale=1).
#[derive(Default, Clone)]
pub struct SweepModulation {
    /// Roll around the path tangent, in radians, at each sample. Linear
    /// interp between supplied samples.
    pub roll: Vec<f32>,
    /// Uniform 2D scale of the profile at each sample. 1.0 = no change.
    pub scale: Vec<f32>,
}

/// Sweep a closed `profile` (CCW in its local XY plane) along a Catmull–Rom
/// path through `points`. `samples_per_segment` subdivides each path
/// segment between control points; rib quads connect adjacent rings. Caps
/// triangulate the start and end profile via earcut (same path as
/// [`extrude_mesh`]).
///
/// `twist_radians` is a uniform total roll spread linearly across the path
/// (separate from per-sample `modulation.roll`, which is added on top).
pub fn sweep_mesh(
    profile: &[[f32; 2]],
    points: &[[f32; 3]],
    samples_per_segment: u32,
    twist_radians: f32,
    modulation: &SweepModulation,
    caps: bool,
    mode: UvMode,
) -> Mesh {
    if profile.len() < 3 || points.len() < 2 {
        return Mesh::default();
    }

    let samples = sample_catmull_rom(points, samples_per_segment);
    if samples.len() < 2 {
        return Mesh::default();
    }

    // Per-sample arc length along the centreline drives V in tile mode.
    let mut sample_arc: Vec<f32> = Vec::with_capacity(samples.len());
    sample_arc.push(0.0);
    for w in samples.windows(2) {
        let last = *sample_arc.last().unwrap();
        sample_arc.push(last + (w[1] - w[0]).length());
    }

    // Tangent and parallel-transported frame at each sample.
    let frames = build_path_frames(&samples);

    // Per-sample roll/scale modulation. Linear interp from a per-control-
    // point list, then add the global twist contribution.
    let total_samples = (samples.len() as f32 - 1.0).max(1.0);
    let roll_at = |i: usize| -> f32 {
        let base_global = twist_radians * (i as f32 / total_samples);
        let mod_roll = if modulation.roll.is_empty() {
            0.0
        } else {
            sample_per_sample(&modulation.roll, i, samples.len())
        };
        base_global + mod_roll
    };
    let scale_at = |i: usize| -> f32 {
        if modulation.scale.is_empty() {
            1.0
        } else {
            sample_per_sample(&modulation.scale, i, samples.len())
        }
    };

    // Profile arc length — drives U in tile mode so textures wrap evenly
    // around the perimeter regardless of profile vertex density.
    let profile_arc = closed_arc_lengths(profile);
    let profile_perimeter = *profile_arc.last().unwrap();

    let mut mesh = Mesh::default();
    let n = profile.len();

    // Build all rings.
    for (i, frame) in frames.iter().enumerate() {
        let s = scale_at(i);
        let r = roll_at(i);
        let (sin_r, cos_r) = r.sin_cos();
        let v = match mode {
            UvMode::Fit => i as f32 / total_samples,
            UvMode::Tile => sample_arc[i],
        };
        for (j, p) in profile.iter().enumerate() {
            // Apply roll first, then scale, then place into the path frame.
            let local_x = p[0] * cos_r - p[1] * sin_r;
            let local_y = p[0] * sin_r + p[1] * cos_r;
            let world = frame.center
                + frame.normal * (local_x * s)
                + frame.binormal * (local_y * s);
            mesh.positions.push([world.x, world.y, world.z]);
            mesh.normals.push([0.0, 0.0, 0.0]); // recomputed
            let u = match mode {
                UvMode::Fit => profile_arc[j] / profile_perimeter.max(1e-6),
                UvMode::Tile => profile_arc[j],
            };
            mesh.uvs.push([u, v]);
        }
        // Closing seam vertex (profile[0] duplicated with U = perimeter)
        // so the texture seam doesn't wrap-skip an edge.
        let p = profile[0];
        let local_x = p[0] * cos_r - p[1] * sin_r;
        let local_y = p[0] * sin_r + p[1] * cos_r;
        let world = frame.center
            + frame.normal * (local_x * s)
            + frame.binormal * (local_y * s);
        mesh.positions.push([world.x, world.y, world.z]);
        mesh.normals.push([0.0, 0.0, 0.0]);
        let u = match mode {
            UvMode::Fit => 1.0,
            UvMode::Tile => profile_perimeter,
        };
        mesh.uvs.push([u, v]);
    }

    // Quad strips between adjacent rings.
    let row = (n + 1) as u32;
    for i in 0..(samples.len() as u32 - 1) {
        for j in 0..n as u32 {
            let a = i * row + j;
            let b = a + 1;
            let c = a + row;
            let d = c + 1;
            mesh.indices.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }

    if caps {
        cap_at(&mut mesh, profile, &frames[0], roll_at(0), scale_at(0), false, mode);
        let last = frames.len() - 1;
        cap_at(&mut mesh, profile, &frames[last], roll_at(last), scale_at(last), true, mode);
    }

    recompute_normals(&mut mesh);
    mesh
}

/// Per-sample frame: tangent, parallel-transported normal, and binormal.
#[derive(Clone, Copy)]
struct Frame {
    center: Vec3,
    /// Tangent — local +Z of the cross-section.
    #[allow(dead_code)]
    tangent: Vec3,
    /// Parallel-transported in-plane axis — local +X of the cross-section.
    normal: Vec3,
    /// Cross of tangent × normal — local +Y of the cross-section.
    binormal: Vec3,
}

fn build_path_frames(samples: &[Vec3]) -> Vec<Frame> {
    let mut out = Vec::with_capacity(samples.len());
    if samples.is_empty() {
        return out;
    }
    let tangent_at = |i: usize| -> Vec3 {
        let a = if i == 0 { samples[0] } else { samples[i - 1] };
        let b = if i + 1 == samples.len() { samples[samples.len() - 1] } else { samples[i + 1] };
        (b - a).normalize_or(Vec3::Z)
    };
    let t0 = tangent_at(0);
    // Pick an initial normal orthogonal to the first tangent — prefer +Y
    // unless the path is nearly vertical at the start.
    let up = if t0.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
    let mut n_prev = up.cross(t0).cross(t0).normalize_or(Vec3::X * -1.0);
    if n_prev.length_squared() < 1e-8 {
        n_prev = Vec3::X;
    }
    let mut t_prev = t0;
    for (i, c) in samples.iter().enumerate() {
        let t_cur = tangent_at(i);
        let dot = t_prev.dot(t_cur).clamp(-1.0, 1.0);
        let n_cur = if dot > 0.9999 {
            n_prev
        } else {
            let axis = t_prev.cross(t_cur).normalize_or_zero();
            if axis.length_squared() < 1e-8 {
                n_prev
            } else {
                let angle = dot.acos();
                rotate_around_axis(n_prev, axis, angle).normalize_or(n_prev)
            }
        };
        let b_cur = t_cur.cross(n_cur).normalize_or(Vec3::Y);
        let n_cur = b_cur.cross(t_cur).normalize_or(n_cur);
        out.push(Frame { center: *c, tangent: t_cur, normal: n_cur, binormal: b_cur });
        n_prev = n_cur;
        t_prev = t_cur;
    }
    out
}

fn cap_at(
    mesh: &mut Mesh,
    profile: &[[f32; 2]],
    frame: &Frame,
    roll: f32,
    scale: f32,
    flip_winding: bool,
    mode: UvMode,
) {
    let base = mesh.positions.len() as u32;
    let (sin_r, cos_r) = roll.sin_cos();

    // Bound the profile for fit-mode UVs.
    let (mut min_x, mut max_x) = (f32::INFINITY, f32::NEG_INFINITY);
    let (mut min_y, mut max_y) = (f32::INFINITY, f32::NEG_INFINITY);
    for p in profile {
        if p[0] < min_x { min_x = p[0]; }
        if p[0] > max_x { max_x = p[0]; }
        if p[1] < min_y { min_y = p[1]; }
        if p[1] > max_y { max_y = p[1]; }
    }
    let span_x = (max_x - min_x).max(1e-6);
    let span_y = (max_y - min_y).max(1e-6);

    for p in profile {
        let local_x = p[0] * cos_r - p[1] * sin_r;
        let local_y = p[0] * sin_r + p[1] * cos_r;
        let world = frame.center
            + frame.normal * (local_x * scale)
            + frame.binormal * (local_y * scale);
        mesh.positions.push([world.x, world.y, world.z]);
        mesh.normals.push([0.0, 0.0, 0.0]);
        let uv = match mode {
            UvMode::Fit => [
                (p[0] - min_x) / span_x,
                (p[1] - min_y) / span_y,
            ],
            UvMode::Tile => [p[0], p[1]],
        };
        mesh.uvs.push(uv);
    }

    // Earcut on the profile (no holes for caps in v1).
    let mut flat: Vec<f32> = Vec::with_capacity(profile.len() * 2);
    for p in profile {
        flat.push(p[0]);
        flat.push(p[1]);
    }
    if let Ok(tri) = earcutr::earcut(&flat, &[], 2) {
        for c in tri.chunks(3) {
            let a = base + c[0] as u32;
            let b = base + c[1] as u32;
            let d = base + c[2] as u32;
            if flip_winding {
                mesh.indices.extend_from_slice(&[a, d, b]);
            } else {
                mesh.indices.extend_from_slice(&[a, b, d]);
            }
        }
    }
}

fn closed_arc_lengths(profile: &[[f32; 2]]) -> Vec<f32> {
    let mut arc = vec![0.0_f32];
    for w in profile.windows(2) {
        let last = *arc.last().unwrap();
        let dx = w[1][0] - w[0][0];
        let dy = w[1][1] - w[0][1];
        arc.push(last + (dx * dx + dy * dy).sqrt());
    }
    let last = *arc.last().unwrap();
    let dx = profile[0][0] - profile[profile.len() - 1][0];
    let dy = profile[0][1] - profile[profile.len() - 1][1];
    arc.push(last + (dx * dx + dy * dy).sqrt());
    arc
}

/// Linear-interp a `len`-sized per-control-point list to a per-sample value.
fn sample_per_sample(values: &[f32], sample_idx: usize, sample_count: usize) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    if values.len() == 1 || sample_count <= 1 {
        return values[0];
    }
    let total = sample_count as f32 - 1.0;
    let pos = (sample_idx as f32 / total) * (values.len() as f32 - 1.0);
    let i = pos.floor().min(values.len() as f32 - 2.0).max(0.0) as usize;
    let t = (pos - i as f32).clamp(0.0, 1.0);
    values[i] * (1.0 - t) + values[i + 1] * t
}

fn sample_catmull_rom(points: &[[f32; 3]], samples_per_segment: u32) -> Vec<Vec3> {
    let n = points.len();
    if n < 2 {
        return points.iter().map(|p| Vec3::from_array(*p)).collect();
    }
    let samples_per_segment = samples_per_segment.max(1);
    let mut out = Vec::with_capacity(n * samples_per_segment as usize + 1);
    let get = |i: isize| -> Vec3 {
        let j = i.clamp(0, n as isize - 1) as usize;
        Vec3::from_array(points[j])
    };
    for seg in 0..n - 1 {
        let p0 = get(seg as isize - 1);
        let p1 = get(seg as isize);
        let p2 = get(seg as isize + 1);
        let p3 = get(seg as isize + 2);
        let steps = if seg == n - 2 { samples_per_segment + 1 } else { samples_per_segment };
        for s in 0..steps {
            let t = s as f32 / samples_per_segment as f32;
            let t2 = t * t;
            let t3 = t2 * t;
            let a = -0.5 * t3 + t2 - 0.5 * t;
            let b = 1.5 * t3 - 2.5 * t2 + 1.0;
            let c = -1.5 * t3 + 2.0 * t2 + 0.5 * t;
            let d = 0.5 * t3 - 0.5 * t2;
            out.push(p0 * a + p1 * b + p2 * c + p3 * d);
        }
    }
    out
}

#[inline]
fn rotate_around_axis(v: Vec3, axis: Vec3, angle: f32) -> Vec3 {
    let (s, c) = angle.sin_cos();
    v * c + axis.cross(v) * s + axis * axis.dot(v) * (1.0 - c)
}

// Allow one unused import for `TAU` to coexist with the optional roll
// modulation API surface.
#[allow(dead_code)]
const _TAU_UNUSED: f32 = TAU;

// `Contour` is reserved for future per-sample profile holes; expose the
// alias for symmetry with [`extrude::Contour`].
pub type SweepProfile = Contour;

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn unit_square_profile() -> Vec<[f32; 2]> {
        vec![[-0.1, -0.1], [0.1, -0.1], [0.1, 0.1], [-0.1, 0.1]]
    }

    #[test]
    fn sweeps_along_straight_path() {
        // Path runs along +X from -1 to +1. Profile is a small XY square.
        let path = vec![[-1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        let mesh = sweep_mesh(
            &unit_square_profile(),
            &path,
            8,
            0.0,
            &SweepModulation::default(),
            true,
            UvMode::default(),
        );
        // Output should span ±1 along X (path), ±0.1 along Y/Z (profile in
        // the local frame — Y is +up, Z is path tangent's perpendicular).
        let (min, max) = aabb(&mesh.positions);
        assert!((min[0] + 1.0).abs() < 1e-3, "min X off (got {})", min[0]);
        assert!((max[0] - 1.0).abs() < 1e-3, "max X off (got {})", max[0]);
        // At least one cap and rib triangle.
        assert!(!mesh.indices.is_empty());
    }

    #[test]
    fn taper_via_scale_modulation_shrinks_far_end() {
        let path = vec![[-1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        let modulation = SweepModulation { roll: vec![], scale: vec![1.0, 0.5, 0.1] };
        let mesh = sweep_mesh(
            &unit_square_profile(),
            &path,
            8,
            0.0,
            &modulation,
            true,
            UvMode::default(),
        );
        // Scan: largest Y span at the path's start (X≈-1), smallest at the
        // end (X≈+1).
        let mut start_y_span = 0.0_f32;
        let mut end_y_span = 0.0_f32;
        for p in &mesh.positions {
            if (p[0] + 1.0).abs() < 0.05 && p[1].abs() > start_y_span {
                start_y_span = p[1].abs();
            }
            if (p[0] - 1.0).abs() < 0.05 && p[1].abs() > end_y_span {
                end_y_span = p[1].abs();
            }
        }
        assert!(start_y_span > end_y_span * 5.0,
            "scale modulation should shrink the far end (start={start_y_span}, end={end_y_span})");
    }

    #[test]
    fn closed_path_via_two_segments_produces_curve() {
        // L-shape — sweep should follow it without snapping.
        let path = vec![
            [-1.0, 0.0, 0.0],
            [0.0, 0.0, -0.5],
            [1.0, 0.0, 0.0],
        ];
        let mesh = sweep_mesh(
            &unit_square_profile(),
            &path,
            16,
            PI * 0.0,
            &SweepModulation::default(),
            true,
            UvMode::default(),
        );
        let (min, _max) = aabb(&mesh.positions);
        // The path dips to z=-0.5 in the middle so the swept volume must
        // reach negative Z below ~-0.4.
        assert!(min[2] < -0.3, "swept mesh should follow L-path into -Z (got min z={})", min[2]);
    }

    fn aabb(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for p in positions {
            for i in 0..3 {
                if p[i] < min[i] { min[i] = p[i]; }
                if p[i] > max[i] { max[i] = p[i]; }
            }
        }
        (min, max)
    }
}
