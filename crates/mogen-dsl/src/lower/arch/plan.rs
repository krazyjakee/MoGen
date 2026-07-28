//! 2D geometry in the ground plane. No architecture here — just the kernel the
//! mitre solver, height solver and sinks all sit on.
//!
//! Everything works in `[x, z]`. There is one handedness convention and it is
//! stated once, in [`left_normal`], because getting it wrong mirrors geometry
//! silently: the result stays a perfectly valid shape, just the wrong one.

use super::consts::{COLLINEAR_EPS, CURVE_EPSILON, OVERLAP_EPS, SNAP_MM};
use super::ir::P2;

/// Quantise a coordinate onto the junction grid.
///
/// Snapping the *coordinates* — not merely a lookup key — is load-bearing.
/// Both walls at a junction then read bit-identical endpoints, so the mitre
/// points they each compute are bit-identical too and their polygons share
/// vertices exactly. Snapping only the key (as their editor does) leaves a
/// sub-millimetre crack between the two walls.
pub(super) fn quantise(v: f32) -> i32 {
    (v * SNAP_MM).round() as i32
}

pub(super) fn dequantise(q: i32) -> f32 {
    q as f32 / SNAP_MM
}

/// Snap a point onto the junction grid.
pub(super) fn snap(p: P2) -> P2 {
    [dequantise(quantise(p[0])), dequantise(quantise(p[1]))]
}

/// Integer grid key for a point, for grouping endpoints into junctions.
pub(super) fn key(p: P2) -> (i32, i32) {
    (quantise(p[0]), quantise(p[1]))
}

pub(super) fn sub(a: P2, b: P2) -> P2 {
    [a[0] - b[0], a[1] - b[1]]
}

pub(super) fn add(a: P2, b: P2) -> P2 {
    [a[0] + b[0], a[1] + b[1]]
}

pub(super) fn scale(a: P2, k: f32) -> P2 {
    [a[0] * k, a[1] * k]
}

pub(super) fn dot(a: P2, b: P2) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

/// 2D cross product — the z of the 3D cross. Zero when parallel.
pub(super) fn perp_dot(a: P2, b: P2) -> f32 {
    a[0] * b[1] - a[1] * b[0]
}

pub(super) fn length(a: P2) -> f32 {
    a[0].hypot(a[1])
}

pub(super) fn distance(a: P2, b: P2) -> f32 {
    length(sub(a, b))
}

/// Normalise, or `None` if the vector is too short to have a direction.
pub(super) fn normalise(a: P2) -> Option<P2> {
    let len = length(a);
    if len < f32::EPSILON {
        return None;
    }
    Some([a[0] / len, a[1] / len])
}

/// The left-hand normal of a direction, in our `[x, z]` right-handed +Y-up
/// frame: rotating `d` by +90° about +Y gives `[d.z, -d.x]`.
///
/// This differs in sign from the `(-dy, dx)` you would write for a 2D plot with
/// y up — which is exactly what pascalorg/editor uses. Ported formulas from
/// their code therefore keep *their* convention internally and convert once at
/// the boundary, rather than being rewritten in terms of this function.
pub(super) fn left_normal(d: P2) -> P2 {
    [d[1], -d[0]]
}

/// An infinite line as a point and a direction.
#[derive(Clone, Copy, Debug)]
pub(super) struct Line {
    pub origin: P2,
    pub dir: P2,
}

/// Intersect two infinite lines. `None` when they are parallel within
/// [`COLLINEAR_EPS`].
///
/// Infinite rather than segment intersection on purpose: two walls of different
/// thickness meeting at a corner have offset lines that only cross beyond the
/// segment ends, and that crossing is precisely the mitre point.
pub(super) fn line_line_intersect(a: Line, b: Line) -> Option<P2> {
    let denom = perp_dot(a.dir, b.dir);
    if denom.abs() < COLLINEAR_EPS {
        return None;
    }
    let diff = sub(b.origin, a.origin);
    let t = perp_dot(diff, b.dir) / denom;
    Some(add(a.origin, scale(a.dir, t)))
}

/// Twice the signed area of a ring. Positive is counter-clockwise in a standard
/// 2D plot; in our `[x, z]` frame the sign convention is whatever
/// [`normalise_ccw`] enforces, and only consistency matters.
pub(super) fn signed_area2(ring: &[P2]) -> f32 {
    if ring.len() < 3 {
        return 0.0;
    }
    let mut acc = 0.0;
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        acc += perp_dot(a, b);
    }
    acc
}

