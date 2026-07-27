//! Turning wall centrelines into plan footprints, mitred at their junctions.
//!
//! Ported from pascalorg/editor's `calculateJunctionIntersections`
//! (`packages/core/src/systems/wall/wall-mitering.ts`, MIT).
//!
//! # The rule
//!
//! At a junction, sort every outgoing wall direction by angle. Each adjacent
//! pair in that cyclic order bounds a **wedge**, and each wedge yields exactly
//! one corner point — the intersection of one wall's offset edge with its
//! neighbour's. *Both* walls adopt that point. Two walls sharing a vertex
//! bit-for-bit is what makes the corner solid rather than two overlapping
//! boxes with a hairline between them.
//!
//! A wall's two sides are called **plus** and **minus**, meaning the `+n` and
//! `-n` sides of its forward direction where `n = (-d.z, d.x)` — the normal
//! convention inherited from their code and explained in [`super::curve`]. The
//! labels are arbitrary; what matters is that a wall uses the same one at both
//! ends. Note the flip that falls out of this: at a wall's *end* the outgoing
//! direction is reversed, so the wedge's `a` side is the wall's *minus* side.
//! Getting that backwards produces a bow-tie footprint, which
//! [`plan::ring_is_simple`] catches but which is much easier to just not do.
//!
//! # Fallbacks
//!
//! A mitre is a line-line intersection, so the corner sits `≈ h / sin θ` from
//! the junction. As `θ → 0` that runs away and the wall becomes an infinite
//! spike. Two guards, both theirs: reject exactly-parallel edges, and reject
//! any corner further than [`MITER_LIMIT`] half-thicknesses from the junction.
//! Rejected wedges leave both walls butt-ended, which can open a notch — so
//! this module also emits a small [`JunctionFiller`] solid over each one.
//!
//! # The middle
//!
//! Wedges tile the *ring* around a junction. They never cover its middle, and
//! from three walls upwards the middle has area — for four walls crossing it is
//! the full thickness-by-thickness square, and every wall stops at its edge.
//! So a [`JunctionFiller`] also goes over that, on the same terms.

use super::consts::MITER_LIMIT;
use super::curve;
use super::ir::{LevelId, Wall, WallId, P2};
use super::junction::{self, End, Incidence};
use super::plan::{self, Line};

/// A wall's plan footprint, kept as its **two sides** rather than one ring.
///
/// A wall with a door in it is not extruded as a single prism — it is sliced
/// along its length into piers, sills and lintels (see [`super::openings`]).
/// Slicing needs to walk each side independently between two arc-length
/// parameters, which a closed ring has already thrown away. [`Self::ring`]
/// rebuilds the loop for anything that just wants the outline.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct WallFootprint {
    pub wall: WallId,
    /// Sample parameters along the centreline, `0.0 → 1.0`. Two entries for a
    /// straight wall, [`super::consts::ARC_SEGMENTS`] + 1 for a curved one.
    pub ts: Vec<f32>,
    /// The `+n` side, start → end. Same length as `ts`.
    pub plus: Vec<P2>,
    /// The `-n` side, start → end. Same length as `ts`.
    pub minus: Vec<P2>,
}

impl WallFootprint {
    /// The closed outline: up the plus side, back down the minus side,
    /// counter-clockwise, first point not repeated.
    pub fn ring(&self) -> Vec<P2> {
        self.slice(0.0, 1.0)
    }

    /// The outline of the stretch between two parameters along the wall.
    ///
    /// This is how a pier or a lintel gets its plan shape: [`super::openings`]
    /// decides *which* stretches are solid, and this turns one of them into a
    /// polygon. A slice that reaches an end of the wall inherits that end's
    /// mitre corner, so a pier beside a corner still meets its neighbour
    /// exactly — which is the reason the two sides are kept rather than a ring.
    pub fn slice(&self, t0: f32, t1: f32) -> Vec<P2> {
        let mut ring = sub_side(&self.ts, &self.plus, t0, t1);
        let mut back = sub_side(&self.ts, &self.minus, t0, t1);
        back.reverse();
        ring.extend(back);
        plan::normalise_ccw(&mut ring);
        ring
    }
}

