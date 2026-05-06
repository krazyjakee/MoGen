//! Helical sweep — a circular cross-section swept along a helix.
//!
//! Given helix parameters (radius, height, turns) and a tube radius, generate
//! a dense sequence of centerline points and defer to `spline_tube_mesh` for
//! the actual sweep. Splitting the work this way keeps the coil-specific
//! logic short (the parametric formula plus a handedness flip) and reuses the
//! parallel-transport frame that already exists in `spline_tube_mesh`, so the
//! cross-section never flips on tightly-wound coils.

use std::f32::consts::TAU;

use mogen_core::{Mesh, UvMode};

use super::lathe::spline_tube_mesh;

/// Coil winding direction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Handedness {
    /// Counter-clockwise in the XZ plane viewed from +Y down — the standard
    /// "right-handed" thread. This is the default.
    Right,
    /// Clockwise — left-handed threads, the second strand of a paired DNA
    /// model, or anything that needs to mirror a `Right` coil.
    Left,
}

/// Build a helical tube of circular cross-section.
///
/// Centerline: `(R·cos θ, H · θ / (2π · turns), R·sin θ)` for
/// θ ∈ [0, 2π · turns]. The mesh sits with its lower endpoint at y=0 and
/// climbs to y=`height`, so it composes directly with default-anchored
/// cylinders.
///
/// `samples_per_turn` controls the density of the helix sampling. At low
/// values the parallel-transport frame inside `spline_tube_mesh` keeps the
/// cross-section orientation smooth, but the path itself reads as a
/// polygonal helix; bump it up for tight coils. `radial_segments` is the
/// cross-section sides count.
///
/// Returns an empty mesh when any of `radius` / `profile_radius` / `turns`
/// is non-positive, since none of the three has a sensible degenerate
/// geometry interpretation.
pub fn coil_mesh(
    radius: f32,
    height: f32,
    turns: f32,
    profile_radius: f32,
    radial_segments: u32,
    samples_per_turn: u32,
    cap_ends: bool,
    handedness: Handedness,
    mode: UvMode,
) -> Mesh {
    if turns <= 0.0 || radius <= 0.0 || profile_radius <= 0.0 {
        return Mesh::default();
    }
    let samples_per_turn = samples_per_turn.max(3);
    // Total path samples — `samples_per_turn` per revolution, plus one extra
    // so the final point lands exactly on the end of the helix (no
    // half-segment shortfall from float rounding on non-integer `turns`).
    let total_samples = ((samples_per_turn as f32 * turns).round() as u32 + 1).max(2);
    let dir = match handedness {
        Handedness::Right => 1.0_f32,
        Handedness::Left => -1.0_f32,
    };
    let theta_max = TAU * turns;
    let mut points: Vec<[f32; 3]> = Vec::with_capacity(total_samples as usize);
    for i in 0..total_samples {
        let t = i as f32 / (total_samples - 1) as f32;
        let theta = dir * theta_max * t;
        let y = height * t;
        points.push([radius * theta.cos(), y, radius * theta.sin()]);
    }
    spline_tube_mesh(
        &points,
        &[profile_radius],
        radial_segments,
        // Helix points already form a dense sample of the parametric curve —
        // we don't need `spline_tube_mesh` to subdivide them further. The
        // Catmull–Rom pass on this dense sample evaluates close to the
        // original helix, which is exactly what we want: smooth, not
        // polygonal, even at low `samples_per_turn`.
        1,
        cap_ends,
        mode,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coil_winds_through_full_revolution() {
        // A 1-turn coil should sweep all four cardinal X/Z signs, proving
        // the helix actually closes one revolution rather than aborting
        // early or drifting along a straight diagonal.
        let m = coil_mesh(
            0.5, 1.0, 1.0, 0.05,
            8, 16, false,
            Handedness::Right,
            UvMode::Fit,
        );
        assert!(!m.positions.is_empty(), "coil produced no vertices");
        let max_x = m.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
        let min_x = m.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
        let max_z = m.positions.iter().map(|p| p[2]).fold(f32::NEG_INFINITY, f32::max);
        let min_z = m.positions.iter().map(|p| p[2]).fold(f32::INFINITY, f32::min);
        // Centerline passes through ±radius on both axes; tube radius adds
        // up to 0.05 on top of that.
        assert!(max_x > 0.4, "missing +X sweep, got max_x={max_x}");
        assert!(min_x < -0.4, "missing -X sweep, got min_x={min_x}");
        assert!(max_z > 0.4, "missing +Z sweep, got max_z={max_z}");
        assert!(min_z < -0.4, "missing -Z sweep, got min_z={min_z}");
    }

    #[test]
    fn coil_height_spans_zero_to_height() {
        let h = 1.5;
        let m = coil_mesh(
            0.4, h, 3.0, 0.04,
            8, 16, false,
            Handedness::Right,
            UvMode::Fit,
        );
        let min_y = m.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        let max_y = m.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
        // Tube cross-section adds up to `profile_radius` of slack on each end
        // because the parallel-transport frame's normal can tilt the disc
        // slightly off-vertical at the endpoints. Use a generous tolerance.
        assert!(min_y.abs() < 0.1, "expected base near y=0, got {min_y}");
        assert!((max_y - h).abs() < 0.1, "expected top near y={h}, got {max_y}");
    }

    #[test]
    fn handedness_mirrors_z_sweep() {
        // At θ slightly past π/2 in a right-handed coil, the centerline is
        // around (cos θ, _, sin θ) with sin θ > 0. The left-handed coil
        // should hit sin θ < 0 at the same parameter. We use the second
        // sample as a stable witness.
        let r = coil_mesh(
            1.0, 0.0, 1.0, 0.05, 8, 16, false,
            Handedness::Right, UvMode::Fit,
        );
        let l = coil_mesh(
            1.0, 0.0, 1.0, 0.05, 8, 16, false,
            Handedness::Left, UvMode::Fit,
        );
        // Average sin θ (the Z component) over all centerline samples.
        // Right-handed coils should average positive over the first
        // half-turn; left-handed, negative. Average over the whole mesh
        // is dominated by the cross-section ring positions, which
        // distribute symmetrically about the centerline, so we average Z
        // over a sample where cos θ > 0 (front half) only.
        let pos_x_sum_r = r.positions.iter().filter(|p| p[0] > 0.5).map(|p| p[2]).sum::<f32>();
        let pos_x_sum_l = l.positions.iter().filter(|p| p[0] > 0.5).map(|p| p[2]).sum::<f32>();
        assert!(
            pos_x_sum_r * pos_x_sum_l < 0.0,
            "right and left coils should have opposite Z bias on the +X half: r={pos_x_sum_r}, l={pos_x_sum_l}",
        );
    }

    #[test]
    fn degenerate_inputs_return_empty_mesh() {
        // Zero turns, zero radius, zero profile radius — no sensible
        // geometry to build. Spline_tube_mesh would also panic on a
        // single point if we passed it raw, so guarding here is the
        // robust thing.
        let m = coil_mesh(0.5, 1.0, 0.0, 0.05, 8, 16, true, Handedness::Right, UvMode::Fit);
        assert!(m.positions.is_empty());
        let m = coil_mesh(0.0, 1.0, 1.0, 0.05, 8, 16, true, Handedness::Right, UvMode::Fit);
        assert!(m.positions.is_empty());
        let m = coil_mesh(0.5, 1.0, 1.0, 0.0, 8, 16, true, Handedness::Right, UvMode::Fit);
        assert!(m.positions.is_empty());
    }
}
