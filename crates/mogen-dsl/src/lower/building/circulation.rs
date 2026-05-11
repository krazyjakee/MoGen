//! Circulation planner. Reserves XY positions for stairs and elevators
//! that line up across every storey, so the floorplan solver can route
//! rooms around them and the emit pass can stamp the same module instances
//! at consistent locations on every floor.
//!
//! T2 strategy: stack all circulation cells in a single column along the
//! east edge of the floorplate. Simple, deterministic, and avoids the
//! "carve a non-rectangular hole out of the BSP" problem entirely — the
//! room-layout area is just a smaller rectangle.

use super::config::BuildingCfg;
use super::layout::Rect2;

const STAIR_WIDTH: f32 = 2.0;
const STAIR_DEPTH: f32 = 3.0;
const ELEVATOR_WIDTH: f32 = 2.0;
const ELEVATOR_DEPTH: f32 = 2.0;
const COLUMN_INSET: f32 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CirculationKind {
    Staircase,
    Elevator,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CirculationCell {
    pub rect: Rect2,
    pub kind: CirculationKind,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CirculationPlan {
    pub cells: Vec<CirculationCell>,
    /// Column width reserved on the east edge. `0.0` if no circulation.
    pub column_width: f32,
}

impl CirculationPlan {
    pub fn has_any(&self) -> bool {
        !self.cells.is_empty()
    }

    // staircases() / elevators() helpers will be needed once T3's
    // adjacency scoring hooks into circulation; keep CirculationKind+
    // CirculationCell on the surface so they're reachable now without
    // having to widen visibility later.
}

pub(super) fn plan(cfg: &BuildingCfg, bounds: Rect2) -> CirculationPlan {
    if cfg.staircases == 0 && cfg.elevators == 0 {
        return CirculationPlan::default();
    }
    let column_w = STAIR_WIDTH.max(ELEVATOR_WIDTH);
    let column_x_max = bounds.x_max;
    let column_x_min = bounds.x_max - column_w;

    let mut cells = Vec::new();
    let mut z_cursor = bounds.z_min + COLUMN_INSET;

    // Stairs go in first (closest to the south entrance for accessibility),
    // then elevators stacked above.
    for _ in 0..cfg.staircases {
        let z_max = z_cursor + STAIR_DEPTH;
        if z_max > bounds.z_max - COLUMN_INSET {
            // Out of vertical room — drop remaining circulation. T3 will
            // grow the floorplate to fit.
            break;
        }
        cells.push(CirculationCell {
            rect: Rect2 {
                x_min: column_x_min,
                x_max: column_x_max,
                z_min: z_cursor,
                z_max,
            },
            kind: CirculationKind::Staircase,
        });
        z_cursor = z_max + COLUMN_INSET;
    }
    for _ in 0..cfg.elevators {
        let z_max = z_cursor + ELEVATOR_DEPTH;
        if z_max > bounds.z_max - COLUMN_INSET {
            break;
        }
        cells.push(CirculationCell {
            rect: Rect2 {
                x_min: column_x_min,
                x_max: column_x_max,
                z_min: z_cursor,
                z_max,
            },
            kind: CirculationKind::Elevator,
        });
        z_cursor = z_max + COLUMN_INSET;
    }

    CirculationPlan {
        cells,
        column_width: column_w,
    }
}
