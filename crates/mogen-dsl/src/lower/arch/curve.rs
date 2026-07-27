//! Curved walls: a single circular arc described by a sagitta.
//!
//! Ported from pascalorg/editor's `packages/core/src/systems/wall/wall-curve.ts`
//! (MIT). The formulas are kept **verbatim**, including their sign conventions,
//! rather than being rewritten in terms of our [`plan::left_normal`]. That is
//! deliberate: their 2D frame is a y-up plot, ours is `[x, z]` under a
//! right-handed +Y-up system, so "left" inverts between them. Rewriting the
//! maths by reasoning about sides is exactly how you end up mirroring every
//! curved wall — and a mirrored arc is still a perfectly valid arc, so nothing
//! catches it.
//!
//! So: this module thinks in their frame. Component 0 is their `x`, component 1
//! is their `y`, which is our `z`. Since that mapping is the identity on the
//! stored array, no conversion is actually needed at the boundary — but the
//! *interpretation* of a normal's sign is theirs, not ours.

use super::consts::{ARC_SEGMENTS, CURVE_EPSILON};
use super::ir::P2;

/// A point on the centreline plus the frame there.
#[derive(Clone, Copy, Debug)]
pub(super) struct CurveFrame {
    pub point: P2,
    /// Unit tangent, pointing from start toward end.
    pub tangent: P2,
}

/// The straight-line frame between two endpoints.
#[derive(Clone, Copy, Debug)]
pub(super) struct ChordFrame {
    pub start: P2,
    pub end: P2,
    pub midpoint: P2,
    pub tangent: P2,
    /// Their normal: `(-dy, dx)`. See the module note on why this is not
    /// [`plan::left_normal`].
    pub normal: P2,
    pub length: f32,
}

pub(super) fn chord_frame(start: P2, end: P2) -> ChordFrame {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let length = dx.hypot(dy);

    if length < CURVE_EPSILON {
        return ChordFrame {
            start,
            end,
            midpoint: start,
            tangent: [1.0, 0.0],
            normal: [0.0, 1.0],
            length: 0.0,
        };
    }

    ChordFrame {
        start,
        end,
        midpoint: [(start[0] + end[0]) * 0.5, (start[1] + end[1]) * 0.5],
        tangent: [dx / length, dy / length],
        normal: [-dy / length, dx / length],
        length,
    }
}

/// A sagitta larger than half the chord is geometrically impossible.
pub(super) fn max_curve_offset(chord_length: f32) -> f32 {
    chord_length * 0.5
}

/// Below this much bulge, treat the wall as straight. Their
/// `getWallStraightSnapOffset`.
pub(super) fn straight_snap_offset(chord_length: f32) -> f32 {
    0.03_f32.min(0.005_f32.max(chord_length * 0.005))
}

/// Clamp and dead-zone a raw sagitta, yielding `0.0` for anything that should
/// be treated as straight.
pub(super) fn normalise_curve_offset(chord_length: f32, offset: f32) -> f32 {
    let max = max_curve_offset(chord_length);
    if !max.is_finite() || max < CURVE_EPSILON {
        return 0.0;
    }
    let clamped = offset.clamp(-max, max);
    if clamped.abs() <= straight_snap_offset(chord_length) {
        0.0
    } else {
        clamped
    }
}

pub(super) fn is_curved(chord_length: f32, offset: f32) -> bool {
    normalise_curve_offset(chord_length, offset).abs() > CURVE_EPSILON
}

/// The circle an arc lies on.
#[derive(Clone, Copy, Debug)]
pub(super) struct ArcData {
    pub center: P2,
    pub radius: f32,
    pub start_angle: f32,
    /// Signed sweep from start to end.
    pub delta: f32,
    /// `+1` or `-1`, the sign of the sagitta.
    pub direction: f32,
}

