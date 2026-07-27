//! Where things sit vertically.
//!
//! The IR describes a building in plan; this decides what Y everything lands
//! at. Three quantities, each verified against pascalorg/editor rather than
//! reasoned out, because every one of them fails silently:
//!
//! 1. **A level's floor plane** is the running sum of the level heights below
//!    it. [`ir::Level::height`] is floor-to-**floor**, so reading it as
//!    floor-to-ceiling delaminates every storey above ground by one slab.
//!
//! 2. **A plane-bound wall's top** is the storey plane — the full
//!    floor-to-floor height — not the underside of the slab above. Their
//!    `wall-top.ts` puts it plainly: `if (wall.height == null) return
//!    storeyHeight`. Stopping at the slab soffit instead would leave a
//!    slab-thickness gap at every floor division, open to the sky, and each
//!    individual wall would still look perfectly correct.
//!
//! 3. **A wall's base** is the top of whatever slab it stands on, so a slab
//!    makes a plane-bound wall *shorter*, never taller.
//!
//! # What is simplified
//!
//! Their slab election (`computeWallSlabSupport`) is ~230 lines of exact
//! interval arithmetic: it tests the centreline *and both face polylines*
//! against every slab, splits the wall at breakpoints, takes the lower of the
//! two faces per segment, and elects by coverage. That precision serves
//! interactive editing, where a user drags a slab edge across a wall and wants
//! the wall to react per-segment.
//!
//! Here the wall is sampled every [`SUPPORT_SAMPLE_STEP`] along its centreline
//! and coverage is counted. The election *rule* is theirs — pool slabs by
//! elevation, a tier covering a majority wins, highest tier breaks ties — but
//! the measurement is approximate. What that costs: a wall straddling two slabs
//! at different elevations gets one base for its whole length rather than a
//! stepped one, and the crossover point can be off by a sample. What it does
//! not cost: anything at all in the common cases of one slab per level, or
//! none. Revisit if a producer starts emitting walls that genuinely span
//! elevation changes.
//!
//! Sampling at a fixed *spacing* rather than a fixed count is load-bearing. A
//! fixed count makes each sample stand for `length / n` metres, which on a long
//! wall exceeds [`SLAB_MIN_OVERLAP`] — and then a slab clipping 20 mm off the
//! end of a wall reads as enough support to lift the entire wall onto it.

use super::consts::{
    CEILING_SHELL_THICKNESS, ELEVATION_POOL_EPS, MIN_WALL_H, SLAB_MIN_OVERLAP,
    SLAB_SUPPORT_MAJORITY, SUPPORT_SAMPLES_MAX, SUPPORT_SAMPLES_MIN, SUPPORT_SAMPLE_STEP,
};
use super::curve;
use super::ir::{ArchModel, Ceiling, LevelId, Slab, Wall, P2};
use super::junction;
use super::plan;

/// A vertical extent in **world** Y.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Span {
    pub base: f32,
    pub top: f32,
}

