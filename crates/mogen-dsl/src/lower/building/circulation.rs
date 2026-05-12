//! Circulation planner. Reserves XY positions for stairs and elevators
//! that line up across every storey, so the floorplan solver can route
//! rooms around them and the emit pass can stamp the same module instances
//! at consistent locations on every floor.
//!
//! Strategy:
//!
//! - Without a corridor (grid / apartment-block / radial / organic / maze):
//!   stack all circulation cells in a single column along the east edge of
//!   the floorplate, packed from south to north.
//! - With a corridor (hotel-corridor / office-core): place the stair just
//!   south of the corridor and the elevator just north so each shares an
//!   edge with the corridor — the door planner will then run a door
//!   straight from the corridor into the cab. Extra stairs / elevators
//!   stack outward from the corridor on their side.
//!
//! Both strategies keep XY consistent across every storey so a stair on
//! floor N lands directly above the same XY on floor N-1.

use super::config::BuildingCfg;
use super::layout::Rect2;

const STAIR_WIDTH: f32 = 2.0;
// Switchback stairs need depth for two half-flights plus an entry/exit
// platform and a 180° mid-landing. 4 m breaks down (see
// `emit::circulation`) into a 1 m south entry zone (the bottom/top
// platform that connects to the floor slab), a 2 m flight zone, and a
// 1 m north mid-landing — comfortable enough for two ~8-step flights
// at ~0.18 m rise per step.
const STAIR_DEPTH: f32 = 4.0;
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

pub(super) fn plan(
    cfg: &BuildingCfg,
    bounds: Rect2,
    corridor_z: Option<(f32, f32)>,
) -> CirculationPlan {
    if cfg.staircases == 0 && cfg.elevators == 0 {
        return CirculationPlan::default();
    }
    let column_w = STAIR_WIDTH.max(ELEVATOR_WIDTH);
    let column_x_max = bounds.x_max;
    let column_x_min = bounds.x_max - column_w;

    let mut cells = Vec::new();

    if let Some((corridor_z_min, corridor_z_max)) = corridor_z {
        // Pack stairs growing southward from the corridor's south wall and
        // elevators growing northward from the corridor's north wall, so
        // every cab shares an edge with the corridor and the door planner
        // can stamp a door straight from the corridor into the shaft.
        let mut south_cursor = corridor_z_min;
        for _ in 0..cfg.staircases {
            let z_min = south_cursor - STAIR_DEPTH;
            if z_min < bounds.z_min + COLUMN_INSET {
                break;
            }
            cells.push(CirculationCell {
                rect: Rect2 {
                    x_min: column_x_min,
                    x_max: column_x_max,
                    z_min,
                    z_max: south_cursor,
                },
                kind: CirculationKind::Staircase,
            });
            south_cursor = z_min;
        }
        let mut north_cursor = corridor_z_max;
        for _ in 0..cfg.elevators {
            let z_max = north_cursor + ELEVATOR_DEPTH;
            if z_max > bounds.z_max - COLUMN_INSET {
                break;
            }
            cells.push(CirculationCell {
                rect: Rect2 {
                    x_min: column_x_min,
                    x_max: column_x_max,
                    z_min: north_cursor,
                    z_max,
                },
                kind: CirculationKind::Elevator,
            });
            north_cursor = z_max;
        }
        return CirculationPlan {
            cells,
            column_width: column_w,
        };
    }

    // No corridor: stack stairs first (closest to the south entrance for
    // accessibility), then elevators above, with insets so consecutive
    // cells don't share a wall (the column has no other cells alongside).
    let mut z_cursor = bounds.z_min + COLUMN_INSET;
    for _ in 0..cfg.staircases {
        let z_max = z_cursor + STAIR_DEPTH;
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