/// Build the arc for a wall, or `None` when it is straight.
///
/// Note where the centre goes: `midpoint + normal * centerOffset * direction`.
/// The arc bulges *opposite* the centre, so a **positive sagitta bulges along
/// `-normal`** — to the right of `start → end` in our frame.
pub(super) fn arc_data(start: P2, end: P2, offset: f32) -> Option<ArcData> {
    let chord = chord_frame(start, end);
    let sagitta = normalise_curve_offset(chord.length, offset);

    if sagitta.abs() <= CURVE_EPSILON || chord.length < CURVE_EPSILON {
        return None;
    }

    let abs_sagitta = sagitta.abs();
    let radius = (chord.length * chord.length) / (8.0 * abs_sagitta) + abs_sagitta * 0.5;
    let center_offset = radius - abs_sagitta;
    let direction = if sagitta < 0.0 { -1.0 } else { 1.0 };
    let center = [
        chord.midpoint[0] + chord.normal[0] * center_offset * direction,
        chord.midpoint[1] + chord.normal[1] * center_offset * direction,
    ];

    let start_angle = (chord.start[1] - center[1]).atan2(chord.start[0] - center[0]);
    let end_angle = (chord.end[1] - center[1]).atan2(chord.end[0] - center[0]);

    let mut delta = end_angle - start_angle;
    let tau = std::f32::consts::TAU;
    if direction > 0.0 {
        while delta <= 0.0 {
            delta += tau;
        }
    } else {
        while delta >= 0.0 {
            delta -= tau;
        }
    }

    Some(ArcData { center, radius, start_angle, delta, direction })
}

/// The frame at parameter `t ∈ [0, 1]` along a wall's centreline.
///
/// The tangent here is **analytic**. Junction mitring must use this rather than
/// the direction of the first or last sampled segment: a curved wall and its
/// straight neighbour would otherwise mitre from slightly different directions
/// and leave a crack that no amount of snapping closes.
pub(super) fn frame_at(start: P2, end: P2, offset: f32, t: f32) -> CurveFrame {
    let chord = chord_frame(start, end);
    let t = t.clamp(0.0, 1.0);

    let Some(arc) = arc_data(start, end, offset) else {
        return CurveFrame {
            point: [
                chord.start[0] + (chord.end[0] - chord.start[0]) * t,
                chord.start[1] + (chord.end[1] - chord.start[1]) * t,
            ],
            tangent: chord.tangent,
        };
    };

    let angle = arc.start_angle + arc.delta * t;
    let point = [
        arc.center[0] + angle.cos() * arc.radius,
        arc.center[1] + angle.sin() * arc.radius,
    ];
    let tangent = if arc.direction > 0.0 {
        [-angle.sin(), angle.cos()]
    } else {
        [angle.sin(), -angle.cos()]
    };

    CurveFrame { point, tangent }
}

/// Sample a centreline into a polyline. Straight walls give exactly the two
/// endpoints; curved walls give [`ARC_SEGMENTS`] + 1 points with the endpoints
/// exact.
pub(super) fn sample_centreline(start: P2, end: P2, offset: Option<f32>) -> Vec<P2> {
    let offset = offset.unwrap_or(0.0);
    let chord = chord_frame(start, end);
    if !is_curved(chord.length, offset) {
        return vec![start, end];
    }

    let mut out = Vec::with_capacity(ARC_SEGMENTS + 1);
    for i in 0..=ARC_SEGMENTS {
        let t = i as f32 / ARC_SEGMENTS as f32;
        out.push(frame_at(start, end, offset, t).point);
    }
    // Pin the endpoints so they match the unsampled values bit-for-bit; the
    // junction solver keys off these.
    out[0] = start;
    let last = out.len() - 1;
    out[last] = end;
    out
}