pub(super) fn area(ring: &[P2]) -> f32 {
    signed_area2(ring).abs() * 0.5
}

/// Force a ring counter-clockwise, in place.
pub(super) fn normalise_ccw(ring: &mut Vec<P2>) {
    if signed_area2(ring) < 0.0 {
        ring.reverse();
    }
}

/// Whether a point lies inside a ring, by ray casting. Points exactly on the
/// boundary are not guaranteed either way — callers that care sample interior
/// points instead.
pub(super) fn point_in_ring(p: P2, ring: &[P2]) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[j]);
        let straddles = (a[1] > p[1]) != (b[1] > p[1]);
        if straddles {
            let t = (p[1] - a[1]) / (b[1] - a[1]);
            if p[0] < a[0] + t * (b[0] - a[0]) {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Inside the outer ring and outside every hole.
pub(super) fn point_in_polygon(p: P2, outer: &[P2], holes: &[Vec<P2>]) -> bool {
    point_in_ring(p, outer) && !holes.iter().any(|h| point_in_ring(p, h))
}

/// Whether a ring is simple — no non-adjacent edge pair crosses.
///
/// This is the guard that stops a self-intersecting plan reaching the
/// triangulator. `extrude_mesh` swallows earcut failures and returns a mesh
/// with side walls but **no caps** — an open tube, no error, no diagnostic. So
/// a self-intersecting polygon is a silent hole, and this check is the only
/// thing standing in front of it.
pub(super) fn ring_is_simple(ring: &[P2]) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    for i in 0..n {
        let a0 = ring[i];
        let a1 = ring[(i + 1) % n];
        for j in (i + 1)..n {
            // Skip adjacent edges: they legitimately share an endpoint.
            if j == i || (j + 1) % n == i || j == (i + 1) % n {
                continue;
            }
            let b0 = ring[j];
            let b1 = ring[(j + 1) % n];
            if segments_properly_cross(a0, a1, b0, b1)
                || segments_overlap_collinearly(a0, a1, b0, b1)
            {
                return false;
            }
        }
    }
    true
}

/// Two non-adjacent edges running *along* one another rather than through.
///
/// [`segments_properly_cross`] needs the four orientations to have strictly
/// opposite signs, so a pair of edges lying on the same line and sharing a
/// stretch of it scores zero four times and reads as "no intersection". That is
/// not a corner case -- it is what an inverted offset produces. Bow a wall
/// tighter than its own half-thickness and the inner edge turns inside out,
/// leaving the ring's two closing edges collinear and overlapping: a ring with
/// no interior, which earcut hands back as a capless tube.
///
/// Found via a wall that was rejected at the origin and accepted six metres
/// away. Nothing changed but its coordinates -- f32 noise near the origin
/// tilted the two edges just enough to register as a true crossing, and further
/// out it did not. The proper-crossing test was never what caught this shape;
/// it only happened to fire, in one place.
fn segments_overlap_collinearly(a0: P2, a1: P2, b0: P2, b1: P2) -> bool {
    let d = sub(a1, a0);
    let len = length(d);
    if len < CURVE_EPSILON {
        return false;
    }
    let dir = [d[0] / len, d[1] / len];

    // Both of b's endpoints must lie on a's line. Measured as a distance, so
    // the threshold means the same on a 0.1 m nib as on a 40 m run; a raw cross
    // product would scale with both edge lengths.
    let off = |p: P2| perp_dot(dir, sub(p, a0)).abs();
    if off(b0) > OVERLAP_EPS || off(b1) > OVERLAP_EPS {
        return false;
    }

    // Collinear, so they intersect iff their 1-D spans share more than a point.
    let at = |p: P2| dot(dir, sub(p, a0));
    let (lo, hi) = {
        let (u, v) = (at(b0), at(b1));
        if u <= v { (u, v) } else { (v, u) }
    };
    hi.min(len) - lo.max(0.0) > OVERLAP_EPS
}

/// Strict crossing -- touching at an endpoint does not count.
fn segments_properly_cross(a0: P2, a1: P2, b0: P2, b1: P2) -> bool {
    let d1 = perp_dot(sub(a1, a0), sub(b0, a0));
    let d2 = perp_dot(sub(a1, a0), sub(b1, a0));
    let d3 = perp_dot(sub(b1, b0), sub(a0, b0));
    let d4 = perp_dot(sub(b1, b0), sub(a1, b0));
    ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0))
}

