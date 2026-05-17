//! Layout subsystem. Turns a `BuildingCfg` into one `Floorplate` per
//! storey: a list of non-overlapping axis-aligned cells (rooms + shared
//! circulation) that tile a rectangle.
//!
//! Each `style=` value picks its own algorithm for the *room* area; all
//! return the same `Floorplate` shape so the emit pass is style-agnostic.
//! Circulation cells (stairs / elevators) are reserved up front by
//! `circulation::plan` and added to every storey at the same XY so a stair
//! at floor N lands directly above the stair at floor N-1.

mod common;
mod grid;
mod bsp;
mod hotel;
mod office;
mod radial;
mod organic;
mod maze;
mod score;

use anyhow::{bail, Result};

use super::circulation::{
    CirculationKind, CirculationPlan, STAIR_ENTRY_DEPTH,
};
use super::config::{BuildingCfg, RoomType, Style};
use super::rng::{attempt_seed, rand_range, weighted_pick};

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

/// Which of the four axis-aligned faces of a cell or floorplate something
/// sits on. Lives here (not in the `emit` layer) because layout-time data
/// — specifically `DoorSlot` on a `RoomCell` — needs to name a face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WallSide {
    North,
    East,
    South,
    West,
}

/// Deterministic random ordering of the four wall sides, derived from the
/// user-facing `cfg.seed`. Used by both opening placement (so the first
/// entrance can land on any facade, not just south) and the layout scorer
/// (so it predicts entrance anchors on the same faces).
///
/// Driven by a sub-seed independent of any storey/attempt state so the
/// scorer (which runs pre-emit, with no `OpeningPlan` to read) and the
/// emitter agree on the order without threading shared mutable state.
pub(super) fn entrance_side_order(seed: u32) -> [WallSide; 4] {
    let mut order = [
        WallSide::South,
        WallSide::North,
        WallSide::East,
        WallSide::West,
    ];
    let mut state = attempt_seed(seed, 0x0E11_7A11);
    for i in (1..order.len()).rev() {
        let j = rand_range(&mut state, (i as u32) + 1) as usize;
        order.swap(i, j);
    }
    order
}

/// Kind discriminator for a `RoomCell`. Most cells are normal rooms; a
/// minority are circulation cells (staircase or elevator) that share XY
/// across every storey.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CellKind {
    Room,
    Staircase,
    Elevator,
}

/// A strip along one face of a cell where a door is allowed to land.
///
/// Used by `place_interior_doors` to constrain door placement on cells
/// that have internal structure the shared-edge math doesn't know about
/// — most notably staircases, whose only valid storey-floor-height entry
/// is the south entry zone (the back half of the cell is a half-storey
/// landing or a flight cutout, so a door there opens into mid-air).
///
/// `along_min..along_max` is in world-space layout coordinates: x for
/// North/South faces, z for East/West faces.
#[derive(Clone, Copy, Debug)]
pub(super) struct DoorSlot {
    pub side: WallSide,
    pub along_min: f32,
    pub along_max: f32,
}

/// A single cell on a floorplate. `room_type_index` indexes into
/// `BuildingCfg.room_types` only when `kind == Room`; for circulation it's
/// left at `usize::MAX` and downstream code branches on `kind`.
///
/// `door_slots` restricts where interior doors may be carved on this
/// cell's perimeter. An empty vec means "anywhere along a shared edge"
/// (the default for plain rooms and for elevators, which run their own
/// shaft-door pipeline). A non-empty vec means a candidate edge must
/// overlap at least one slot by a door's width before the door planner
/// will consider it.
#[derive(Clone, Debug)]
pub(super) struct RoomCell {
    pub rect: Rect2,
    pub room_type_index: usize,
    pub kind: CellKind,
    pub door_slots: Vec<DoorSlot>,
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
    let bounds_above = bounds_for_area(cfg.floor_area);
    // Cellar uses a smaller footprint when `cellar_area` is set. We clamp
    // the cellar to never exceed `floor_area` — a larger cellar would stick
    // out under the above-ground walls (the validator warns about this with
    // W1116; here we silently clamp so lowering still succeeds). The
    // basement is east-aligned with the above-ground plate so they share
    // the east wall — that's where the circulation column lives, and
    // sharing the wall is what keeps stairs aligned vertically across
    // every storey when the cellar is smaller than the ground floor.
    let cellar_effective = cfg
        .cellar_area
        .map(|c| c.min(cfg.floor_area))
        .unwrap_or(cfg.floor_area);
    let bounds_below = if cfg.cellar_area.is_some() {
        east_aligned_bounds(cellar_effective, bounds_above)
    } else {
        bounds_above
    };

