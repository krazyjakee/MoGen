//! Where walls meet.
//!
//! A junction is a plan position with two or more wall ends at it, plus any
//! wall whose *middle* passes through that position — a T. Grouping these is
//! the whole input to the mitre solver: a wall on its own has square ends, and
//! everything interesting happens where two of them arrive.
//!
//! Ported from pascalorg/editor's `findJunctions`
//! (`packages/core/src/systems/wall/wall-mitering.ts`, MIT), with two
//! deliberate departures:
//!
//! 1. **Endpoints are snapped, not merely keyed.** Their code groups by a
//!    rounded key but then mitres from the raw coordinates, so two walls
//!    "meeting" 0.4 mm apart compute corners 0.4 mm apart and leave a crack.
//!    Here [`plan::snap`] is applied to the coordinates themselves, so both
//!    walls do their arithmetic on bit-identical numbers and their polygons
//!    share vertices exactly. See [`plan::quantise`].
//! 2. **Grouping is sort-and-group over a `Vec`,** not a hash map. Junction
//!    order feeds directly into geometry, and hash iteration order is the
//!    classic way a "deterministic" generator stops being one.

use super::consts::CONNECTIVITY_SLOP;
use super::curve;
use super::ir::{LevelId, Wall, WallId, P2};
use super::plan;

/// Which part of a wall touches a junction.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum End {
    Start,
    End,
    /// The junction lands in the middle of this wall — a T. The wall's own
    /// geometry is unaffected; it is present so the wall arriving at it has
    /// two faces to mitre against.
    Through,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) struct Incidence {
    pub wall: WallId,
    pub end: End,
}

#[derive(Clone, Debug)]
pub(super) struct Junction {
    /// Grid key the incidences were grouped by.
    pub key: (i32, i32),
    /// The meeting point, on the snap grid.
    pub point: P2,
    /// Sorted by `(wall, end)`, so the order never depends on input order.
    pub incidences: Vec<Incidence>,
}

/// A wall's endpoints, snapped onto the junction grid.
///
/// Every consumer must go through this rather than reading `wall.start` /
/// `wall.end` directly — mixing snapped and unsnapped coordinates reintroduces
/// exactly the sub-millimetre crack the snapping exists to close.
pub(super) fn ends(w: &Wall) -> (P2, P2) {
    (plan::snap(w.start), plan::snap(w.end))
}

/// Group the wall ends on one level into junctions.
///
/// Levels are solved independently and this is where that is enforced: a
/// ground-floor wall and the first-floor wall directly above it share a plan
/// position but must not mitre together.
pub(super) fn find_junctions(walls: &[Wall], level: LevelId) -> Vec<Junction> {
    let on_level = || walls.iter().filter(move |w| w.level == level);

    let mut raw: Vec<((i32, i32), Incidence)> = Vec::new();
    for w in on_level() {
        let (s, e) = ends(w);
        raw.push((plan::key(s), Incidence { wall: w.id, end: End::Start }));
        raw.push((plan::key(e), Incidence { wall: w.id, end: End::End }));
    }
    // Sorting by the whole tuple gives both the grouping and the within-group
    // order in one pass, and `Incidence` is `Ord` precisely so this works.
    raw.sort_unstable();

    let mut out: Vec<Junction> = Vec::new();
    for (k, inc) in raw {
        match out.last_mut() {
            Some(j) if j.key == k => j.incidences.push(inc),
            _ => out.push(Junction {
                key: k,
                point: [plan::dequantise(k.0), plan::dequantise(k.1)],
                incidences: vec![inc],
            }),
        }
    }

    // Second pass: a wall whose body crosses a junction joins it, so walls
    // ending there mitre against its faces instead of butting into open air.
    for j in &mut out {
        for w in on_level() {
            if j.incidences.iter().any(|i| i.wall == w.id) {
                continue;
            }
            if point_on_wall_body(j.point, w) {
                j.incidences.push(Incidence { wall: w.id, end: End::Through });
            }
        }
        // The pass-through walls were appended after the sort, so restore the
        // documented order. Nothing downstream should depend on it — the mitre
        // solver re-sorts by angle — but an order that is *nearly* canonical is
        // the kind of thing a later change quietly starts relying on.
        j.incidences.sort_unstable();
    }

    // A single wall end is not a junction; it is just an end.
    out.retain(|j| j.incidences.len() >= 2);
    out
}

