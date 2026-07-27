//! `solve` — the one way in.
//!
//! Everything above this file answers a question about a building. This answers
//! *the* question: given an [`ArchModel`], what solids exist and where? Both
//! sinks call this and nothing else, which is what stops geometry decisions
//! leaking into them.
//!
//! The order of operations matters and is not arbitrary:
//!
//! 1. **Validate first.** A model with a duplicate level or a wall on a level
//!    that does not exist produces geometry that looks fine in isolation, so
//!    the check has to come before anything is built, not after.
//! 2. **Mitre per level**, because a ground-floor wall must not join a
//!    first-floor one that happens to share its plan position.
//! 3. **Resolve heights**, which needs the slabs but not the footprints.
//! 4. **Slice each wall** by its openings and emit one solid per surviving
//!    panel.
//!
//! Nothing here is fatal. A wall that cannot be solved is dropped with a
//! warning naming it, because half a building is more useful to look at than an
//! error message — and the warnings ride along in [`ResolvedGeometry`] so the
//! importer can put them in the generated file's header.

use super::height::{self, HeightError};
use super::ir::{ArchModel, LevelId, Opening, Polygon, Wall};
use super::miter::{self, FootprintError, JunctionFiller, WallFootprint};
use super::openings::solid_panels;
use super::resolved::{Placement, ResolvedGeometry, Role, Shape, Solid};
use super::roof;
use super::validate;
use super::{curve, junction};

/// Turn a model into geometry.
pub(super) fn solve(model: &ArchModel) -> ResolvedGeometry {
    let mut out = ResolvedGeometry::default();

    for problem in validate::check_model(model) {
        out.warnings.push(format!("model: {problem:?}"));
    }

    // Markers are producer data. The solver passes them through untouched --
    // an importer turning a sofa into an anchor already knows where the sofa
    // is, and no amount of geometry solving would rediscover it.
    out.markers = model.markers.clone();

    // Levels are solved independently, in id order so the output never depends
    // on the order elements were declared in.
    let mut levels: Vec<LevelId> = model.levels.iter().map(|l| l.id).collect();
    levels.sort_unstable();

    for level in levels {
        let solution = miter::solve_level(&model.walls, level);

        for (wall, err) in &solution.rejected {
            out.warnings.push(match err {
                FootprintError::Degenerate => {
                    format!("wall {}: zero length or thickness, dropped", wall.0)
                }
                FootprintError::SelfIntersecting => format!(
                    "wall {}: curves tighter than its own thickness, dropped",
                    wall.0
                ),
            });
        }

        for footprint in &solution.footprints {
            let wall = &model.walls[footprint.wall.0 as usize];
            emit_wall(model, wall, footprint, &mut out);
        }
        for filler in &solution.fillers {
            emit_filler(model, filler, level, &mut out);
        }
    }

    for slab in &model.slabs {
        emit_prism(
            format!("slab_{}", slab.id.0),
            Role::Slab,
            slab.level,
            slab.poly.clone(),
            height::slab_span(model, slab),
            slab.material.clone(),
            &mut out,
        );
    }

    for ceiling in &model.ceilings {
        emit_prism(
            format!("ceiling_{}", ceiling.id.0),
            Role::Ceiling,
            ceiling.level,
            ceiling.poly.clone(),
            height::ceiling_span(model, ceiling),
            ceiling.material.clone(),
            &mut out,
        );
    }

    for seg in &model.roofs {
        let solids = roof::solids(model, seg);
        if solids.is_empty() {
            out.warnings.push(format!("roof {}: produced no geometry", seg.id.0));
        }
        out.solids.extend(solids);
    }

    out
}

/// Emit one wall as a set of panels — piers, sills, lintels — around its
/// openings.
fn emit_wall(model: &ArchModel, wall: &Wall, fp: &WallFootprint, out: &mut ResolvedGeometry) {
    let span = match height::wall_span(model, wall) {
        Ok(s) => s,
        Err(e) => {
            out.warnings.push(describe(wall, e));
            return;
        }
    };

    let (s, e) = junction::ends(wall);
    let length = curve::centreline_length(s, e, wall.curve_offset);
    let height_m = span.height();

    let panels = solid_panels(length, height_m, &holes(wall, length, height_m));
    if panels.is_empty() {
        out.warnings.push(format!("wall {}: entirely taken up by openings", wall.id.0));
        return;
    }

    for (i, panel) in panels.iter().enumerate() {
        // Panel X runs -length/2 .. +length/2; the footprint runs 0 .. 1.
        let t0 = (panel.x0 / length + 0.5).clamp(0.0, 1.0);
        let t1 = (panel.x1 / length + 0.5).clamp(0.0, 1.0);
        let ring = fp.slice(t0, t1);

        // Panel Y is measured from the wall's mid-height.
        let mid = span.base + 0.5 * height_m;
        let shape = Shape::Prism {
            poly: Polygon { outer: ring, holes: Vec::new() },
            base: mid + panel.y0,
            top: mid + panel.y1,
        };
        if let Err(err) = shape.check() {
            out.warnings.push(format!("wall {} panel {i}: {err:?}, dropped", wall.id.0));
            continue;
        }

        out.solids.push(Solid {
            // Single-panel walls keep the plain name, so an unbroken wall is
            // not renamed the moment someone adds a window to it.
            name: if panels.len() == 1 {
                format!("wall_{}", wall.id.0)
            } else {
                format!("wall_{}_panel{i}", wall.id.0)
            },
            role: Role::Wall,
            level: wall.level,
            shape,
            placement: Placement::IDENTITY,
            material: wall.material.clone(),
        });
    }
}