    // Plan circulation against the smaller of the two plates so the column
    // is guaranteed to fit in every storey. Once the cellar is east-aligned
    // with the above-ground plate, the column lands on the east wall of
    // both (the shared edge) — so above-ground floors still see the
    // column on their east boundary, basement floors do too.
    let circ_bounds = if cfg.cellar_area.is_some() {
        bounds_below
    } else {
        bounds_above
    };
    // For corridor-bearing styles, place circulation cells flush with the
    // corridor's south/north walls so the door planner can stamp a door
    // straight from the corridor into each shaft. Otherwise circulation
    // stacks in the east-edge column on its own.
    let corridor_z = corridor_z_range(cfg.style, bounds_above);
    let circ = super::circulation::plan(cfg, circ_bounds, corridor_z);

    // Hard-fail if the column literally doesn't fit in the basement, even
    // after east-alignment (e.g. cellar is too shallow on Z). The
    // validator W1115 warns ahead of time; this catches the residual.
    if cfg.cellar_area.is_some() && circ.has_any() {
        for cell in &circ.cells {
            let fits_x =
                cell.rect.x_min >= bounds_below.x_min - 1e-3 && cell.rect.x_max <= bounds_below.x_max + 1e-3;
            let fits_z =
                cell.rect.z_min >= bounds_below.z_min - 1e-3 && cell.rect.z_max <= bounds_below.z_max + 1e-3;
            if !fits_x || !fits_z {
                bail!(
                    "cellar_area={} m² is too small to fit the vertical circulation column \
                     — grow `cellar_area` or drop `staircases`/`elevators`",
                    cfg.cellar_area.unwrap_or(0.0)
                );
            }
        }
    }

    let layout_bounds_above = layout_bounds_for(bounds_above, &circ, cfg.wall_thickness);
    let layout_bounds_below = layout_bounds_for(bounds_below, &circ, cfg.wall_thickness);

    let storey_ids = storey_indices(cfg);
    let rooms_per_storey = distribute_rooms(cfg.rooms as usize, storey_ids.len());

    let mut storeys = Vec::new();
    for (i, s) in storey_ids.iter().enumerate() {
        let (bounds, layout_bounds) = if *s < 0 {
            (bounds_below, layout_bounds_below)
        } else {
            (bounds_above, layout_bounds_above)
        };
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
        bounds: bounds_above,
        storeys,
        circulation: circ,
    })
}

fn bounds_for_area(area: f32) -> Rect2 {
    let (w, d) = floor_dims(area);
    Rect2 {
        x_min: -0.5 * w,
        x_max: 0.5 * w,
        z_min: -0.5 * d,
        z_max: 0.5 * d,
    }
}

/// Build a smaller rectangle of the given `area` whose **east edge** is
/// aligned with `reference`'s east edge. The Z axis is centered on the
/// reference Z midpoint (basements tend to sit under the centre of the
/// building, not its south wall). Used so a basement plate of
/// `cellar_area` shares a wall with the above-ground plate — that wall is
/// where the circulation column lives, and sharing it keeps stairs
/// aligned vertically across every storey without forcing the basement
/// to match the full ground footprint.
fn east_aligned_bounds(area: f32, reference: Rect2) -> Rect2 {
    let (w, d) = floor_dims(area);
    let z_mid = 0.5 * (reference.z_min + reference.z_max);
    Rect2 {
        x_min: reference.x_max - w,
        x_max: reference.x_max,
        z_min: z_mid - 0.5 * d,
        z_max: z_mid + 0.5 * d,
    }
}

