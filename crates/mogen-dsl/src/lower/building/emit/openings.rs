//! Compute the list of openings (entrances, interior doors, windows,
//! skylights) for a single floor. This is the bridge between layout and
//! emission: layout decides where rooms go, this module decides where
//! openings cut through the resulting walls.
//!
//! Multi-storey rules:
//! - Entrances live only on storey 0 (the ground floor).
//! - Windows are distributed across above-ground storeys; basements
//!   receive none.
//! - Interior doors fall on a spanning tree across all cells (rooms +
//!   circulation), so every staircase/elevator is reachable from a room.

use super::super::circulation::CirculationPlan;
use super::super::config::BuildingCfg;
use super::super::layout::{CellKind, Floorplate, Rect2, RoomCell};
use super::super::rng::{attempt_seed, rand_f01, rand_range};
use super::StoreyCtx;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpeningKind {
    Entrance,
    InteriorDoor,
    Window(WindowClass),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowClass {
    Small,
    Medium,
    Large,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WallSide {
    North,
    East,
    South,
    West,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Opening {
    pub kind: OpeningKind,
    pub x: f32,
    pub z: f32,
    pub sill: f32,
    pub width: f32,
    pub height: f32,
    pub side: Option<WallSide>,
    pub facing: [f32; 3],
}

#[derive(Clone, Debug, Default)]
pub(super) struct OpeningPlan {
    pub entrances: Vec<Opening>,
    pub interior_doors: Vec<Opening>,
    pub windows: Vec<Opening>,
    pub skylights: Vec<Opening>,
}

pub(super) fn plan_openings(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    ctx: StoreyCtx,
) -> OpeningPlan {
    let mut plan = OpeningPlan::default();
    let mut state = attempt_seed(cfg.seed, 99u32.wrapping_add(ctx.storey as u32));

    if ctx.has_entrances() {
        place_entrances(cfg, plate, &mut plan, &mut state);
    }

    place_interior_doors(cfg, plate, &mut plan, &mut state);

    let storey_windows = windows_for_storey(cfg, ctx);
    if storey_windows > 0 {
        place_windows(cfg, plate, storey_windows, &mut plan, &mut state);
    }
    plan
}

fn place_entrances(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    plan: &mut OpeningPlan,
    state: &mut u32,
) {
    let bounds = &plate.bounds;
    let span = bounds.x_max - bounds.x_min;
    let count = cfg.entrances.max(1) as usize;
    let usable = (span - 2.0 * cfg.door_w).max(0.1);
    for i in 0..count {
        let t = (i as f32 + 1.0) / (count as f32 + 1.0);
        let jitter = (rand_f01(state) - 0.5) * 0.4 / (count as f32 + 1.0);
        let cx = bounds.x_min + cfg.door_w + (t + jitter).clamp(0.05, 0.95) * usable;
        plan.entrances.push(Opening {
            kind: OpeningKind::Entrance,
            x: cx,
            z: bounds.z_min,
            sill: 0.0,
            width: cfg.door_w,
            height: cfg.door_h,
            side: Some(WallSide::South),
            facing: [0.0, 0.0, -1.0],
        });
    }
}

fn place_interior_doors(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    plan: &mut OpeningPlan,
    state: &mut u32,
) {
    let n = plate.rooms.len();
    if n < 2 {
        return;
    }
    let mut edges: Vec<(usize, usize, f32, [f32; 2])> = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            let edge = plate.rooms[i].rect.shared_edge_length(&plate.rooms[j].rect);
            if edge >= cfg.door_w * 1.1 {
                let mid = shared_edge_midpoint(&plate.rooms[i].rect, &plate.rooms[j].rect);
                edges.push((i, j, edge, mid));
            }
        }
    }
    let root = room_nearest_entrance(plate);
    let mut visited = vec![false; n];
    let mut queue = std::collections::VecDeque::new();
    visited[root] = true;
    queue.push_back(root);
    while let Some(u) = queue.pop_front() {
        let mut neighbours: Vec<usize> = edges
            .iter()
            .filter_map(|(a, b, _, _)| {
                if *a == u && !visited[*b] {
                    Some(*b)
                } else if *b == u && !visited[*a] {
                    Some(*a)
                } else {
                    None
                }
            })
            .collect();
        for i in (1..neighbours.len()).rev() {
            let j = (rand_range(state, (i + 1) as u32)) as usize;
            neighbours.swap(i, j);
        }
        for v in neighbours {
            if visited[v] {
                continue;
            }
            visited[v] = true;
            if let Some((_, _, _, mid)) = edges
                .iter()
                .find(|(a, b, _, _)| (*a == u && *b == v) || (*a == v && *b == u))
            {
                let facing = interior_facing(&plate.rooms[u].rect, &plate.rooms[v].rect);
                plan.interior_doors.push(Opening {
                    kind: OpeningKind::InteriorDoor,
                    x: mid[0],
                    z: mid[1],
                    sill: 0.0,
                    width: cfg.door_w,
                    height: cfg.door_h,
                    side: None,
                    facing,
                });
            }
            queue.push_back(v);
        }
    }
}

fn place_windows(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    count: usize,
    plan: &mut OpeningPlan,
    state: &mut u32,
) {
    #[derive(Clone, Copy)]
    struct ExtSeg {
        side: WallSide,
        lo: f32,
        hi: f32,
        fixed: f32,
        facing: [f32; 3],
    }
    let mut segments: Vec<ExtSeg> = Vec::new();
    for cell in &plate.rooms {
        if !matches!(cell.kind, CellKind::Room) {
            // Circulation cells get no exterior windows in T2 — stairwells
            // are typically lit by skylights or borrowed light.
            continue;
        }
        for side in [WallSide::North, WallSide::East, WallSide::South, WallSide::West] {
            if !room_touches_exterior(&cell.rect, &plate.bounds, side) {
                continue;
            }
            let (lo, hi, fixed, facing) = exterior_segment(cell, &plate.bounds, side);
            if side == WallSide::South {
                let mut covered = false;
                for e in &plan.entrances {
                    if e.x - cfg.door_w >= lo && e.x + cfg.door_w <= hi {
                        covered = true;
                        break;
                    }
                }
                if covered {
                    continue;
                }
            }
            if hi - lo < cfg.window_w * 1.5 {
                continue;
            }
            segments.push(ExtSeg {
                side,
                lo,
                hi,
                fixed,
                facing,
            });
        }
    }
    if segments.is_empty() {
        return;
    }
    for i in 0..count {
        let seg = segments[i % segments.len()];
        let class = match i % 3 {
            0 => WindowClass::Medium,
            1 => WindowClass::Large,
            _ => WindowClass::Small,
        };
        let (cw, ch) = window_size(cfg, class);
        let usable = (seg.hi - seg.lo - cw).max(0.1);
        let t = 0.2 + 0.6 * rand_f01(state);
        let along = seg.lo + 0.5 * cw + t * usable;
        let (x, z) = match seg.side {
            WallSide::North | WallSide::South => (along, seg.fixed),
            WallSide::East | WallSide::West => (seg.fixed, along),
        };
        let sill = (cfg.ceiling_height - ch).max(0.4) * 0.5;
        plan.windows.push(Opening {
            kind: OpeningKind::Window(class),
            x,
            z,
            sill,
            width: cw,
            height: ch,
            side: Some(seg.side),
            facing: seg.facing,
        });
    }
}

fn window_size(cfg: &BuildingCfg, class: WindowClass) -> (f32, f32) {
    match class {
        WindowClass::Small => (cfg.window_w * 0.6, cfg.window_h * 0.6),
        WindowClass::Medium => (cfg.window_w, cfg.window_h),
        WindowClass::Large => (cfg.window_w * 1.4, cfg.window_h * 1.2),
    }
}

fn room_touches_exterior(r: &Rect2, b: &Rect2, side: WallSide) -> bool {
    match side {
        WallSide::North => (r.z_max - b.z_max).abs() < 1e-3,
        WallSide::South => (r.z_min - b.z_min).abs() < 1e-3,
        WallSide::East => (r.x_max - b.x_max).abs() < 1e-3,
        WallSide::West => (r.x_min - b.x_min).abs() < 1e-3,
    }
}

fn exterior_segment(
    cell: &RoomCell,
    bounds: &Rect2,
    side: WallSide,
) -> (f32, f32, f32, [f32; 3]) {
    match side {
        WallSide::North => (cell.rect.x_min, cell.rect.x_max, bounds.z_max, [0.0, 0.0, 1.0]),
        WallSide::South => (cell.rect.x_min, cell.rect.x_max, bounds.z_min, [0.0, 0.0, -1.0]),
        WallSide::East => (cell.rect.z_min, cell.rect.z_max, bounds.x_max, [1.0, 0.0, 0.0]),
        WallSide::West => (cell.rect.z_min, cell.rect.z_max, bounds.x_min, [-1.0, 0.0, 0.0]),
    }
}

fn shared_edge_midpoint(a: &Rect2, b: &Rect2) -> [f32; 2] {
    if (a.x_max - b.x_min).abs() < 1e-3 {
        return [a.x_max, 0.5 * (a.z_min.max(b.z_min) + a.z_max.min(b.z_max))];
    }
    if (b.x_max - a.x_min).abs() < 1e-3 {
        return [b.x_max, 0.5 * (a.z_min.max(b.z_min) + a.z_max.min(b.z_max))];
    }
    if (a.z_max - b.z_min).abs() < 1e-3 {
        return [0.5 * (a.x_min.max(b.x_min) + a.x_max.min(b.x_max)), a.z_max];
    }
    if (b.z_max - a.z_min).abs() < 1e-3 {
        return [0.5 * (a.x_min.max(b.x_min) + a.x_max.min(b.x_max)), b.z_max];
    }
    [
        0.5 * (a.centre()[0] + b.centre()[0]),
        0.5 * (a.centre()[1] + b.centre()[1]),
    ]
}

fn interior_facing(a: &Rect2, b: &Rect2) -> [f32; 3] {
    if (a.x_max - b.x_min).abs() < 1e-3 {
        [1.0, 0.0, 0.0]
    } else if (b.x_max - a.x_min).abs() < 1e-3 {
        [-1.0, 0.0, 0.0]
    } else if (a.z_max - b.z_min).abs() < 1e-3 {
        [0.0, 0.0, 1.0]
    } else if (b.z_max - a.z_min).abs() < 1e-3 {
        [0.0, 0.0, -1.0]
    } else {
        [1.0, 0.0, 0.0]
    }
}

fn room_nearest_entrance(plate: &Floorplate) -> usize {
    let target = [
        0.5 * (plate.bounds.x_min + plate.bounds.x_max),
        plate.bounds.z_min,
    ];
    let mut best = (f32::INFINITY, 0usize);
    for (i, cell) in plate.rooms.iter().enumerate() {
        let c = cell.rect.centre();
        let dx = c[0] - target[0];
        let dz = c[1] - target[1];
        let d2 = dx * dx + dz * dz;
        if d2 < best.0 {
            best = (d2, i);
        }
    }
    best.1
}

/// Per-storey window budget. Above-ground storeys split the total evenly
/// with remainder biased to the lower floors; basements get zero (they're
/// underground).
fn windows_for_storey(cfg: &BuildingCfg, ctx: StoreyCtx) -> usize {
    if ctx.storey < 0 {
        return 0;
    }
    let above_floors = cfg.floors_above.max(1);
    let total = cfg.windows as usize;
    let base = total / above_floors as usize;
    let extra = total % above_floors as usize;
    let storey_idx = ctx.storey as usize;
    if storey_idx < extra {
        base + 1
    } else {
        base
    }
}

/// Unused import suppressor: keep CirculationPlan reachable for future
/// adjacency rules that involve circulation cells. T2 doesn't read it
/// directly here (the circulation cells are already in `plate.rooms` with
/// `CellKind::Staircase` / `CellKind::Elevator`), but the import stays so
/// T3's scoring extensions can hook in without a churning diff.
#[allow(dead_code)]
fn _circ_reachable(_: &CirculationPlan) {}