/// Convert the IR's openings into the `[along, centre_y, w, h]` holes
/// [`solid_panels`] expects.
///
/// Two conversions, both easy to get backwards. `along` is measured from the
/// wall's start but the panel frame is centred, and `sill` is the opening's
/// bottom edge while the panel frame wants its centre.
fn holes(wall: &Wall, length: f32, height_m: f32) -> Vec<[f32; 4]> {
    wall.openings
        .iter()
        .map(|o: &Opening| {
            [
                o.along - 0.5 * length,
                o.sill + 0.5 * o.height - 0.5 * height_m,
                o.width,
                o.height,
            ]
        })
        .collect()
}

/// Patch a butt-jointed corner, over the range where both its walls exist.
fn emit_filler(
    model: &ArchModel,
    filler: &JunctionFiller,
    level: LevelId,
    out: &mut ResolvedGeometry,
) {
    let spans: Vec<_> = filler
        .walls
        .iter()
        .filter_map(|w| height::wall_span(model, &model.walls[w.0 as usize]).ok())
        .collect();
    if spans.len() != 2 {
        // Both walls have to resolve for the notch between them to mean
        // anything; if one was dropped there is nothing to patch against.
        return;
    }

    let base = spans[0].base.max(spans[1].base);
    let top = spans[0].top.min(spans[1].top);
    let shape = Shape::Prism {
        poly: Polygon { outer: filler.ring.clone(), holes: Vec::new() },
        base,
        top,
    };
    if shape.check().is_err() {
        return;
    }

    out.solids.push(Solid {
        name: format!("wall_joint_{}_{}", filler.walls[0].0, filler.walls[1].0),
        role: Role::WallJoint,
        level,
        shape,
        placement: Placement::IDENTITY,
        material: model.walls[filler.walls[0].0 as usize].material.clone(),
    });
}

fn emit_prism(
    name: String,
    role: Role,
    level: LevelId,
    poly: Polygon,
    span: Result<height::Span, HeightError>,
    material: Option<super::ir::MatRef>,
    out: &mut ResolvedGeometry,
) {
    let span = match span {
        Ok(s) => s,
        Err(e) => {
            out.warnings.push(format!("{name}: {e:?}, dropped"));
            return;
        }
    };
    let shape = Shape::Prism { poly, base: span.base, top: span.top };
    if let Err(err) = shape.check() {
        out.warnings.push(format!("{name}: {err:?}, dropped"));
        return;
    }
    out.solids.push(Solid { name, role, level, shape, placement: Placement::IDENTITY, material });
}