/// One side of the wall between two parameters: the interpolated start, every
/// original sample strictly inside, then the interpolated end. Keeping the
/// interior samples is what preserves a curved wall's arc through a slice.
fn sub_side(ts: &[f32], pts: &[P2], t0: f32, t1: f32) -> Vec<P2> {
    const EDGE: f32 = 1e-6;
    let mut out = vec![lerp_at(ts, pts, t0)];
    for (i, &t) in ts.iter().enumerate() {
        if t > t0 + EDGE && t < t1 - EDGE {
            out.push(pts[i]);
        }
    }
    out.push(lerp_at(ts, pts, t1));
    out
}

/// The point at parameter `t`, interpolated between samples.
fn lerp_at(ts: &[f32], pts: &[P2], t: f32) -> P2 {
    let last = ts.len() - 1;
    if t <= ts[0] {
        return pts[0];
    }
    if t >= ts[last] {
        return pts[last];
    }
    for i in 1..=last {
        if t <= ts[i] {
            let span = ts[i] - ts[i - 1];
            let f = if span > 0.0 { (t - ts[i - 1]) / span } else { 0.0 };
            return plan::add(pts[i - 1], plan::scale(plan::sub(pts[i], pts[i - 1]), f));
        }
    }
    pts[last]
}

/// A patch over a junction whose mitre fell back to butt joints.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct JunctionFiller {
    pub ring: Vec<P2>,
    /// The two walls whose ends left this notch. The patch spans where both of
    /// them exist -- the *intersection* of their vertical extents. A half wall
    /// meeting a full one leaves a notch only as high as the shorter, and
    /// filling to the taller would poke a stub into the air above it.
    pub walls: [WallId; 2],
}

/// Why a wall produced no footprint. Both are loud on purpose: a wall silently
/// vanishing from a plan is a hole in the building.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum FootprintError {
    /// Zero-length centreline or non-positive thickness.
    Degenerate,
    /// The ring crosses itself — a curve tighter than its own half-thickness,
    /// or a mitre that folded back past the wall. Never hand this to the
    /// triangulator: it drops the caps and reports nothing.
    SelfIntersecting,
}

#[derive(Clone, Debug, Default)]
pub(super) struct MiterSolution {
    pub footprints: Vec<WallFootprint>,
    pub fillers: Vec<JunctionFiller>,
    pub rejected: Vec<(WallId, FootprintError)>,
}

/// The corner a wall adopts on one side of one end. Absent means "no junction
/// here", and the end squares off.
#[derive(Clone, Copy, Debug, Default)]
struct EndCorners {
    plus: Option<P2>,
    minus: Option<P2>,
}

#[derive(Clone, Copy, Debug, Default)]
struct WallCorners {
    start: EndCorners,
    end: EndCorners,
}

impl WallCorners {
    fn at(&mut self, end: End) -> Option<&mut EndCorners> {
        match end {
            End::Start => Some(&mut self.start),
            End::End => Some(&mut self.end),
            // A wall crossing a junction keeps its own geometry; only the wall
            // terminating there changes shape.
            End::Through => None,
        }
    }
}

/// The normal used throughout this module: `(-d.z, d.x)`.
///
/// This is their y-up-plot convention, *not* [`plan::left_normal`]. Kept
/// because the ported formulas depend on it and because handedness is only
/// ever relative here — see the module note on plus/minus.
fn normal(d: P2) -> P2 {
    [-d[1], d[0]]
}

/// One wall direction leaving a junction, with its two offset edges.
#[derive(Clone, Copy, Debug)]
struct Ray {
    inc: Incidence,
    /// Push order, so ties in `angle` break deterministically.
    slot: usize,
    angle: f32,
    half: f32,
    /// Offset edge on the `+n` side of the outgoing direction.
    edge_a: Line,
    edge_b: Line,
}