/// Arc length of a wall's centreline — what [`super::ir::Opening::along`] is
/// measured in.
pub(super) fn centreline_length(start: P2, end: P2, offset: Option<f32>) -> f32 {
    let offset = offset.unwrap_or(0.0);
    let chord = chord_frame(start, end);
    match arc_data(start, end, offset) {
        Some(arc) => arc.radius * arc.delta.abs(),
        None => chord.length,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-4;

    #[test]
    fn straight_wall_has_no_arc() {
        assert!(arc_data([0.0, 0.0], [4.0, 0.0], 0.0).is_none());
        assert_eq!(sample_centreline([0.0, 0.0], [4.0, 0.0], None).len(), 2);
    }

    #[test]
    fn tiny_bulge_snaps_to_straight() {
        // Below the dead zone, a wall is straight — otherwise dragging a wall
        // handle a fraction of a millimetre would silently curve it.
        let chord = 4.0;
        let tiny = straight_snap_offset(chord) * 0.5;
        assert_eq!(normalise_curve_offset(chord, tiny), 0.0);
        assert!(!is_curved(chord, tiny));
    }

    #[test]
    fn sagitta_is_clamped_to_half_the_chord() {
        let chord = 4.0;
        assert!((normalise_curve_offset(chord, 99.0) - 2.0).abs() < EPS);
        assert!((normalise_curve_offset(chord, -99.0) + 2.0).abs() < EPS);
    }

    #[test]
    fn positive_sagitta_bulges_right_of_start_to_end() {
        // The worked example from the audit of their getWallArcData, and the
        // single most important assertion in this module: guessing this sign
        // wrong mirrors every curved wall while leaving it valid geometry.
        //
        // start=(0,0) end=(1,0) sagitta=+0.1  =>  arc midpoint at (0.5, -0.1)
        let mid = frame_at([0.0, 0.0], [1.0, 0.0], 0.1, 0.5).point;
        assert!((mid[0] - 0.5).abs() < EPS, "got {mid:?}");
        assert!((mid[1] + 0.1).abs() < EPS, "expected z = -0.1, got {mid:?}");
    }

    #[test]
    fn negative_sagitta_mirrors_the_bulge() {
        let mid = frame_at([0.0, 0.0], [1.0, 0.0], -0.1, 0.5).point;
        assert!((mid[1] - 0.1).abs() < EPS, "expected z = +0.1, got {mid:?}");
    }

    #[test]
    fn arc_passes_exactly_through_its_endpoints() {
        let (a, b) = ([1.0, -2.0], [4.0, 3.0]);
        let s = frame_at(a, b, 0.4, 0.0).point;
        let e = frame_at(a, b, 0.4, 1.0).point;
        assert!((s[0] - a[0]).abs() < EPS && (s[1] - a[1]).abs() < EPS, "got {s:?}");
        assert!((e[0] - b[0]).abs() < EPS && (e[1] - b[1]).abs() < EPS, "got {e:?}");
    }

    #[test]
    fn sampled_endpoints_are_bit_identical_to_the_inputs() {
        // Junction grouping keys off these, so "very close" is not good enough.
        let (a, b) = ([1.5, -2.25], [4.75, 3.5]);
        let pts = sample_centreline(a, b, Some(0.5));
        assert_eq!(pts[0], a);
        assert_eq!(pts[pts.len() - 1], b);
        assert_eq!(pts.len(), ARC_SEGMENTS + 1);
    }

    #[test]
    fn analytic_tangent_is_perpendicular_to_the_radius() {
        let arc = arc_data([0.0, 0.0], [2.0, 0.0], 0.3).expect("curved");
        for i in 0..=8 {
            let t = i as f32 / 8.0;
            let f = frame_at([0.0, 0.0], [2.0, 0.0], 0.3, t);
            let radial = [f.point[0] - arc.center[0], f.point[1] - arc.center[1]];
            let d = radial[0] * f.tangent[0] + radial[1] * f.tangent[1];
            assert!(d.abs() < 1e-3, "tangent not perpendicular at t={t}: {d}");
        }
    }

    #[test]
    fn tangent_points_from_start_toward_end() {
        let f = frame_at([0.0, 0.0], [1.0, 0.0], 0.1, 0.5);
        // Bulging right, the midpoint tangent should still head broadly +x.
        assert!(f.tangent[0] > 0.0, "got {:?}", f.tangent);
    }

    #[test]
    fn curved_centreline_is_longer_than_its_chord() {
        let straight = centreline_length([0.0, 0.0], [2.0, 0.0], None);
        let curved = centreline_length([0.0, 0.0], [2.0, 0.0], Some(0.4));
        assert!((straight - 2.0).abs() < EPS);
        assert!(curved > straight, "arc {curved} should exceed chord {straight}");
    }

    #[test]
    fn semicircle_length_matches_the_analytic_value() {
        // Sagitta == half the chord is a semicircle: length = π * radius.
        let chord = 2.0;
        let len = centreline_length([0.0, 0.0], [chord, 0.0], Some(chord * 0.5));
        let expected = std::f32::consts::PI * (chord * 0.5);
        assert!((len - expected).abs() < 1e-3, "got {len}, want {expected}");
    }
}
