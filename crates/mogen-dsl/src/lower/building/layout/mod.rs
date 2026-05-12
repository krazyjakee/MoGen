//! Layout subsystem. Turns a `BuildingCfg` into one `Floorplate` per
//! storey: a list of non-overlapping axis-aligned cells (rooms + shared
//! circulation) that tile a rectangle.
//!
//! Each `style=` value picks its own algorithm for the *room* area; all
//! return the same `Floorplate` shape so the emit pass is style-agnostic.
//! Circulation cells (stairs / elevators) are reserved up front by
//! `circulation::plan` and added to every storey at the same XY so a stair
//! at floor N lands directly above the stair at floor N-1.

mod grid;
mod bsp;
mod corridor;
mod hotel;
mod office;
mod score;

use anyhow::{bail, Result};

use super::circulation::{CirculationKind, CirculationPlan};
use super::config::{BuildingCfg, RoomType, Style};
use super::rng::{attempt_seed, weighted_pick};

/// 2D axis-aligned rectangle in floor-local space. `x`/`z` are the floor
/// plane (matches the building's local frame after wrapper translation).
#[derive(Clone, Copy, Debug)]
pub(super) struct Rect2 {
    pub x_min: f32,
    pub x_max: f32,
    pub z_min: f32,
    pub z_max: f32,
}

impl Rect2 {
    pub fn width(&self) -> f32 {
        self.x_max - self.x_min
    }
    pub fn depth(&self) -> f32 {
        self.z_max - self.z_min
    }
    pub fn area(&self) -> f32 {
        self.width() * self.depth()
    }
    pub fn centre(&self) -> [f32; 2] {
        [
            0.5 * (self.x_min + self.x_max),
            0.5 * (self.z_min + self.z_max),
        ]
    }

    /// Length of the shared edge with `other`, or `0.0` if they don't touch
    /// along a full edge (touching at a corner counts as 0).
    pub fn shared_edge_length(&self, other: &Rect2) -> f32 {
        let x_share = (self.x_max - other.x_min).abs() < EDGE_EPS
            || (other.x_max - self.x_min).abs() < EDGE_EPS;
        let z_share = (self.z_max - other.z_min).abs() < EDGE_EPS
            || (other.z_max - self.z_min).abs() < EDGE_EPS;
        if x_share && !z_share {
            let z_lo = self.z_min.max(other.z_min);
            let z_hi = self.z_max.min(other.z_max);
            (z_hi - z_lo).max(0.0)
        } else if z_share && !x_share {
            let x_lo = self.x_min.max(other.x_min);
            let x_hi = self.x_max.min(other.x_max);
            (x_hi - x_lo).max(0.0)
        } else {
            0.0
        }
    }
}

const EDGE_EPS: f32 = 1e-3;

/// Kind discriminator for a `RoomCell`. Most cells are normal rooms; a
/// minority are circulation cells (staircase or elevator) that share XY
/// across every storey.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CellKind {
    Room,
    Staircase,
    Elevator,
}

/// A single cell on a floorplate. `room_type_index` indexes into
/// `BuildingCfg.room_types` only when `kind == Room`; for circulation it's
/// left at `usize::MAX` and downstream code branches on `kind`.
#[derive(Clone, Debug)]
pub(super) struct RoomCell {
    pub rect: Rect2,
    pub room_type_index: usize,
    pub kind: CellKind,
}

#[derive(Clone, Debug)]
pub(super) struct Floorplate {
    /// Outer rectangle (interior face — does not include exterior wall thickness).
    pub bounds: Rect2,
    pub rooms: Vec<RoomCell>,
}

/// One floorplate per storey, plus the shared circulation plan.
#[derive(Clone, Debug)]
#[allow(dead_code)] // `bounds` is the building footprint reserved for the
                    // T3 roof emitter (gabled/hipped need the footprint
                    // independent of any one storey's room cells).
pub(super) struct BuildingLayout {
    pub bounds: Rect2,
    pub storeys: Vec<StoreyPlate>,
    pub circulation: CirculationPlan,
}

#[derive(Clone, Debug)]
pub(super) struct StoreyPlate {
    /// Signed storey index. `0` = ground, `1..N` = upper floors, `-1..-M`
    /// = basements.
    pub storey: i32,
    pub plate: Floorplate,
}

const ATTEMPTS: u32 = 10;

/// Top-level entry point.
pub(super) fn solve(cfg: &BuildingCfg) -> Result<BuildingLayout> {
    let (w, d) = floor_dims(cfg.floor_area);
    let bounds = Rect2 {
        x_min: -0.5 * w,
        x_max: 0.5 * w,
        z_min: -0.5 * d,
        z_max: 0.5 * d,
    };
    let circ = super::circulation::plan(cfg, bounds);
    // The room layout operates on the floorplate minus the circulation
    // column. If circulation is present we leave a thin gap of wall
    // thickness between the room area and the circulation column.
    let layout_bounds = if circ.has_any() {
        Rect2 {
            x_min: bounds.x_min,
            x_max: bounds.x_max - circ.column_width - cfg.wall_thickness,
            z_min: bounds.z_min,
            z_max: bounds.z_max,
        }
    } else {
        bounds
    };

    let storey_ids = storey_indices(cfg);
    let rooms_per_storey = distribute_rooms(cfg.rooms as usize, storey_ids.len());

    let mut storeys = Vec::new();
    for (i, s) in storey_ids.iter().enumerate() {
        let plate = solve_storey(
            cfg,
            *s,
            layout_bounds,
            bounds,
            &circ,
            rooms_per_storey[i],
        )?;
        storeys.push(StoreyPlate {
            storey: *s,
            plate,
        });
    }

    Ok(BuildingLayout {
        bounds,
        storeys,
        circulation: circ,
    })
}