/// Solve every wall on one level.
///
/// `walls` must be the model's full wall list — ids index into it directly.
pub(super) fn solve_level(walls: &[Wall], level: LevelId) -> MiterSolution {
    debug_assert!(
        walls.iter().enumerate().all(|(i, w)| w.id.0 as usize == i),
        "wall ids must be dense indices; run validate::check_model first"
    );

    let mut corners = vec![WallCorners::default(); walls.len()];
    let mut fillers = Vec::new();

    for j in junction::find_junctions(walls, level) {
        solve_junction(walls, &j.point, &j.incidences, &mut corners, &mut fillers);
    }

    let mut out = MiterSolution { fillers, ..Default::default() };
    for w in walls.iter().filter(|w| w.level == level) {
        match footprint(w, corners[w.id.0 as usize]) {
            Ok(f) => out.footprints.push(f),
            Err(e) => out.rejected.push((w.id, e)),
        }
    }
    out
}

fn solve_junction(
    walls: &[Wall],
    point: &P2,
    incidences: &[Incidence],
    corners: &mut [WallCorners],
    fillers: &mut Vec<JunctionFiller>,
) {
    let mp = *point;
    let mut rays: Vec<Ray> = Vec::with_capacity(incidences.len() + 1);

    for inc in incidences {
        let w = &walls[inc.wall.0 as usize];
        let half = w.thickness * 0.5;
        if half <= 0.0 {
            continue;
        }
        let Some(fwd) = junction::outgoing_direction(w, inc.end) else { continue };

        // A wall passing *through* presents two faces to mitre against, so it
        // contributes a ray in each direction.
        let dirs: &[P2] = if inc.end == End::Through {
            &[fwd, [-fwd[0], -fwd[1]]]
        } else {
            std::slice::from_ref(&fwd)
        };

        for &d in dirs {
            let n = normal(d);
            rays.push(Ray {
                inc: *inc,
                slot: rays.len(),
                angle: d[1].atan2(d[0]),
                half,
                edge_a: Line { origin: plan::add(mp, plan::scale(n, half)), dir: d },
                edge_b: Line { origin: plan::sub(mp, plan::scale(n, half)), dir: d },
            });
        }
    }

    if rays.len() < 2 {
        return;
    }
    // Cyclic order around the junction. `total_cmp` rather than `partial_cmp`
    // so there is no unwrap to get wrong, then `(wall, end, slot)` so two walls
    // leaving at the same angle still order identically every run.
    rays.sort_by(|a, b| {
        a.angle
            .total_cmp(&b.angle)
            .then(a.inc.cmp(&b.inc))
            .then(a.slot.cmp(&b.slot))
    });

    let n = rays.len();
    let mut boundary: Vec<P2> = Vec::with_capacity(n * 2);
    for i in 0..n {
        let (r1, r2) = (rays[i], rays[(i + 1) % n]);
        match wedge_corner(&r1, &r2, mp) {
            Some(p) => {
                assign(corners, r1.inc, true, p);
                assign(corners, r2.inc, false, p);
                boundary.push(p);
            }
            None => {
                boundary.push(r1.edge_a.origin);
                boundary.push(r2.edge_b.origin);
                if let Some(ring) = filler_ring(mp, r1.edge_a.origin, r2.edge_b.origin) {
                    fillers.push(JunctionFiller {
                        ring,
                        walls: [r1.inc.wall, r2.inc.wall],
                    });
                }
            }
        }
    }

    // The wedges tile the *ring* around a junction and never its middle, and
    // once there are three or more of them the middle has area. Four walls
    // meeting is the case that matters, because a BSP floorplate produces one
    // at nearly every interior intersection: each wall stops at the near edge
    // of the central thickness-square, so the square belongs to none of them.
    // That is a full-height column of nothing at every crossing — invisible in
    // plan, unmissable from inside the room.
    //
    // With two rays this same hull degenerates to the segment joining the inner
    // corner to the outer one, which is why an L needs no patch and gets none.
    // The rule is therefore uniform and the arity test is only an early exit.
    if n >= 3 {
        if let Some(ring) = core_ring(&boundary) {
            let mut walls: Vec<WallId> = rays.iter().map(|r| r.inc.wall).collect();
            walls.sort_unstable();
            walls.dedup();
            // Attributed to the two lowest ids present. Arbitrary, but a patch
            // has to belong to *some* pair to get a height, and this pair is
            // the same on every run.
            let pair = [walls[0], *walls.get(1).unwrap_or(&walls[0])];
            fillers.push(JunctionFiller { ring, walls: pair });
        }
    }
}