impl Span {
    pub fn height(&self) -> f32 {
        self.top - self.base
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum HeightError {
    /// The element names a level the model does not have.
    UnknownLevel(LevelId),
    /// The resolved wall would be shorter than [`MIN_WALL_H`] — usually a slab
    /// raised almost to the storey plane. Reported rather than emitted: a
    /// sliver wall is degenerate geometry, and silently dropping it is a hole.
    WallTooShort(f32),
}

/// World Y of a level's floor plane.
///
/// Level 0 is the datum at Y = 0. Positive levels stack upward by the heights
/// of the levels beneath them; negative levels (basements) hang below by their
/// own heights, so level −1's *ceiling* lands exactly on the ground plane.
///
/// Gaps in the numbering are summed over rather than rejected — a model with
/// levels 0 and 2 but no 1 puts level 2 directly on top of level 0. That is the
/// only sane reading available, but it is a reading, so a producer that means
/// something else should emit the intermediate level.
pub(super) fn level_plane_y(model: &ArchModel, level: LevelId) -> Option<f32> {
    model.level(level)?;
    let mut y = 0.0;
    for l in &model.levels {
        if level.0 > 0 && l.id.0 >= 0 && l.id.0 < level.0 {
            y += l.height;
        } else if level.0 < 0 && l.id.0 < 0 && l.id.0 >= level.0 {
            y -= l.height;
        }
    }
    Some(y)
}

/// The level-local elevation a wall's base rests at: the top of the slab tier
/// carrying it, or `0.0` for a wall standing on the level plane itself.
pub(super) fn slab_support(model: &ArchModel, wall: &Wall) -> f32 {
    let (s, e) = junction::ends(wall);
    let length = curve::centreline_length(s, e, wall.curve_offset);
    if length < f32::EPSILON {
        return 0.0;
    }

    // Sampled at a fixed spacing along the *arc*, so a curved wall is measured
    // where it actually runs rather than along its chord.
    let n = ((length / SUPPORT_SAMPLE_STEP).ceil() as usize + 1)
        .clamp(SUPPORT_SAMPLES_MIN, SUPPORT_SAMPLES_MAX);
    let offset = wall.curve_offset.unwrap_or(0.0);
    let samples: Vec<P2> = (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1) as f32;
            curve::frame_at(s, e, offset, t).point
        })
        .collect();

    // Slabs poured at the same elevation form one tier, and a tier's coverage
    // is the *union* of its slabs' — two half-slabs meeting under a wall
    // support it as well as one whole one would.
    struct Tier {
        elevation: f32,
        covered: Vec<bool>,
    }
    let mut tiers: Vec<Tier> = Vec::new();

    // A short wall needs proportionally less support, or a 100 mm nib could
    // never stand on anything. Their clamp.
    let min_support = SLAB_MIN_OVERLAP.min(length * 0.5).max(1e-3);

    for slab in model.slabs.iter().filter(|s| s.level == wall.level) {
        let mask: Vec<bool> = samples
            .iter()
            .map(|p| plan::point_in_polygon(*p, &slab.poly.outer, &slab.poly.holes))
            .collect();
        let hits = mask.iter().filter(|b| **b).count();
        let supported = length * (hits as f32) / (n as f32);
        if hits == 0 || supported < min_support {
            continue;
        }

        match tiers
            .iter_mut()
            .find(|t| (t.elevation - slab.elevation).abs() <= ELEVATION_POOL_EPS)
        {
            Some(t) => {
                for (acc, hit) in t.covered.iter_mut().zip(&mask) {
                    *acc |= *hit;
                }
            }
            None => tiers.push(Tier { elevation: slab.elevation, covered: mask }),
        }
    }

    let coverage = |t: &Tier| t.covered.iter().filter(|b| **b).count() as f32 / (n as f32);

    // A tier under most of the wall wins outright, highest first — a wall on a
    // raised plinth stands on the plinth, not on the floor beside it.
    let majority = tiers
        .iter()
        .filter(|t| coverage(t) >= SLAB_SUPPORT_MAJORITY)
        .map(|t| t.elevation)
        .fold(None::<f32>, |acc, e| Some(acc.map_or(e, |a| a.max(e))));
    if let Some(e) = majority {
        return e;
    }

    // Otherwise the best-covered tier. Slabs were visited in id order and ties
    // break on elevation, so the answer never depends on declaration order.
    tiers
        .iter()
        .fold(None::<&Tier>, |best, t| match best {
            None => Some(t),
            Some(b) => {
                let (ct, cb) = (coverage(t), coverage(b));
                let better = ct > cb || (ct == cb && t.elevation > b.elevation);
                Some(if better { t } else { b })
            }
        })
        .map(|t| t.elevation)
        .unwrap_or(0.0)
}