fn layout_bounds_for(bounds: Rect2, circ: &CirculationPlan, wall_thickness: f32) -> Rect2 {
    // The room layout operates on the floorplate minus the circulation
    // column. Side rooms abut the column directly (no wall-thickness
    // gap) — the shared edge is the column's west wall, which the room
    // emitter will then wrap with an interior wall.
    let _ = wall_thickness;
    if circ.has_any() {
        Rect2 {
            x_min: bounds.x_min,
            x_max: bounds.x_max - circ.column_width,
            z_min: bounds.z_min,
            z_max: bounds.z_max,
        }
    } else {
        bounds
    }
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
            Style::ApartmentBlock => bsp::layout(layout_bounds, &assigned_types, &mut state),
            Style::HotelCorridor => {
                let idx = cfg
                    .corridor_type_index()
                    .expect("hotel-corridor synthesises a corridor type in read_cfg");
                let mut cells = hotel::layout(layout_bounds, &assigned_types, idx, &mut state);
                extend_corridor_through_column(&mut cells, idx, full_bounds, circ);
                cells
            }
            Style::OfficeCore => {
                let idx = cfg
                    .corridor_type_index()
                    .expect("office-core synthesises a corridor type in read_cfg");
                let mut cells = office::layout(layout_bounds, &assigned_types, idx, &mut state);
                extend_corridor_through_column(&mut cells, idx, full_bounds, circ);
                cells
            }
            Style::Radial => radial::layout(layout_bounds, &assigned_types, &mut state),
            Style::Organic => organic::layout(layout_bounds, &assigned_types, &mut state),
            Style::Maze => maze::layout(layout_bounds, &assigned_types, &mut state),
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
        let kind = match cell.kind {
            CirculationKind::Staircase => CellKind::Staircase,
            CirculationKind::Elevator => CellKind::Elevator,
        };
        let door_slots = match kind {
            CellKind::Staircase => staircase_door_slots(&cell.rect),
            // Elevators have their own shaft-door pipeline that places a
            // centred opening on the open face; leaving slots empty here
            // means `place_interior_doors` skips the elevator entirely
            // (it already explicitly bails on elevator edges).
            CellKind::Elevator | CellKind::Room => Vec::new(),
        };
        rooms.push(RoomCell {
            rect: cell.rect,
            room_type_index: usize::MAX,
            kind,
            door_slots,
        });
    }
    rooms
}

/// Door slots for a switchback staircase: the south entry zone is the
/// only strip at storey-floor height that touches a walkable platform.
/// We expose it on three faces (east, west, south) so whichever neighbour
/// the BFS picks can attach there. The north face has no slot — its
/// neighbour would walk through the door onto the half-storey mid-landing,
/// which sits 1.5 m above the storey floor.
fn staircase_door_slots(r: &Rect2) -> Vec<DoorSlot> {
    let entry_z_max = r.z_min + STAIR_ENTRY_DEPTH;
    vec![
        DoorSlot { side: WallSide::East,  along_min: r.z_min, along_max: entry_z_max },
        DoorSlot { side: WallSide::West,  along_min: r.z_min, along_max: entry_z_max },
        DoorSlot { side: WallSide::South, along_min: r.x_min, along_max: r.x_max },
    ]
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

/// Extend the corridor cell east through the circulation column so it shares
/// edges with the stair / elevator cells the planner flushed against the
/// corridor's south/north walls. Without this the corridor would terminate
/// at `layout_bounds.x_max` with a 0.12 m gap before the column, leaving a
/// floating wall segment on the corridor's east end and an elevator door
/// that doesn't open onto the corridor.
///
/// No-op when:
/// - the layout returned no corridor cell (e.g. hotel fell back to grid),
/// - circulation is empty (no column to bridge to),
/// - the corridor runs along Z (column is on the east edge, not aligned
///   with the corridor's natural extension axis).
fn extend_corridor_through_column(
    cells: &mut [RoomCell],
    corridor_type_idx: usize,
    full_bounds: Rect2,
    circ: &CirculationPlan,
) {
    if !circ.has_any() {
        return;
    }
    let Some(corridor) = cells.iter_mut().find(|c| {
        matches!(c.kind, CellKind::Room) && c.room_type_index == corridor_type_idx
    }) else {
        return;
    };
    // Only extend east when the corridor is X-aligned (the only case where
    // `corridor_z_range()` returns Some and the planner placed circulation
    // cells flush with the corridor's south/north walls). For a Z-aligned
    // corridor the column would be on the wrong side and there's nothing
    // to bridge.
    if corridor.rect.width() < corridor.rect.depth() {
        return;
    }
    corridor.rect.x_max = corridor.rect.x_max.max(full_bounds.x_max);
}

/// For hotel-corridor / office-core, return the corridor cell's Z extent
/// so the circulation planner can flush its cells against the corridor.
/// Mirrors `hotel.rs`'s mid_z + CORRIDOR_WIDTH/2 maths so the layout and
/// the circulation cells line up exactly.
pub(super) fn corridor_z_range(style: Style, bounds: Rect2) -> Option<(f32, f32)> {
    const CORRIDOR_WIDTH: f32 = 1.8;
    if !matches!(style, Style::HotelCorridor | Style::OfficeCore) {
        return None;
    }
    // hotel.rs's `along_x` test: corridor runs along whichever axis is
    // longer. We only support the X-axis variant for adjacent placement;
    // for Z-axis corridors the column is on the east edge but the
    // corridor's east end is at bounds.x_max — wrong axis — so fall back
    // to stack-mode.
    if bounds.width() < bounds.depth() {
        return None;
    }
    let mid_z = 0.5 * (bounds.z_min + bounds.z_max);
    let half = 0.5 * CORRIDOR_WIDTH;
    Some((mid_z - half, mid_z + half))
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