/// The corner shared by two adjacent wedge walls, or `None` if it must fall
/// back to butt joints.
fn wedge_corner(r1: &Ray, r2: &Ray, mp: P2) -> Option<P2> {
    // Directions are unit here, so `line_line_intersect`'s parallel test reads
    // as `|sin θ| < COLLINEAR_EPS` — a fixed angle. Theirs compares an
    // unnormalised determinant against 1e-9, which makes the threshold depend
    // on how long the walls happen to be.
    let p = plan::line_line_intersect(r1.edge_a, r2.edge_b)?;
    if !p[0].is_finite() || !p[1].is_finite() {
        return None;
    }
    let limit = MITER_LIMIT * r1.half.max(r2.half);
    (plan::distance(p, mp) <= limit).then_some(p)
}

/// Record a corner. `is_a` means the point came from this ray's `+n` edge.
fn assign(corners: &mut [WallCorners], inc: Incidence, is_a: bool, p: P2) {
    let Some(slot) = corners[inc.wall.0 as usize].at(inc.end) else { return };
    // At a wall's start the outgoing direction is its forward direction, so the
    // `+n` edge is the wall's plus side. At its end the direction is reversed
    // and the two swap.
    if is_a == (inc.end == End::Start) {
        slot.plus = Some(p);
    } else {
        slot.minus = Some(p);
    }
}

/// The notch left behind when a wedge falls back to butt joints.
///
/// Two collinear walls butting end-to-end give three collinear points, so the
/// hull degenerates to a segment and there is nothing to fill — correct, since
/// a straight run has no notch, only (possibly) a thickness step.
///
/// The area floor is a **degeneracy guard, not a significance threshold**. It
/// is tempting to skip notches below some visible size, and wrong: two walls
/// meeting at 178° leave a sliver of a few square centimetres that is
/// nonetheless a 3 mm slot running the full height of the wall. Anything the
/// hull reports as genuinely two-dimensional gets patched.
/// The polygon at the centre of a junction that the wedges around it leave
/// uncovered.
fn core_ring(boundary: &[P2]) -> Option<Vec<P2>> {
    let mut ring = plan::convex_hull(boundary);
    if ring.len() < 3 || plan::area(&ring) < 1e-9 {
        return None;
    }
    plan::normalise_ccw(&mut ring);
    Some(ring)
}

fn filler_ring(mp: P2, a: P2, b: P2) -> Option<Vec<P2>> {
    let mut ring = plan::convex_hull(&[mp, a, b]);
    if ring.len() < 3 || plan::area(&ring) < 1e-9 {
        return None;
    }
    plan::normalise_ccw(&mut ring);
    Some(ring)
}