/// Whether `p` lies strictly inside a wall's span — not at either end, which
/// the first pass already handled.
///
/// Measured against the **chord**, matching their implementation. A curved wall
/// therefore only registers a T where the arc is shallow enough that chord and
/// arc agree to within [`CONNECTIVITY_SLOP`]; a wall landing on the belly of a
/// tight arc butts instead of mitring. That is a cosmetic miss, not a hole.
fn point_on_wall_body(p: P2, w: &Wall) -> bool {
    let (s, e) = ends(w);
    if plan::key(p) == plan::key(s) || plan::key(p) == plan::key(e) {
        return false;
    }

    let v = plan::sub(e, s);
    let len2 = plan::dot(v, v);
    if len2 < f32::EPSILON {
        return false;
    }

    let t = plan::dot(v, plan::sub(p, s)) / len2;
    let margin = CONNECTIVITY_SLOP / len2.sqrt();
    if t < margin || t > 1.0 - margin {
        return false;
    }

    plan::distance(p, plan::add(s, plan::scale(v, t))) < CONNECTIVITY_SLOP
}

/// The unit direction leading *away* from a junction along a wall.
///
/// For curved walls this is the **analytic** tangent, not the direction of the
/// first sampled segment. A curved wall and its straight neighbour would
/// otherwise mitre from directions that differ by half a segment's turn and
/// leave a crack no snapping can close.
pub(super) fn outgoing_direction(w: &Wall, end: End) -> Option<P2> {
    let (s, e) = ends(w);
    let offset = w.curve_offset.unwrap_or(0.0);
    let v = match end {
        End::Start => curve::frame_at(s, e, offset, 0.0).tangent,
        End::End => plan::scale(curve::frame_at(s, e, offset, 1.0).tangent, -1.0),
        End::Through => plan::sub(e, s),
    };
    plan::normalise(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::arch::ir::{ArchModel, Level, ModelSource, MatRef};

    fn wall(model: &mut ArchModel, start: P2, end: P2) -> WallId {
        model.push_wall(Wall {
            id: WallId(0),
            level: LevelId(0),
            start,
            end,
            thickness: 0.2,
            height: None,
            curve_offset: None,
            openings: Vec::new(),
            material: None,
        })
    }

    fn model() -> ArchModel {
        let mut m = ArchModel::new(ModelSource::PascalEditor);
        m.levels.push(Level { id: LevelId(0), name: None, height: 2.5 });
        m
    }

    #[test]
    fn an_l_corner_is_one_junction_with_two_incidences() {
        let mut m = model();
        let a = wall(&mut m, [-5.0, 0.0], [0.0, 0.0]);
        let b = wall(&mut m, [0.0, 0.0], [0.0, 5.0]);

        let js = find_junctions(&m.walls, LevelId(0));
        assert_eq!(js.len(), 1, "free ends are not junctions: {js:?}");
        assert_eq!(js[0].point, [0.0, 0.0]);
        assert_eq!(
            js[0].incidences,
            vec![
                Incidence { wall: a, end: End::End },
                Incidence { wall: b, end: End::Start },
            ]
        );
    }

    #[test]
    fn a_closed_rectangle_has_four_two_wall_junctions() {
        let mut m = model();
        let c = [[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]];
        for i in 0..4 {
            wall(&mut m, c[i], c[(i + 1) % 4]);
        }
        let js = find_junctions(&m.walls, LevelId(0));
        assert_eq!(js.len(), 4);
        assert!(js.iter().all(|j| j.incidences.len() == 2), "{js:?}");
    }

    #[test]
    fn sub_millimetre_disagreement_still_makes_one_junction() {
        // The case snapping exists for: two walls a fraction of a millimetre
        // apart must produce one junction, at one coordinate.
        let mut m = model();
        wall(&mut m, [-5.0, 0.0], [0.000_2, -0.000_3]);
        wall(&mut m, [0.0, 0.0], [0.0, 5.0]);

        let js = find_junctions(&m.walls, LevelId(0));
        assert_eq!(js.len(), 1, "{js:?}");
        assert_eq!(js[0].point, [0.0, 0.0]);
    }

    #[test]
    fn a_stem_meeting_a_wall_midspan_makes_a_t() {
        let mut m = model();
        let spine = wall(&mut m, [-4.0, 0.0], [4.0, 0.0]);
        let stem = wall(&mut m, [0.0, 0.0], [0.0, 3.0]);

        let js = find_junctions(&m.walls, LevelId(0));
        assert_eq!(js.len(), 1, "{js:?}");
        assert_eq!(
            js[0].incidences,
            vec![
                Incidence { wall: spine, end: End::Through },
                Incidence { wall: stem, end: End::Start },
            ],
            "the spine passes through; only the stem terminates"
        );
    }

    #[test]
    fn a_stem_stopping_short_of_a_wall_is_not_a_junction() {
        let mut m = model();
        wall(&mut m, [-4.0, 0.0], [4.0, 0.0]);
        // 5 mm shy — well beyond CONNECTIVITY_SLOP.
        wall(&mut m, [0.0, 0.005], [0.0, 3.0]);
        assert!(find_junctions(&m.walls, LevelId(0)).is_empty());
    }

    #[test]
    fn walls_on_different_levels_never_share_a_junction() {
        // The same plan position on two storeys. Mitring these together would
        // fuse a ground-floor wall into the one above it.
        let mut m = model();
        m.levels.push(Level { id: LevelId(1), name: None, height: 2.5 });
        wall(&mut m, [-5.0, 0.0], [0.0, 0.0]);
        let upper = m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(1),
            start: [0.0, 0.0],
            end: [0.0, 5.0],
            thickness: 0.2,
            height: None,
            curve_offset: None,
            openings: Vec::new(),
            material: Some(MatRef("upper".into())),
        });

        assert!(find_junctions(&m.walls, LevelId(0)).is_empty());
        assert!(find_junctions(&m.walls, LevelId(1)).is_empty());
        // ...and the upper wall really is on the upper level.
        assert_eq!(m.walls[upper.0 as usize].level, LevelId(1));
    }

    #[test]
    fn junction_order_does_not_depend_on_wall_order() {
        let corners = [[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]];

        let mut forward = model();
        for i in 0..4 {
            wall(&mut forward, corners[i], corners[(i + 1) % 4]);
        }
        let mut backward = model();
        for i in (0..4).rev() {
            wall(&mut backward, corners[i], corners[(i + 1) % 4]);
        }

        let a: Vec<_> = find_junctions(&forward.walls, LevelId(0)).iter().map(|j| j.key).collect();
        let b: Vec<_> = find_junctions(&backward.walls, LevelId(0)).iter().map(|j| j.key).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn outgoing_directions_point_away_from_the_junction() {
        let mut m = model();
        let a = wall(&mut m, [-5.0, 0.0], [0.0, 0.0]);
        let b = wall(&mut m, [0.0, 0.0], [0.0, 5.0]);

        let da = outgoing_direction(&m.walls[a.0 as usize], End::End).unwrap();
        let db = outgoing_direction(&m.walls[b.0 as usize], End::Start).unwrap();
        assert!((da[0] + 1.0).abs() < 1e-6 && da[1].abs() < 1e-6, "{da:?}");
        assert!(db[0].abs() < 1e-6 && (db[1] - 1.0).abs() < 1e-6, "{db:?}");
    }

    #[test]
    fn a_curved_walls_outgoing_direction_is_its_arc_tangent() {
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
        let d = outgoing_direction(&m.walls[id.0 as usize], End::Start).unwrap();
        // Bulging right (−z), the tangent leaves the start heading +x and −z.
        assert!(d[0] > 0.0 && d[1] < 0.0, "{d:?}");
        // ...and it is a genuine tangent, not the chord.
        assert!(d[1].abs() > 1e-3, "chord direction leaked through: {d:?}");
    }
}