/// Their `resolveWallTop`, ported as-is.
///
/// The `base > 0` branch is theirs and looks odd until you read it as: an
/// explicit height means "this tall, measured from whatever holds it up", but a
/// wall on a sunken or absent base measures from the level plane instead. Kept
/// verbatim rather than tidied, so an imported half-wall lands where their
/// editor drew it.
fn resolve_top(height: Option<f32>, storey_height: f32, base: f32) -> f32 {
    match height {
        None => storey_height,
        Some(h) if base > 0.0 => base + h,
        Some(h) => h,
    }
}

/// The vertical extent of a wall, in world Y.
pub(super) fn wall_span(model: &ArchModel, wall: &Wall) -> Result<Span, HeightError> {
    let level = model.level(wall.level).ok_or(HeightError::UnknownLevel(wall.level))?;
    let plane = level_plane_y(model, wall.level).ok_or(HeightError::UnknownLevel(wall.level))?;

    let base = slab_support(model, wall);
    let top = resolve_top(wall.height, level.height, base);
    if top - base < MIN_WALL_H {
        return Err(HeightError::WallTooShort(top - base));
    }
    Ok(Span { base: plane + base, top: plane + top })
}

/// The vertical extent of a slab. `elevation` is the walking surface and the
/// solid hangs below it, which is why a default 50 mm slab on level 0 occupies
/// `[0.0, 0.05]` rather than `[-0.05, 0.0]`.
pub(super) fn slab_span(model: &ArchModel, slab: &Slab) -> Result<Span, HeightError> {
    let plane = level_plane_y(model, slab.level).ok_or(HeightError::UnknownLevel(slab.level))?;
    let top = plane + slab.elevation;
    Ok(Span { base: top - slab.thickness, top })
}