fn describe(wall: &Wall, e: HeightError) -> String {
    match e {
        HeightError::UnknownLevel(l) => {
            format!("wall {}: level {} does not exist, dropped", wall.id.0, l.0)
        }
        HeightError::WallTooShort(h) => {
            format!("wall {}: resolves to {h:.3}m tall, dropped", wall.id.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::arch::ir::{
        Ceiling, CeilingId, Level, MatRef, ModelSource, OpeningKind, RoofId, RoofParams,
        RoofSegment, RoofType, Slab, SlabId, WallId,
    };
    use crate::lower::arch::plan;

    fn model() -> ArchModel {
        let mut m = ArchModel::new(ModelSource::PascalEditor);
        m.levels.push(Level { id: LevelId(0), name: None, height: 2.7 });
        m
    }

    fn wall(m: &mut ArchModel, start: [f32; 2], end: [f32; 2]) -> WallId {
        m.push_wall(Wall {
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

    fn room(m: &mut ArchModel, w: f32, d: f32) -> Vec<WallId> {
        let c = [[0.0, 0.0], [w, 0.0], [w, d], [0.0, d]];
        (0..4).map(|i| wall(m, c[i], c[(i + 1) % 4])).collect()
    }

    fn named<'a>(g: &'a ResolvedGeometry, name: &str) -> &'a Solid {
        g.solids
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no solid {name:?}; have {:?}", names(g)))
    }

    fn names(g: &ResolvedGeometry) -> Vec<&str> {
        g.solids.iter().map(|s| s.name.as_str()).collect()
    }

    fn prism(s: &Solid) -> (&Polygon, f32, f32) {
        match &s.shape {
            Shape::Prism { poly, base, top } => (poly, *base, *top),
            other => panic!("expected a prism, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_model_solves_to_nothing() {
        let g = solve(&model());
        assert!(g.solids.is_empty() && g.warnings.is_empty());
    }

    #[test]
    fn a_bare_room_gives_one_solid_per_wall() {
        let mut m = model();
        room(&mut m, 6.0, 4.0);
        let g = solve(&m);
        assert_eq!(g.solids.len(), 4, "{:?}", names(&g));
        assert!(g.warnings.is_empty(), "{:?}", g.warnings);
        for s in &g.solids {
            let (_, base, top) = prism(s);
            assert!((base - 0.0).abs() < 1e-5 && (top - 2.7).abs() < 1e-5, "{s:?}");
        }
    }

    #[test]
    fn everything_solve_emits_is_a_closed_solid() {
        // The blanket guarantee the sinks rely on.
        let mut m = model();
        m.levels.push(Level { id: LevelId(1), name: None, height: 2.7 });
        room(&mut m, 6.0, 4.0);
        m.walls[0].openings.push(Opening {
            kind: OpeningKind::Door,
            along: 3.0,
            sill: 0.0,
            width: 0.9,
            height: 2.1,
        });
        m.push_slab(Slab {
            id: SlabId(0),
            level: LevelId(0),
            poly: Polygon {
                outer: vec![[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]],
                holes: vec![],
            },
            elevation: 0.05,
            thickness: 0.05,
            material: None,
        });
        m.push_ceiling(Ceiling {
            id: CeilingId(0),
            level: LevelId(0),
            poly: Polygon {
                outer: vec![[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]],
                holes: vec![],
            },
            elevation: None,
            material: None,
        });
        m.push_roof(RoofSegment {
            id: RoofId(0),
            level: LevelId(0),
            centre: [3.0, 2.0],
            width: 6.0,
            depth: 4.0,
            rotation: 0.0,
            pitch_deg: 35.0,
            roof_type: RoofType::Gable,
            overhang: 0.4,
            wall_height: 2.7,
            params: RoofParams::default(),
            material: None,
        });

        let g = solve(&m);
        assert!(g.warnings.is_empty(), "{:?}", g.warnings);
        assert!(g.check().is_empty(), "{:?}", g.check());
        assert!(g.solids.len() > 6, "{:?}", names(&g));
    }

    #[test]
    fn a_door_splits_its_wall_into_panels() {
        let mut m = model();
        let ids = room(&mut m, 6.0, 4.0);
        m.walls[ids[0].0 as usize].openings.push(Opening {
            kind: OpeningKind::Door,
            along: 3.0,
            sill: 0.0,
            width: 0.9,
            height: 2.1,
        });

        let g = solve(&m);
        let panels: Vec<_> = names(&g).into_iter().filter(|n| n.starts_with("wall_0")).collect();
        // Two piers and a lintel; no sill, because the door meets the floor.
        assert_eq!(panels.len(), 3, "{panels:?}");
        assert!(g.check().is_empty(), "{:?}", g.check());
    }

    #[test]
    fn an_unbroken_wall_keeps_its_plain_name() {
        // Adding a window to one wall must not rename the others.
        let mut m = model();
        room(&mut m, 6.0, 4.0);
        let g = solve(&m);
        assert!(names(&g).contains(&"wall_0"), "{:?}", names(&g));
    }

    #[test]
    fn a_pier_beside_a_corner_still_reaches_the_mitre() {
        // The reason slices are taken from the two sides rather than the ring:
        // the outermost panel has to inherit the corner the mitre solved, or a
        // wall with a door in it stops meeting its neighbour.
        let mut m = model();
        let ids = room(&mut m, 6.0, 4.0);
        m.walls[ids[0].0 as usize].openings.push(Opening {
            kind: OpeningKind::Door,
            along: 3.0,
            sill: 0.0,
            width: 0.9,
            height: 2.1,
        });

        let g = solve(&m);
        let bare = solve(&{
            let mut plain = model();
            room(&mut plain, 6.0, 4.0);
            plain
        });

        // The intact wall_1 shares two corners with wall_0's outer piers.
        let neighbour = prism(named(&bare, "wall_1")).0.outer.clone();
        let corner = neighbour
            .iter()
            .copied()
            .find(|p| plan::distance(*p, [6.0, 0.0]) < 0.3)
            .expect("wall_1 has a corner near (6, 0)");

        let touches = g
            .solids
            .iter()
            .filter(|s| s.name.starts_with("wall_0"))
            .any(|s| prism(s).0.outer.iter().any(|p| plan::distance(*p, corner) < 1e-5));
        assert!(touches, "no panel of wall_0 reaches the mitre at {corner:?}");
    }

    #[test]
    fn a_wall_fully_taken_up_by_an_opening_is_reported() {
        let mut m = model();
        let id = wall(&mut m, [0.0, 0.0], [1.0, 0.0]);
        m.walls[id.0 as usize].openings.push(Opening {
            kind: OpeningKind::Passage,
            along: 0.5,
            sill: 0.0,
            width: 4.0,
            height: 4.0,
        });
        let g = solve(&m);
        assert!(g.solids.is_empty());
        // Two independent reports, and both are wanted: the validator objects
        // to an opening wider than its wall, and the solver separately notices
        // nothing solid survived. Either alone could miss a case the other
        // catches, so neither subsumes the other.
        assert!(
            g.warnings.iter().any(|w| w.contains("OpeningOutsideWall")),
            "{:?}",
            g.warnings
        );
        assert!(
            g.warnings.iter().any(|w| w.contains("entirely taken up by openings")),
            "{:?}",
            g.warnings
        );
    }

    #[test]
    fn a_bad_wall_is_dropped_with_a_warning_not_a_failure() {
        // Half a building beats an error message.
        let mut m = model();
        room(&mut m, 6.0, 4.0);
        wall(&mut m, [2.0, 2.0], [2.0, 2.0]); // zero length
        let g = solve(&m);
        assert_eq!(g.solids.len(), 4, "the good walls still come through");
        assert!(g.warnings.iter().any(|w| w.contains("wall 4")), "{:?}", g.warnings);
    }

    #[test]
    fn levels_are_solved_independently() {
        // Same plan position on two storeys must not fuse into one wall.
        let mut m = model();
        m.levels.push(Level { id: LevelId(1), name: None, height: 2.7 });
        wall(&mut m, [0.0, 0.0], [4.0, 0.0]);
        m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(1),
            start: [4.0, 0.0],
            end: [4.0, 4.0],
            thickness: 0.2,
            height: None,
            curve_offset: None,
            openings: Vec::new(),
            material: Some(MatRef("upper".into())),
        });

        let g = solve(&m);
        assert_eq!(g.solids.len(), 2);
        let (_, base_lo, _) = prism(named(&g, "wall_0"));
        let (_, base_hi, _) = prism(named(&g, "wall_1"));
        assert!((base_lo - 0.0).abs() < 1e-5);
        assert!((base_hi - 2.7).abs() < 1e-5, "upper wall sits on its own plane");
    }

    #[test]
    fn a_slab_lifts_the_walls_that_stand_on_it() {
        let mut m = model();
        room(&mut m, 6.0, 4.0);
        m.push_slab(Slab {
            id: SlabId(0),
            level: LevelId(0),
            poly: Polygon {
                outer: vec![[-1.0, -1.0], [7.0, -1.0], [7.0, 5.0], [-1.0, 5.0]],
                holes: vec![],
            },
            elevation: 0.05,
            thickness: 0.05,
            material: None,
        });
        let g = solve(&m);
        let (_, base, top) = prism(named(&g, "wall_0"));
        assert!((base - 0.05).abs() < 1e-5, "{base}");
        assert!((top - 2.7).abs() < 1e-5, "the top stays put: {top}");
    }

    #[test]
    fn solving_is_reproducible() {
        let mut m = model();
        room(&mut m, 6.0, 4.0);
        m.walls[0].openings.push(Opening {
            kind: OpeningKind::Window,
            along: 2.0,
            sill: 0.9,
            width: 1.2,
            height: 1.0,
        });
        let a = solve(&m);
        let b = solve(&m);
        assert_eq!(names(&a), names(&b));
        assert_eq!(a.solids, b.solids);
    }

    #[test]
    fn a_curved_wall_keeps_its_arc_through_slicing() {
        let mut m = model();
        let id = m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(0),
            start: [0.0, 0.0],
            end: [6.0, 0.0],
            thickness: 0.2,
            height: None,
            curve_offset: Some(1.0),
            openings: vec![Opening {
                kind: OpeningKind::Window,
                along: 3.2,
                sill: 0.9,
                width: 1.2,
                height: 1.0,
            }],
            material: None,
        });
        let _ = id;

        let g = solve(&m);
        assert!(g.check().is_empty(), "{:?}", g.check());
        // A sliced panel of a curved wall must keep interior arc samples, not
        // collapse to a flat quad between its two ends.
        let biggest = g
            .solids
            .iter()
            .map(|s| prism(s).0.outer.len())
            .max()
            .expect("at least one panel");
        assert!(biggest > 4, "panels flattened to quads: {biggest}");
    }
}