/// Convex hull, monotone chain. Deterministic: the input is sorted
/// lexicographically first, so the result never depends on input order.
pub(super) fn convex_hull(points: &[P2]) -> Vec<P2> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup_by(|a, b| a[0] == b[0] && a[1] == b[1]);
    if pts.len() < 3 {
        return pts;
    }

    let build = |src: &[P2]| -> Vec<P2> {
        let mut chain: Vec<P2> = Vec::with_capacity(src.len());
        for &p in src {
            while chain.len() >= 2 {
                let a = chain[chain.len() - 2];
                let b = chain[chain.len() - 1];
                if perp_dot(sub(b, a), sub(p, a)) <= 0.0 {
                    chain.pop();
                } else {
                    break;
                }
            }
            chain.push(p);
        }
        chain
    };

    let mut lower = build(&pts);
    let reversed: Vec<P2> = pts.iter().rev().copied().collect();
    let mut upper = build(&reversed);

    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Offset a polyline sideways by `d`, joining consecutive segments at their
/// intersection so the offset stays parallel through interior bends.
///
/// Positive `d` moves along [`left_normal`]. Degenerate segments are skipped
/// rather than producing NaNs.
pub(super) fn offset_polyline(points: &[P2], d: f32) -> Vec<P2> {
    if points.len() < 2 {
        return points.to_vec();
    }

    // Per-segment offset lines, skipping zero-length segments.
    let mut lines: Vec<Line> = Vec::with_capacity(points.len() - 1);
    for w in points.windows(2) {
        let Some(dir) = normalise(sub(w[1], w[0])) else { continue };
        let n = left_normal(dir);
        lines.push(Line { origin: add(w[0], scale(n, d)), dir });
    }
    if lines.is_empty() {
        return points.to_vec();
    }

    let mut out = Vec::with_capacity(lines.len() + 1);
    out.push(lines[0].origin);
    for pair in lines.windows(2) {
        // Parallel neighbours (a straight run) need no joint; the shared
        // endpoint of the second line is already correct.
        match line_line_intersect(pair[0], pair[1]) {
            Some(p) => out.push(p),
            None => out.push(pair[1].origin),
        }
    }
    let last = lines[lines.len() - 1];
    let seg_len = distance(points[points.len() - 2], points[points.len() - 1]);
    out.push(add(last.origin, scale(last.dir, seg_len)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_makes_near_identical_endpoints_bit_identical() {
        // Two walls "meeting" with sub-millimetre disagreement must land on
        // exactly the same coordinate, or their mitre points drift apart.
        let a = snap([3.000_02, -1.999_98]);
        let b = snap([2.999_97, -2.000_01]);
        assert_eq!(a, b);
        assert_eq!(key(a), key(b));
    }

    #[test]
    fn perpendicular_offset_lines_intersect_at_the_corner() {
        // Wall A along +X, wall B along +Z, both 0.2 thick, meeting at origin.
        let a = Line { origin: [0.0, 0.1], dir: [1.0, 0.0] };
        let b = Line { origin: [0.1, 0.0], dir: [0.0, 1.0] };
        let p = line_line_intersect(a, b).expect("perpendicular lines cross");
        assert!((p[0] - 0.1).abs() < 1e-6, "got {p:?}");
        assert!((p[1] - 0.1).abs() < 1e-6, "got {p:?}");
    }

    #[test]
    fn parallel_lines_do_not_intersect() {
        let a = Line { origin: [0.0, 0.0], dir: [1.0, 0.0] };
        let b = Line { origin: [0.0, 1.0], dir: [1.0, 0.0] };
        assert!(line_line_intersect(a, b).is_none());
    }

    #[test]
    fn square_is_simple_and_bowtie_is_not() {
        let square = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        assert!(ring_is_simple(&square));

        // Swapping two vertices turns it into a self-crossing bowtie, which is
        // the shape that silently loses its caps in the triangulator.
        let bowtie = [[0.0, 0.0], [1.0, 1.0], [1.0, 0.0], [0.0, 1.0]];
        assert!(!ring_is_simple(&bowtie));
    }

    #[test]
    fn edges_lying_along_one_another_are_not_simple() {
        // The shape an inverted offset leaves behind: the ring doubles back
        // along itself, so it encloses nothing. There is no proper crossing
        // anywhere in it -- every orientation test scores zero -- which is why
        // it used to pass.
        let doubled = [[0.0, 0.0], [2.0, 0.0], [0.5, 0.0], [1.5, 0.0]];
        assert!(!ring_is_simple(&doubled));

        // Touching end-to-end is not overlapping, and a rectangle's opposite
        // sides are parallel without being collinear. Neither may trip it.
        let touching = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        assert!(ring_is_simple(&touching));
        let long_thin = [[0.0, 0.0], [40.0, 0.0], [40.0, 0.05], [0.0, 0.05]];
        assert!(ring_is_simple(&long_thin));
    }

    #[test]
    fn simplicity_does_not_depend_on_where_the_building_sits() {
        // This started as a real wall that was rejected at the origin and
        // accepted six metres away -- the shape was the same, only the
        // coordinates moved. Because `ring_is_simple` is the sole guard in
        // front of the triangulator, "accepted" there meant a capless tube and
        // no diagnostic, so the bug's symptom was a hole that appeared only in
        // buildings drawn away from the origin.
        //
        // The offsets are deliberately awkward rather than round: a translation
        // that is exact in binary would move the ring without disturbing any of
        // the arithmetic, and prove nothing.
        // A ring doubled back along itself. Whether f32 noise tilts the two
        // collinear edges into a detectable crossing depends on how large the
        // coordinates are, so before the overlap test existed this was rejected
        // at the origin and accepted a few metres out -- and "accepted" meant a
        // capless tube with no diagnostic.
        let ring: Vec<P2> = vec![[0.0, 0.0], [2.0, 0.0], [0.5, 0.0], [1.5, 0.0]];
        assert!(!ring_is_simple(&ring), "doubled-back ring at the origin");

        for (dx, dz) in [(6.1, 2.4), (-37.3, 91.7), (410.5, -260.9)] {
            let moved: Vec<P2> = ring.iter().map(|p| [p[0] + dx, p[1] + dz]).collect();
            assert!(
                !ring_is_simple(&moved),
                "the same ring became simple once moved to ({dx}, {dz})"
            );
        }
    }

    #[test]
    fn winding_normalises_consistently() {
        let mut cw = vec![[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
        let mut ccw = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        normalise_ccw(&mut cw);
        normalise_ccw(&mut ccw);
        assert!(signed_area2(&cw) > 0.0);
        assert!(signed_area2(&ccw) > 0.0);
        assert!((area(&cw) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn point_in_polygon_respects_holes() {
        let outer = vec![[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]];
        let hole = vec![[1.0, 1.0], [1.0, 3.0], [3.0, 3.0], [3.0, 1.0]];
        assert!(point_in_polygon([0.5, 0.5], &outer, &[]));
        assert!(!point_in_polygon([2.0, 2.0], &outer, &[hole.clone()]));
        assert!(point_in_polygon([0.5, 0.5], &outer, &[hole]));
        assert!(!point_in_polygon([5.0, 5.0], &outer, &[]));
    }

    #[test]
    fn convex_hull_is_order_independent() {
        let pts = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0], [1.0, 1.0]];
        let mut shuffled = pts.to_vec();
        shuffled.reverse();

        let a = convex_hull(&pts);
        let b = convex_hull(&shuffled);
        assert_eq!(a, b, "hull must not depend on input order");
        // The interior point is dropped.
        assert_eq!(a.len(), 4);
        assert!((area(&a) - 4.0).abs() < 1e-6);
    }

    #[test]
    fn offsetting_a_straight_run_shifts_it_sideways() {
        let line = [[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]];
        let off = offset_polyline(&line, 0.5);
        assert_eq!(off.len(), 3);
        for p in &off {
            assert!((p[1] - left_normal([1.0, 0.0])[1] * 0.5).abs() < 1e-6, "got {p:?}");
        }
    }

    #[test]
    fn offsetting_an_l_bend_meets_at_the_inner_corner() {
        // +X then +Z. The offset joint must be a single point, not two.
        let bend = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]];
        let off = offset_polyline(&bend, 0.1);
        assert_eq!(off.len(), 3, "one point per input vertex");
        // The middle point is the intersection of the two offset lines, so it
        // sits diagonally off the original corner.
        let mid = off[1];
        assert!((mid[0] - 1.0).abs() > 1e-6 || (mid[1]).abs() > 1e-6);
    }
}