fn solve_storey(
    cfg: &BuildingCfg,
    storey: i32,
    layout_bounds: Rect2,
    full_bounds: Rect2,
    circ: &CirculationPlan,
    room_count: usize,
) -> Result<Floorplate> {
    let mut best: Option<(f32, Vec<RoomCell>)> = None;
    let storey_mix = (storey as i64).wrapping_mul(1_000_003) as u32;
    for attempt in 0..ATTEMPTS {
        let mut state = attempt_seed(cfg.seed.wrapping_add(storey_mix), attempt);
        let assigned_types = assign_room_types(cfg, room_count, &mut state);
        let room_cells = match cfg.style {
            Style::Grid => grid::layout(layout_bounds, &assigned_types, &mut state),
            Style::ApartmentBlock => match cfg.corridor_type_index() {
                Some(idx) => {
                    corridor::layout(layout_bounds, &assigned_types, idx, &mut state)
                }
                None => bsp::layout(layout_bounds, &assigned_types, &mut state),
            },
            Style::HotelCorridor => {
                let idx = cfg
                    .corridor_type_index()
                    .expect("hotel-corridor synthesises a corridor type in read_cfg");
                hotel::layout(layout_bounds, &assigned_types, idx, &mut state)
            }
            Style::OfficeCore => {
                let idx = cfg
                    .corridor_type_index()
                    .expect("office-core synthesises a corridor type in read_cfg");
                office::layout(layout_bounds, &assigned_types, idx, &mut state)
            }
        };
        let scratch_plate = Floorplate {
            bounds: full_bounds,
            rooms: with_circulation(room_cells.clone(), circ),
        };
        let s = score::score(cfg, &scratch_plate);
        match &best {
            None => best = Some((s, room_cells)),
            Some((bs, _)) if s > *bs => best = Some((s, room_cells)),
            _ => {}
        }
    }
    let Some((_, rooms)) = best else {
        bail!("building layout solver failed to produce any candidate");
    };
    if rooms.is_empty() && room_count > 0 {
        bail!(
            "building layout produced 0 rooms on storey {storey} \
             — try increasing `floor_area` or lowering `rooms`"
        );
    }
    Ok(Floorplate {
        bounds: full_bounds,
        rooms: with_circulation(rooms, circ),
    })
}

fn with_circulation(mut rooms: Vec<RoomCell>, circ: &CirculationPlan) -> Vec<RoomCell> {
    for cell in &circ.cells {
        rooms.push(RoomCell {
            rect: cell.rect,
            room_type_index: usize::MAX,
            kind: match cell.kind {
                CirculationKind::Staircase => CellKind::Staircase,
                CirculationKind::Elevator => CellKind::Elevator,
            },
        });
    }
    rooms
}

fn storey_indices(cfg: &BuildingCfg) -> Vec<i32> {
    let mut out = Vec::new();
    let below = cfg.floors_below as i32;
    let above = cfg.floors_above.max(1) as i32;
    for s in -below..above {
        out.push(s);
    }
    out
}

/// Distribute `total` rooms across `n` storeys, biased toward earlier
/// storeys when there's a remainder. Always returns exactly `n` entries.
fn distribute_rooms(total: usize, n: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    let base = total / n;
    let extra = total % n;
    (0..n)
        .map(|i| if i < extra { base + 1 } else { base })
        .collect()
}

/// Sample `count` room-type indices from `cfg.room_types`, weighted by
/// density. Same logic as in T1 — moved into this module untouched.
fn assign_room_types(cfg: &BuildingCfg, count: usize, state: &mut u32) -> Vec<usize> {
    let weights = cfg.density_weights();
    let active_indices: Vec<usize> = weights
        .iter()
        .enumerate()
        .filter(|(_, w)| **w > 0.0)
        .map(|(i, _)| i)
        .collect();
    if active_indices.is_empty() {
        return Vec::new();
    }
    let mut result: Vec<usize> = Vec::with_capacity(count);
    for &i in active_indices.iter().take(count) {
        result.push(i);
    }
    while result.len() < count {
        let pick = weighted_pick(state, &weights);
        result.push(pick);
    }
    for i in (1..result.len()).rev() {
        let j = (super::rng::step(state) as usize) % (i + 1);
        result.swap(i, j);
    }
    result
}

pub(super) fn floor_dims(area: f32) -> (f32, f32) {
    let target_aspect = std::f32::consts::SQRT_2;
    let depth = (area / target_aspect).sqrt();
    let width = area / depth;
    (width.max(2.0), depth.max(2.0))
}

pub(super) fn cell_type<'a>(cfg: &'a BuildingCfg, cell: &RoomCell) -> Option<&'a RoomType> {
    match cell.kind {
        CellKind::Room => Some(&cfg.room_types[cell.room_type_index]),
        _ => None,
    }
}

pub(super) fn cell_kind_label(cell: &RoomCell) -> &'static str {
    match cell.kind {
        CellKind::Room => "room",
        CellKind::Staircase => "staircase",
        CellKind::Elevator => "elevator",
    }
}

/// Role tag for the per-storey cell group. Distinct from the shaft's
/// role (`staircase` / `elevator` set by `emit/circulation.rs`) so a tag
/// search for "the building's elevator" doesn't conflict with "each
/// floor's elevator landing".
pub(super) fn cell_kind_role(cell: &RoomCell) -> &'static str {
    match cell.kind {
        CellKind::Room => "room",
        CellKind::Staircase => "staircase_landing",
        CellKind::Elevator => "elevator_landing",
    }
}
