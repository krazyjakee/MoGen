//! Circulation planner. Reserves XY positions for stairs and elevators
//! that line up across every storey, so the floorplan solver can route
//! rooms around them and the emit pass can stamp the same module instances
//! at consistent locations on every floor.
//!
//! Strategy:
//!
//! - Without a corridor (grid / apartment-block / radial / organic / maze):
//!   stack all circulation cells in a single column along the east edge of
//!   the floorplate. The seed picks both the south-to-north order of the
//!   two kinds (stairs-first or elevators-first) and an offset that slides
//!   the whole stack along the available east-column slack.
//! - With a corridor (hotel-corridor / office-core): each kind shares an
//!   edge with the corridor (so the door planner can run a door straight
//!   from the corridor into each shaft); the seed picks which side of the
//!   corridor gets stairs and which gets elevators.
//!
//! Both strategies keep XY consistent across every storey so a stair on
//! floor N lands directly above the same XY on floor N-1.

use super::config::BuildingCfg;
use super::layout::Rect2;
use super::rng::{rand_f01, step};

const STAIR_WIDTH: f32 = 2.0;
// Switchback stairs need depth for two half-flights plus an entry/exit
// platform and a 180° mid-landing. 4 m breaks down (see
// `emit::circulation`) into a 1 m south entry zone (the bottom/top
// platform that connects to the floor slab), a 2 m flight zone, and a
// 1 m north mid-landing — comfortable enough for two ~8-step flights
// at ~0.18 m rise per step.
const STAIR_DEPTH: f32 = 4.0;

/// Depth (along Z) of the staircase's south entry/exit platform — the
/// flat slab at every storey's floor height that a user steps onto when
/// passing through a door into the stairwell. The stair flights begin
/// `STAIR_ENTRY_DEPTH` north of `rect.z_min`; everything north of that
/// is at half-storey height (mid-landing) or empty (flight cutout), so a
/// door whose anchor falls outside the entry zone opens into mid-air.
///
/// Lives on the planner side (not in `emit/`) because `layout` builds
/// `RoomCell::door_slots` from this value and `emit/openings.rs` clamps
/// door anchors against it; the geometry emitter in `emit/circulation`
/// merely realises the same slab the planner already promised.
pub(super) const STAIR_ENTRY_DEPTH: f32 = 1.0;

/// Depth (along Z) of the staircase's north mid-landing slab. Symmetric
/// counterpart to `STAIR_ENTRY_DEPTH`: a half-storey-high platform that
/// the two switchback flights meet at. Doors are never allowed onto this
/// strip because at storey-floor height there is no walkable surface.
pub(super) const STAIR_LANDING_DEPTH: f32 = 1.0;

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

/// Sub-seed tag mixed with `cfg.seed` so circulation decisions evolve
/// independently of room-layout RNG draws.
const CIRC_SEED_TAG: u32 = 0xC15C_0AAA;

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

    let mut state = cfg
        .seed
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(CIRC_SEED_TAG)
        .max(1);

    let mut cells = Vec::new();

    if let Some((corridor_z_min, corridor_z_max)) = corridor_z {
        // Both kinds sit flush against the corridor so the door planner
        // can stamp a door straight from the corridor into each shaft;
        // the seed chooses which side of the corridor gets stairs and
        // which gets elevators. Extra cells of the same kind stack
        // outward from the corridor on their side.
        let stairs_south = (step(&mut state) & 1) == 0;
        let (stair_anchor, stair_grows_north, elev_anchor, elev_grows_north) =
            if stairs_south {
                (corridor_z_min, false, corridor_z_max, true)
            } else {
                (corridor_z_max, true, corridor_z_min, false)
            };

        emit_corridor_stack(
            &mut cells,
            cfg.staircases,
            CirculationKind::Staircase,
            STAIR_DEPTH,
            stair_anchor,
            stair_grows_north,
            bounds,
            column_x_min,
            column_x_max,
        );
        emit_corridor_stack(
            &mut cells,
            cfg.elevators,
            CirculationKind::Elevator,
            ELEVATOR_DEPTH,
            elev_anchor,
            elev_grows_north,
            bounds,
            column_x_min,
            column_x_max,
        );
        return CirculationPlan {
            cells,
            column_width: column_w,
        };
    }

    // No corridor: the seed picks south-to-north order (stairs-first or
    // elevators-first) and a Z offset that slides the entire stack along
    // the available east-column slack. Cells in the stack abut directly,
    // separated only by `COLUMN_INSET` so they don't share a wall.
    let stairs_first = (step(&mut state) & 1) == 0;

    let n_cells = cfg.staircases.saturating_add(cfg.elevators);
    let stack_depth = (cfg.staircases as f32) * STAIR_DEPTH
        + (cfg.elevators as f32) * ELEVATOR_DEPTH
        + (n_cells.saturating_sub(1) as f32) * COLUMN_INSET;
    let usable = (bounds.z_max - bounds.z_min) - 2.0 * COLUMN_INSET;
    let slack = (usable - stack_depth).max(0.0);
    let z_start = bounds.z_min + COLUMN_INSET + slack * rand_f01(&mut state);

    let groups: [(CirculationKind, f32, u32); 2] = if stairs_first {
        [
            (CirculationKind::Staircase, STAIR_DEPTH, cfg.staircases),
            (CirculationKind::Elevator, ELEVATOR_DEPTH, cfg.elevators),
        ]
    } else {
        [
            (CirculationKind::Elevator, ELEVATOR_DEPTH, cfg.elevators),
            (CirculationKind::Staircase, STAIR_DEPTH, cfg.staircases),
        ]
    };

    let mut z_cursor = z_start;
    let mut placed = 0u32;
    for (kind, depth, count) in groups {
        for _ in 0..count {
            if placed > 0 {
                z_cursor += COLUMN_INSET;
            }
            let z_max = z_cursor + depth;
            if z_max > bounds.z_max - COLUMN_INSET {
                return CirculationPlan {
                    cells,
                    column_width: column_w,
                };
            }
            cells.push(CirculationCell {
                rect: Rect2 {
                    x_min: column_x_min,
                    x_max: column_x_max,
                    z_min: z_cursor,
                    z_max,
                },
                kind,
            });
            z_cursor = z_max;
            placed += 1;
        }
    }

    CirculationPlan {
        cells,
        column_width: column_w,
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_corridor_stack(
    cells: &mut Vec<CirculationCell>,
    count: u32,
    kind: CirculationKind,
    depth: f32,
    anchor: f32,
    grows_north: bool,
    bounds: Rect2,
    column_x_min: f32,
    column_x_max: f32,
) {
    let mut cursor = anchor;
    for _ in 0..count {
        let (z_min, z_max) = if grows_north {
            (cursor, cursor + depth)
        } else {
            (cursor - depth, cursor)
        };
        if z_min < bounds.z_min + COLUMN_INSET || z_max > bounds.z_max - COLUMN_INSET {
            break;
        }
        cells.push(CirculationCell {
            rect: Rect2 {
                x_min: column_x_min,
                x_max: column_x_max,
                z_min,
                z_max,
            },
            kind,
        });
        cursor = if grows_north { z_max } else { z_min };
    }
}