/// Build one wall's plan footprint.
fn footprint(w: &Wall, c: WallCorners) -> Result<WallFootprint, FootprintError> {
    let (s, e) = junction::ends(w);
    let half = w.thickness * 0.5;
    let offset = w.curve_offset.unwrap_or(0.0);
    let chord = curve::chord_frame(s, e);
    if half <= 0.0 || chord.length <= 0.0 {
        return Err(FootprintError::Degenerate);
    }

    // Sides are sampled analytically off the centreline frame rather than by
    // offsetting the sampled polyline: the endpoint normals then match the ones
    // the junction solver used exactly, so mitre points land where the sides
    // actually arrive.
    let side = |t: f32, sign: f32| {
        let f = curve::frame_at(s, e, offset, t);
        plan::add(f.point, plan::scale(normal(f.tangent), sign * half))
    };

    let steps = if curve::is_curved(chord.length, offset) {
        super::consts::ARC_SEGMENTS
    } else {
        1
    };

    let mut ts: Vec<f32> = Vec::with_capacity(steps + 1);
    let mut plus: Vec<P2> = Vec::with_capacity(steps + 1);
    let mut minus: Vec<P2> = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        ts.push(t);
        plus.push(side(t, 1.0));
        minus.push(side(t, -1.0));
    }

    // Junction corners override the squared-off defaults.
    if let Some(p) = c.start.plus {
        plus[0] = p;
    }
    if let Some(p) = c.start.minus {
        minus[0] = p;
    }
    if let Some(p) = c.end.plus {
        *plus.last_mut().expect("non-empty") = p;
    }
    if let Some(p) = c.end.minus {
        *minus.last_mut().expect("non-empty") = p;
    }

    let out = WallFootprint { wall: w.id, ts, plus, minus };
    if !plan::ring_is_simple(&out.ring()) {
        return Err(FootprintError::SelfIntersecting);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::arch::ir::{ArchModel, Level, LevelId, ModelSource};

    const EPS: f32 = 1e-5;

    fn model() -> ArchModel {
        let mut m = ArchModel::new(ModelSource::PascalEditor);
        m.levels.push(Level { id: LevelId(0), name: None, height: 2.5 });
        m
    }

    fn wall(m: &mut ArchModel, start: P2, end: P2, thickness: f32) -> WallId {
        m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(0),
            start,
            end,
            thickness,
            height: None,
            curve_offset: None,
            openings: Vec::new(),
            material: None,
        })
    }

    fn ring_of(sol: &MiterSolution, w: WallId) -> Vec<P2> {
        sol.footprints
            .iter()
            .find(|f| f.wall == w)
            .unwrap_or_else(|| panic!("no footprint for {w:?}: {:?}", sol.rejected))
            .ring()
    }

    fn has_point(ring: &[P2], p: P2) -> bool {
        ring.iter().any(|q| plan::distance(*q, p) < EPS)
    }

    #[test]
    fn a_lone_wall_squares_off_at_both_ends() {
        let mut m = model();
        let w = wall(&mut m, [0.0, 0.0], [4.0, 0.0], 0.2);
        let sol = solve_level(&m.walls, LevelId(0));

        assert!(sol.rejected.is_empty() && sol.fillers.is_empty());
        let ring = ring_of(&sol, w);
        assert_eq!(ring.len(), 4);
        assert!((plan::area(&ring) - 4.0 * 0.2).abs() < EPS, "{ring:?}");
    }

    #[test]
    fn a_right_angle_corner_is_shared_by_both_walls() {
        // The worked case. Wall A arrives from −x, wall B leaves along +z, both
        // 0.2 thick, meeting at the origin. A occupies z ∈ [−0.1, 0.1] and B
        // occupies x ∈ [−0.1, 0.1], so their edges cross at the L's outer elbow
        // (0.1, −0.1) and its inner elbow (−0.1, 0.1).
        let mut m = model();
        let a = wall(&mut m, [-5.0, 0.0], [0.0, 0.0], 0.2);
        let b = wall(&mut m, [0.0, 0.0], [0.0, 5.0], 0.2);

        let sol = solve_level(&m.walls, LevelId(0));
        assert!(sol.rejected.is_empty(), "{:?}", sol.rejected);
        assert!(sol.fillers.is_empty(), "a clean 90° mitre needs no filler");

        let (ra, rb) = (ring_of(&sol, a), ring_of(&sol, b));
        for corner in [[0.1, -0.1], [-0.1, 0.1]] {
            assert!(has_point(&ra, corner), "A missing {corner:?}: {ra:?}");
            assert!(has_point(&rb, corner), "B missing {corner:?}: {rb:?}");
        }
    }

    #[test]
    fn a_closed_rectangle_tiles_its_ring_exactly() {
        // The strongest statement available without a mesh: four mitred walls
        // around a 6×4 centreline rectangle must cover the annulus between the
        // outer and inner rectangles with no overlap and no gap. Overlap would
        // push the total above the analytic area, a gap below it.
        let (w, d, t) = (6.0_f32, 4.0_f32, 0.15_f32);
        let mut m = model();
        let c = [[0.0, 0.0], [w, 0.0], [w, d], [0.0, d]];
        for i in 0..4 {
            wall(&mut m, c[i], c[(i + 1) % 4], t);
        }

        let sol = solve_level(&m.walls, LevelId(0));
        assert!(sol.rejected.is_empty(), "{:?}", sol.rejected);
        assert_eq!(sol.footprints.len(), 4);
        assert!(sol.fillers.is_empty());

        let total: f32 = sol.footprints.iter().map(|f| plan::area(&f.ring())).sum();
        let expected = (w + t) * (d + t) - (w - t) * (d - t);
        assert!((total - expected).abs() < 1e-3, "got {total}, want {expected}");
    }

    #[test]
    fn walls_of_different_thickness_still_share_their_corner() {
        let mut m = model();
        let a = wall(&mut m, [-5.0, 0.0], [0.0, 0.0], 0.3);
        let b = wall(&mut m, [0.0, 0.0], [0.0, 5.0], 0.1);

        let sol = solve_level(&m.walls, LevelId(0));
        let (ra, rb) = (ring_of(&sol, a), ring_of(&sol, b));
        // A is 0.3 thick so its faces are at z = ±0.15; B is 0.1 thick so its
        // faces are at x = ±0.05.
        for corner in [[0.05, -0.15], [-0.05, 0.15]] {
            assert!(has_point(&ra, corner), "A missing {corner:?}: {ra:?}");
            assert!(has_point(&rb, corner), "B missing {corner:?}: {rb:?}");
        }
    }

    #[test]
    fn a_t_junction_leaves_the_spine_untouched() {
        let mut m = model();
        let spine = wall(&mut m, [-4.0, 0.0], [4.0, 0.0], 0.2);
        let stem = wall(&mut m, [0.0, 0.0], [0.0, 3.0], 0.2);

        let sol = solve_level(&m.walls, LevelId(0));
        assert!(sol.rejected.is_empty(), "{:?}", sol.rejected);

        // The spine keeps its plain rectangle: a wall passing through a
        // junction must not change shape, or every T would shorten it.
        let rs = ring_of(&sol, spine);
        assert_eq!(rs.len(), 4);
        assert!((plan::area(&rs) - 8.0 * 0.2).abs() < EPS, "{rs:?}");

        // The stem stops on the spine's face rather than crossing it.
        let rt = ring_of(&sol, stem);
        assert!(has_point(&rt, [-0.1, 0.1]), "{rt:?}");
        assert!(has_point(&rt, [0.1, 0.1]), "{rt:?}");
    }

    #[test]
    fn a_shallow_corner_falls_back_instead_of_spiking() {
        // Two walls meeting at ~2°. A true mitre would put the corner ~3 m from
        // the junction on a 0.2 m wall — an infinite spike, as far as anyone
        // looking at it is concerned.
        let mut m = model();
        let a = wall(&mut m, [-5.0, 0.0], [0.0, 0.0], 0.2);
        let angle = 178.0_f32.to_radians();
        let b = wall(&mut m, [0.0, 0.0], [5.0 * angle.cos(), 5.0 * angle.sin()], 0.2);

        let sol = solve_level(&m.walls, LevelId(0));
        assert!(sol.rejected.is_empty(), "{:?}", sol.rejected);

        let limit = MITER_LIMIT * 0.1;
        for f in &sol.footprints {
            for p in &f.ring() {
                assert!(
                    plan::distance(*p, [0.0, 0.0]) <= 5.0 + limit,
                    "spike at {p:?} on {:?}",
                    f.wall
                );
            }
        }
        // Neither end mitred, so both walls keep a plain quad — and wall A's
        // butt face sits square on x = 0.
        assert_eq!(ring_of(&sol, a).len(), 4);
        assert_eq!(ring_of(&sol, b).len(), 4);
        assert!(has_point(&ring_of(&sol, a), [0.0, -0.1]));
        // ...while B's butt face is tilted with it, opening a sliver between
        // the two. That sliver is a 3 mm slot through the full height of the
        // wall, so it must be patched however small its area looks in plan.
        assert!(!sol.fillers.is_empty(), "a butted corner leaves a gap to fill");
        let in_sliver = [0.000_8, 0.05];
        let covered = sol
            .footprints
            .iter()
            .map(|f| f.ring())
            .chain(sol.fillers.iter().map(|f| f.ring.clone()))
            .any(|r| plan::point_in_ring(in_sliver, &r));
        assert!(covered, "gap at {in_sliver:?} left open by {:?}", sol.fillers);
    }

    #[test]
    fn collinear_walls_butt_without_a_filler() {
        // A wall split in two is not a corner: the halves meet flush and there
        // is nothing to patch.
        let mut m = model();
        wall(&mut m, [-4.0, 0.0], [0.0, 0.0], 0.2);
        wall(&mut m, [0.0, 0.0], [4.0, 0.0], 0.2);

        let sol = solve_level(&m.walls, LevelId(0));
        assert!(sol.rejected.is_empty(), "{:?}", sol.rejected);
        assert!(sol.fillers.is_empty(), "{:?}", sol.fillers);
        let total: f32 = sol.footprints.iter().map(|f| plan::area(&f.ring())).sum();
        assert!((total - 8.0 * 0.2).abs() < 1e-3, "got {total}");
    }

    #[test]
    fn a_curved_wall_keeps_its_arc_and_stays_simple() {
        let mut m = model();
        let id = m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(0),
            start: [0.0, 0.0],
            end: [4.0, 0.0],
            thickness: 0.2,
            height: None,
            curve_offset: Some(0.6),
            openings: Vec::new(),
            material: None,
        });

        let sol = solve_level(&m.walls, LevelId(0));
        assert!(sol.rejected.is_empty(), "{:?}", sol.rejected);
        let ring = ring_of(&sol, id);
        assert_eq!(ring.len(), 2 * (super::super::consts::ARC_SEGMENTS + 1));
        assert!(plan::ring_is_simple(&ring));
        // Its area is the arc length times the thickness, near enough.
        let expected = curve::centreline_length([0.0, 0.0], [4.0, 0.0], Some(0.6)) * 0.2;
        assert!((plan::area(&ring) - expected).abs() < 5e-3, "{}", plan::area(&ring));
    }

    #[test]
    fn a_curve_tighter_than_its_own_thickness_is_rejected_not_folded() {
        // Half-thickness 0.6 on an arc of radius ~0.5: the inner offset turns
        // inside out. That ring must never reach the triangulator, which drops
        // the caps and says nothing.
        let mut m = model();
        m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(0),
            start: [0.0, 0.0],
            end: [1.0, 0.0],
            thickness: 1.2,
            height: None,
            curve_offset: Some(0.5),
            openings: Vec::new(),
            material: None,
        });

        let sol = solve_level(&m.walls, LevelId(0));
        assert_eq!(sol.rejected, vec![(WallId(0), FootprintError::SelfIntersecting)]);
        assert!(sol.footprints.is_empty());
    }

    #[test]
    fn a_degenerate_wall_is_reported_rather_than_dropped() {
        let mut m = model();
        wall(&mut m, [1.0, 1.0], [1.0, 1.0], 0.2);
        let sol = solve_level(&m.walls, LevelId(0));
        assert_eq!(sol.rejected, vec![(WallId(0), FootprintError::Degenerate)]);
    }

    #[test]
    fn footprints_are_reproducible_and_order_independent() {
        let c = [[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]];

        let mut forward = model();
        for i in 0..4 {
            wall(&mut forward, c[i], c[(i + 1) % 4], 0.15);
        }
        let mut backward = model();
        for i in (0..4).rev() {
            wall(&mut backward, c[i], c[(i + 1) % 4], 0.15);
        }

        let a = solve_level(&forward.walls, LevelId(0));
        assert_eq!(a.footprints, solve_level(&forward.walls, LevelId(0)).footprints);

        // Declaring the same rectangle backwards must give the same set of
        // shapes, even though the walls come out under different ids.
        let b = solve_level(&backward.walls, LevelId(0));
        let mut areas_a: Vec<_> =
            a.footprints.iter().map(|f| (plan::area(&f.ring()) * 1e4) as i64).collect();
        let mut areas_b: Vec<_> =
            b.footprints.iter().map(|f| (plan::area(&f.ring()) * 1e4) as i64).collect();
        areas_a.sort_unstable();
        areas_b.sort_unstable();
        assert_eq!(areas_a, areas_b);
    }

    #[test]
    fn every_footprint_winds_the_same_way() {
        // The sinks assume one winding. A ring that comes out clockwise would
        // extrude inside out — visible only as backface culling in the engine,
        // which is a miserable thing to debug.
        let mut m = model();
        let c = [[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]];
        for i in 0..4 {
            wall(&mut m, c[i], c[(i + 1) % 4], 0.15);
        }
        wall(&mut m, [2.0, 0.0], [2.0, 4.0], 0.1);

        let sol = solve_level(&m.walls, LevelId(0));
        for f in &sol.footprints {
            assert!(plan::signed_area2(&f.ring()) > 0.0, "{:?} winds backwards", f.wall);
        }
        for f in &sol.fillers {
            assert!(plan::signed_area2(&f.ring) > 0.0);
        }
    }

    #[test]
    fn a_four_way_crossing_mitres_all_four_arms() {
        let mut m = model();
        let ids: Vec<_> = [[-4.0, 0.0], [4.0, 0.0], [0.0, -4.0], [0.0, 4.0]]
            .into_iter()
            .map(|end| wall(&mut m, [0.0, 0.0], end, 0.2))
            .collect();

        let sol = solve_level(&m.walls, LevelId(0));
        assert!(sol.rejected.is_empty(), "{:?}", sol.rejected);

        // Every arm stops on the square at the centre, so all four share the
        // same four corner points.
        for id in ids {
            let ring = ring_of(&sol, id);
            assert_eq!(ring.len(), 4);
            let near: Vec<_> =
                ring.iter().filter(|p| plan::distance(**p, [0.0, 0.0]) < 0.2).collect();
            assert_eq!(near.len(), 2, "arm {id:?} should meet the crossing twice: {ring:?}");
        }

        // And that square belongs to none of them, so it has to be patched.
        // This assertion used to read `fillers.is_empty()` — "four right angles
        // need no patching" — which was the bug written down as a fact. Four
        // arms stopping on a square leave the square empty; that is a
        // 0.2 × 0.2 column of nothing running the full height of the wall.
        assert_eq!(sol.fillers.len(), 1, "the centre square is unpatched");
        let patch = &sol.fillers[0];
        assert!(
            (plan::area(&patch.ring) - 0.04).abs() < 1e-5,
            "patch should be the thickness square, got area {}",
            plan::area(&patch.ring),
        );
        for p in &patch.ring {
            assert!(plan::distance(*p, [0.0, 0.0]) < 0.15, "patch corner {p:?} is not at the centre");
        }
    }

    #[test]
    fn a_right_angle_needs_no_patch() {
        // The other half of the rule: two walls meeting leave a hull that
        // degenerates to the segment from the inner corner to the outer one, so
        // nothing is emitted. If this ever starts producing a patch, every
        // corner in every building gains a redundant overlapping solid.
        let mut m = model();
        wall(&mut m, [-4.0, 0.0], [0.0, 0.0], 0.2);
        wall(&mut m, [0.0, 0.0], [0.0, 4.0], 0.2);
        let sol = solve_level(&m.walls, LevelId(0));
        assert!(sol.fillers.is_empty(), "{:?}", sol.fillers);
    }
}