/// The vertical extent of a ceiling. Absent elevation means the storey plane,
/// and the shell hangs below it so the room below keeps its full clear height.
pub(super) fn ceiling_span(model: &ArchModel, c: &Ceiling) -> Result<Span, HeightError> {
    let level = model.level(c.level).ok_or(HeightError::UnknownLevel(c.level))?;
    let plane = level_plane_y(model, c.level).ok_or(HeightError::UnknownLevel(c.level))?;
    let top = plane + c.elevation.unwrap_or(level.height);
    Ok(Span { base: top - CEILING_SHELL_THICKNESS, top })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::arch::ir::{Level, ModelSource, Polygon, WallId};

    const EPS: f32 = 1e-5;

    fn model(heights: &[(i32, f32)]) -> ArchModel {
        let mut m = ArchModel::new(ModelSource::PascalEditor);
        for (id, h) in heights {
            m.levels.push(Level { id: LevelId(*id), name: None, height: *h });
        }
        m.levels.sort_by_key(|l| l.id);
        m
    }

    fn wall(m: &mut ArchModel, level: i32, height: Option<f32>) -> WallId {
        m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(level),
            start: [0.0, 0.0],
            end: [4.0, 0.0],
            thickness: 0.2,
            height,
            curve_offset: None,
            openings: Vec::new(),
            material: None,
        })
    }

    fn rect(x0: f32, z0: f32, x1: f32, z1: f32) -> Polygon {
        Polygon { outer: vec![[x0, z0], [x1, z0], [x1, z1], [x0, z1]], holes: vec![] }
    }

    fn slab(m: &mut ArchModel, level: i32, poly: Polygon, elevation: f32) {
        m.push_slab(Slab {
            id: super::super::ir::SlabId(0),
            level: LevelId(level),
            poly,
            elevation,
            thickness: 0.05,
            material: None,
        });
    }

    #[test]
    fn storeys_stack_by_floor_to_floor_height() {
        let m = model(&[(0, 2.7), (1, 2.7), (2, 3.0)]);
        assert_eq!(level_plane_y(&m, LevelId(0)), Some(0.0));
        assert_eq!(level_plane_y(&m, LevelId(1)), Some(2.7));
        assert_eq!(level_plane_y(&m, LevelId(2)), Some(5.4));
    }

    #[test]
    fn a_basement_ceiling_lands_on_the_ground_plane() {
        // The check that catches a sign error: level −1 must hang below the
        // datum by its own height, so its top is exactly Y = 0.
        let m = model(&[(-1, 2.4), (0, 2.7)]);
        let plane = level_plane_y(&m, LevelId(-1)).expect("level exists");
        assert!((plane + 2.4).abs() < EPS, "got {plane}");
        assert!((plane + 2.4 - 0.0).abs() < EPS);
    }

    #[test]
    fn two_basements_stack_downward() {
        let m = model(&[(-2, 2.4), (-1, 2.4), (0, 2.7)]);
        assert!((level_plane_y(&m, LevelId(-1)).unwrap() + 2.4).abs() < EPS);
        assert!((level_plane_y(&m, LevelId(-2)).unwrap() + 4.8).abs() < EPS);
    }

    #[test]
    fn an_unknown_level_has_no_plane() {
        let m = model(&[(0, 2.7)]);
        assert_eq!(level_plane_y(&m, LevelId(3)), None);
    }

    #[test]
    fn a_plane_bound_wall_reaches_the_storey_plane() {
        // No slab: the wall runs the whole floor-to-floor height.
        let mut m = model(&[(0, 2.7)]);
        let id = wall(&mut m, 0, None);
        let span = wall_span(&m, &m.walls[id.0 as usize]).expect("resolves");
        assert!((span.base - 0.0).abs() < EPS, "{span:?}");
        assert!((span.top - 2.7).abs() < EPS, "{span:?}");
    }

    #[test]
    fn a_slab_makes_a_plane_bound_wall_shorter_not_taller() {
        // The inversion that matters. The top stays pinned at the storey
        // plane; the base rises to the slab. Getting this backwards opens a
        // gap at every floor division.
        let mut m = model(&[(0, 2.7)]);
        slab(&mut m, 0, rect(-1.0, -1.0, 5.0, 1.0), 0.05);
        let id = wall(&mut m, 0, None);
        let span = wall_span(&m, &m.walls[id.0 as usize]).expect("resolves");
        assert!((span.base - 0.05).abs() < EPS, "{span:?}");
        assert!((span.top - 2.7).abs() < EPS, "top must not move: {span:?}");
        assert!((span.height() - 2.65).abs() < EPS, "{span:?}");
    }

    #[test]
    fn an_explicit_height_rides_a_raised_base() {
        let mut m = model(&[(0, 2.7)]);
        slab(&mut m, 0, rect(-1.0, -1.0, 5.0, 1.0), 0.05);
        let id = wall(&mut m, 0, Some(1.1)); // a parapet
        let span = wall_span(&m, &m.walls[id.0 as usize]).expect("resolves");
        assert!((span.base - 0.05).abs() < EPS, "{span:?}");
        assert!((span.top - 1.15).abs() < EPS, "base + height: {span:?}");
    }

    #[test]
    fn an_explicit_height_without_a_base_is_measured_from_the_plane() {
        // Their legacy branch: with no slab under it, an explicit height is
        // absolute rather than relative.
        let mut m = model(&[(0, 2.7)]);
        let id = wall(&mut m, 0, Some(1.1));
        let span = wall_span(&m, &m.walls[id.0 as usize]).expect("resolves");
        assert!((span.top - 1.1).abs() < EPS, "{span:?}");
    }

    #[test]
    fn an_upper_storey_wall_sits_on_its_own_plane() {
        let mut m = model(&[(0, 2.7), (1, 2.7)]);
        let id = wall(&mut m, 1, None);
        let span = wall_span(&m, &m.walls[id.0 as usize]).expect("resolves");
        assert!((span.base - 2.7).abs() < EPS, "{span:?}");
        assert!((span.top - 5.4).abs() < EPS, "{span:?}");
    }

    #[test]
    fn a_slab_almost_at_the_storey_plane_is_reported_not_emitted() {
        let mut m = model(&[(0, 2.7)]);
        slab(&mut m, 0, rect(-1.0, -1.0, 5.0, 1.0), 2.69);
        let id = wall(&mut m, 0, None);
        match wall_span(&m, &m.walls[id.0 as usize]) {
            Err(HeightError::WallTooShort(h)) => assert!((h - 0.01).abs() < EPS, "got {h}"),
            other => panic!("expected a too-short report, got {other:?}"),
        }
    }

    #[test]
    fn a_wall_on_a_missing_level_is_reported() {
        let mut m = model(&[(0, 2.7)]);
        let id = wall(&mut m, 4, None);
        assert_eq!(
            wall_span(&m, &m.walls[id.0 as usize]),
            Err(HeightError::UnknownLevel(LevelId(4)))
        );
    }

    #[test]
    fn a_wall_beside_a_slab_does_not_stand_on_it() {
        // The slab is nowhere near the wall, so it must not be elected.
        let mut m = model(&[(0, 2.7)]);
        slab(&mut m, 0, rect(20.0, 20.0, 26.0, 24.0), 0.4);
        let id = wall(&mut m, 0, None);
        assert!((slab_support(&m, &m.walls[id.0 as usize]) - 0.0).abs() < EPS);
    }

    #[test]
    fn a_slab_clipping_one_corner_does_not_decide_the_whole_wall() {
        // The wall runs x ∈ [0, 4]; this slab catches only its first 20 mm,
        // well under the minimum overlap.
        let mut m = model(&[(0, 2.7)]);
        slab(&mut m, 0, rect(-1.0, -1.0, 0.02, 1.0), 0.5);
        let id = wall(&mut m, 0, None);
        assert!((slab_support(&m, &m.walls[id.0 as usize]) - 0.0).abs() < EPS);
    }

    #[test]
    fn the_higher_of_two_covering_slabs_wins() {
        // A wall over a plinth stands on the plinth, not the floor beside it.
        let mut m = model(&[(0, 2.7)]);
        slab(&mut m, 0, rect(-1.0, -1.0, 5.0, 1.0), 0.05);
        slab(&mut m, 0, rect(-1.0, -1.0, 5.0, 1.0), 0.30);
        let id = wall(&mut m, 0, None);
        assert!((slab_support(&m, &m.walls[id.0 as usize]) - 0.30).abs() < EPS);
    }

    #[test]
    fn two_half_slabs_at_one_elevation_support_a_wall_together() {
        // Neither half covers a majority alone; pooled by elevation they do.
        // Without the union step this wall would fall through to the plane.
        let mut m = model(&[(0, 2.7)]);
        slab(&mut m, 0, rect(-1.0, -1.0, 2.0, 1.0), 0.20);
        slab(&mut m, 0, rect(2.0, -1.0, 5.0, 1.0), 0.20);
        let id = wall(&mut m, 0, None);
        assert!((slab_support(&m, &m.walls[id.0 as usize]) - 0.20).abs() < EPS);
    }

    #[test]
    fn slab_election_does_not_depend_on_declaration_order() {
        let build = |reverse: bool| {
            let mut m = model(&[(0, 2.7)]);
            let specs = [(rect(-1.0, -1.0, 3.0, 1.0), 0.05), (rect(1.0, -1.0, 5.0, 1.0), 0.30)];
            for (poly, elev) in if reverse {
                vec![specs[1].clone(), specs[0].clone()]
            } else {
                specs.to_vec()
            } {
                slab(&mut m, 0, poly, elev);
            }
            let id = wall(&mut m, 0, None);
            slab_support(&m, &m.walls[id.0 as usize])
        };
        assert_eq!(build(false), build(true));
    }

    #[test]
    fn a_slab_hole_removes_support() {
        // A stairwell opening under the wall means nothing holds it up there.
        let mut m = model(&[(0, 2.7)]);
        let mut poly = rect(-1.0, -1.0, 5.0, 1.0);
        poly.holes.push(vec![[-0.5, -0.5], [4.5, -0.5], [4.5, 0.5], [-0.5, 0.5]]);
        m.push_slab(Slab {
            id: super::super::ir::SlabId(0),
            level: LevelId(0),
            poly,
            elevation: 0.4,
            thickness: 0.05,
            material: None,
        });
        let id = wall(&mut m, 0, None);
        assert!((slab_support(&m, &m.walls[id.0 as usize]) - 0.0).abs() < EPS);
    }

    #[test]
    fn a_default_slab_occupies_the_first_fifty_millimetres() {
        let mut m = model(&[(0, 2.7)]);
        slab(&mut m, 0, rect(0.0, 0.0, 4.0, 4.0), 0.05);
        let span = slab_span(&m, &m.slabs[0]).expect("resolves");
        assert!((span.base - 0.0).abs() < EPS, "{span:?}");
        assert!((span.top - 0.05).abs() < EPS, "walking surface on top: {span:?}");
    }

    #[test]
    fn an_upper_storey_slab_sits_on_its_own_plane() {
        let mut m = model(&[(0, 2.7), (1, 2.7)]);
        slab(&mut m, 1, rect(0.0, 0.0, 4.0, 4.0), 0.05);
        let span = slab_span(&m, &m.slabs[0]).expect("resolves");
        assert!((span.top - 2.75).abs() < EPS, "{span:?}");
    }

    #[test]
    fn a_ceiling_defaults_to_the_storey_plane_and_hangs_below_it() {
        let mut m = model(&[(0, 2.7)]);
        m.push_ceiling(Ceiling {
            id: super::super::ir::CeilingId(0),
            level: LevelId(0),
            poly: rect(0.0, 0.0, 4.0, 4.0),
            elevation: None,
            material: None,
        });
        let span = ceiling_span(&m, &m.ceilings[0]).expect("resolves");
        assert!((span.top - 2.7).abs() < EPS, "{span:?}");
        assert!(span.base < span.top, "the shell hangs below: {span:?}");
        assert!((span.height() - CEILING_SHELL_THICKNESS).abs() < EPS);
    }

    #[test]
    fn a_curved_wall_is_sampled_along_its_arc_not_its_chord() {
        // The wall bows out to z = −1.2 at its midpoint. The slab sits under
        // the belly of that arc and nowhere near the chord, so measuring along
        // the chord would find no support at all and drop the wall to the
        // level plane.
        let mut m = model(&[(0, 2.7)]);
        slab(&mut m, 0, rect(-1.0, -1.3, 5.0, -0.5), 0.4);
        let id = m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(0),
            start: [0.0, 0.0],
            end: [4.0, 0.0],
            thickness: 0.2,
            height: None,
            curve_offset: Some(1.2),
            openings: Vec::new(),
            material: None,
        });
        assert!(
            (slab_support(&m, &m.walls[id.0 as usize]) - 0.4).abs() < EPS,
            "the arc's belly is supported; the chord is not"
        );
    }

    #[test]
    fn a_slab_clipping_a_long_wall_cannot_hoist_it() {
        // The counterpart to the 4 m case, and the reason samples are spaced
        // rather than counted: on a 40 m wall a fixed sample count would make
        // each sample stand for more than SLAB_MIN_OVERLAP, so this 20 mm
        // clip would read as adequate support.
        let mut m = model(&[(0, 2.7)]);
        slab(&mut m, 0, rect(-1.0, -1.0, 0.02, 1.0), 0.5);
        let id = m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(0),
            start: [0.0, 0.0],
            end: [40.0, 0.0],
            thickness: 0.2,
            height: None,
            curve_offset: None,
            openings: Vec::new(),
            material: None,
        });
        assert!((slab_support(&m, &m.walls[id.0 as usize]) - 0.0).abs() < EPS);
    }
}
