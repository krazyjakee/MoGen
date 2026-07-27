//! Model checks that run before any geometry is built.
//!
//! These exist to fail loudly and early. The alternative — letting a malformed
//! model reach the triangulator — fails *silently*, because `extrude_mesh`
//! swallows earcut errors and returns a capless mesh. A model rejected here
//! costs a diagnostic; one that slips through costs a hole nobody notices.

use super::consts::MIN_WALL_H;
use super::ir::{ArchModel, LevelId};
use super::plan;

/// A problem with the model itself, before solving.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ModelError {
    /// Ids must equal `Vec` indices — the invariant that keeps hash maps out of
    /// the solver and thus keeps output deterministic.
    IdNotDense { kind: &'static str, index: usize, id: u32 },
    LevelsNotSorted { at: usize },
    DuplicateLevel(LevelId),
    UnknownLevel { kind: &'static str, index: usize, level: LevelId },
    NonPositiveThickness { kind: &'static str, index: usize, value: f32 },
    DegenerateWall { index: usize },
    RingTooShort { kind: &'static str, index: usize, points: usize },
    RingSelfIntersects { kind: &'static str, index: usize },
    OpeningOutsideWall { wall: usize, opening: usize },
}

/// Check a model is well formed. Returns every problem found, not just the
/// first — a converter fixing one field at a time is a slow way to work.
pub(crate) fn check_model(m: &ArchModel) -> Vec<ModelError> {
    let mut errs = Vec::new();

    for (i, w) in m.walls.iter().enumerate() {
        if w.id.0 as usize != i {
            errs.push(ModelError::IdNotDense { kind: "wall", index: i, id: w.id.0 });
        }
    }
    for (i, s) in m.slabs.iter().enumerate() {
        if s.id.0 as usize != i {
            errs.push(ModelError::IdNotDense { kind: "slab", index: i, id: s.id.0 });
        }
    }
    for (i, c) in m.ceilings.iter().enumerate() {
        if c.id.0 as usize != i {
            errs.push(ModelError::IdNotDense { kind: "ceiling", index: i, id: c.id.0 });
        }
    }
    for (i, r) in m.roofs.iter().enumerate() {
        if r.id.0 as usize != i {
            errs.push(ModelError::IdNotDense { kind: "roof", index: i, id: r.id.0 });
        }
    }

    // Levels ascend, without repeats — the height prefix sum depends on it.
    for i in 1..m.levels.len() {
        if m.levels[i].id <= m.levels[i - 1].id {
            if m.levels[i].id == m.levels[i - 1].id {
                errs.push(ModelError::DuplicateLevel(m.levels[i].id));
            } else {
                errs.push(ModelError::LevelsNotSorted { at: i });
            }
        }
    }

    let known = |l: LevelId| m.levels.iter().any(|lv| lv.id == l);

    for (i, w) in m.walls.iter().enumerate() {
        if !known(w.level) {
            errs.push(ModelError::UnknownLevel { kind: "wall", index: i, level: w.level });
        }
        if w.thickness <= 0.0 {
            errs.push(ModelError::NonPositiveThickness {
                kind: "wall",
                index: i,
                value: w.thickness,
            });
        }
        let chord = plan::distance(w.start, w.end);
        if chord < MIN_WALL_H {
            errs.push(ModelError::DegenerateWall { index: i });
            // Opening checks below would be meaningless on a zero-length wall.
            continue;
        }
        let span = super::curve::centreline_length(w.start, w.end, w.curve_offset);
        for (j, o) in w.openings.iter().enumerate() {
            let half = o.width * 0.5;
            if o.width <= 0.0 || o.height <= 0.0 || o.along - half < -1e-3 || o.along + half > span + 1e-3
            {
                errs.push(ModelError::OpeningOutsideWall { wall: i, opening: j });
            }
        }
    }

    for (i, s) in m.slabs.iter().enumerate() {
        if !known(s.level) {
            errs.push(ModelError::UnknownLevel { kind: "slab", index: i, level: s.level });
        }
        if s.thickness <= 0.0 {
            errs.push(ModelError::NonPositiveThickness {
                kind: "slab",
                index: i,
                value: s.thickness,
            });
        }
        check_rings("slab", i, &s.poly, &mut errs);
    }

    for (i, c) in m.ceilings.iter().enumerate() {
        if !known(c.level) {
            errs.push(ModelError::UnknownLevel { kind: "ceiling", index: i, level: c.level });
        }
        check_rings("ceiling", i, &c.poly, &mut errs);
    }

    for (i, r) in m.roofs.iter().enumerate() {
        if !known(r.level) {
            errs.push(ModelError::UnknownLevel { kind: "roof", index: i, level: r.level });
        }
    }

    errs
}

fn check_rings(kind: &'static str, index: usize, poly: &super::ir::Polygon, errs: &mut Vec<ModelError>) {
    if poly.outer.len() < 3 {
        errs.push(ModelError::RingTooShort { kind, index, points: poly.outer.len() });
    } else if !plan::ring_is_simple(&poly.outer) {
        errs.push(ModelError::RingSelfIntersects { kind, index });
    }
    for h in &poly.holes {
        if h.len() < 3 {
            errs.push(ModelError::RingTooShort { kind, index, points: h.len() });
        } else if !plan::ring_is_simple(h) {
            errs.push(ModelError::RingSelfIntersects { kind, index });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::arch::ir::*;

    fn model() -> ArchModel {
        let mut m = ArchModel::new(ModelSource::PascalEditor);
        m.levels.push(Level { id: LevelId(0), name: None, height: 2.5 });
        m
    }

    fn wall(start: P2, end: P2) -> Wall {
        Wall {
            id: WallId(0),
            level: LevelId(0),
            start,
            end,
            thickness: 0.1,
            height: None,
            curve_offset: None,
            openings: Vec::new(),
            material: None,
        }
    }

    #[test]
    fn a_minimal_valid_model_passes() {
        let mut m = model();
        m.push_wall(wall([0.0, 0.0], [4.0, 0.0]));
        assert_eq!(check_model(&m), vec![]);
    }

    #[test]
    fn dense_ids_are_enforced() {
        let mut m = model();
        let mut w = wall([0.0, 0.0], [4.0, 0.0]);
        w.id = WallId(7); // pushed directly, bypassing push_wall
        m.walls.push(w);
        assert!(matches!(
            check_model(&m).as_slice(),
            [ModelError::IdNotDense { kind: "wall", index: 0, id: 7 }]
        ));
    }

    #[test]
    fn unsorted_levels_are_rejected() {
        let mut m = model();
        m.levels.push(Level { id: LevelId(-1), name: None, height: 2.5 });
        assert!(check_model(&m).iter().any(|e| matches!(e, ModelError::LevelsNotSorted { .. })));
    }

    #[test]
    fn zero_length_wall_is_degenerate() {
        let mut m = model();
        m.push_wall(wall([1.0, 1.0], [1.0, 1.0]));
        assert!(check_model(&m).iter().any(|e| matches!(e, ModelError::DegenerateWall { .. })));
    }

    #[test]
    fn wall_on_a_missing_level_is_reported() {
        let mut m = model();
        let mut w = wall([0.0, 0.0], [4.0, 0.0]);
        w.level = LevelId(3);
        m.push_wall(w);
        assert!(check_model(&m).iter().any(|e| matches!(e, ModelError::UnknownLevel { .. })));
    }

    #[test]
    fn opening_running_off_the_end_is_reported() {
        let mut m = model();
        let mut w = wall([0.0, 0.0], [4.0, 0.0]);
        w.openings.push(Opening {
            kind: OpeningKind::Door,
            along: 3.9,
            sill: 0.0,
            width: 0.9,
            height: 2.1,
        });
        m.push_wall(w);
        assert!(check_model(&m)
            .iter()
            .any(|e| matches!(e, ModelError::OpeningOutsideWall { wall: 0, opening: 0 })));
    }

    #[test]
    fn opening_within_the_span_is_fine() {
        let mut m = model();
        let mut w = wall([0.0, 0.0], [4.0, 0.0]);
        w.openings.push(Opening {
            kind: OpeningKind::Door,
            along: 2.0,
            sill: 0.0,
            width: 0.9,
            height: 2.1,
        });
        m.push_wall(w);
        assert_eq!(check_model(&m), vec![]);
    }

    #[test]
    fn self_intersecting_slab_ring_is_rejected() {
        let mut m = model();
        m.push_slab(Slab {
            id: SlabId(0),
            level: LevelId(0),
            poly: Polygon {
                // A bowtie — the shape that silently loses its caps.
                outer: vec![[0.0, 0.0], [1.0, 1.0], [1.0, 0.0], [0.0, 1.0]],
                holes: Vec::new(),
            },
            elevation: 0.05,
            thickness: 0.05,
            material: None,
        });
        assert!(check_model(&m)
            .iter()
            .any(|e| matches!(e, ModelError::RingSelfIntersects { kind: "slab", .. })));
    }

    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let mut m = model();
        let mut a = wall([0.0, 0.0], [0.0, 0.0]);
        a.thickness = -1.0;
        m.push_wall(a);
        m.push_wall(wall([2.0, 2.0], [2.0, 2.0]));
        assert!(check_model(&m).len() >= 3, "expected several errors");
    }
}
