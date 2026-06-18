//! Layout solver — places rooms, joins them with corridors, and threads
//! staircases between levels. Produces a [`DungeonLayout`] of per-level
//! occupancy grids plus the room and stair records the emit/POI passes consume.
//!
//! Everything is a deterministic function of `cfg.seed`: each phase draws from
//! its own `sub_seed` stream so adding a phase never perturbs an earlier one.
//!
//! The grid is the single source of truth for "is this cell walkable floor".
//! Rooms are axis-aligned rectangles stamped into it; corridors are L-shaped
//! runs carved between room centres along a spanning tree (plus `loops` extra
//! edges). Stairs reserve a straight run of cells that is floor on the lower
//! level at its foot and floor on the upper level at its head, then punch a deck
//! opening through the upper level so the step geometry can rise between floors.

use super::config::DungeonCfg;
use super::rng::{rand_range, sub_seed};

/// One rectangular room on a level, in integer cell coordinates.
#[derive(Clone, Copy, Debug)]
pub(super) struct Room {
    pub level: usize,
    pub x0: i32,
    pub z0: i32,
    pub w: i32,
    pub d: i32,
}

impl Room {
    /// Centre cell (rounded down) of the room.
    pub fn center_cell(&self) -> (i32, i32) {
        (self.x0 + self.w / 2, self.z0 + self.d / 2)
    }

    /// True if this room (expanded by `pad` cells on every side) overlaps `other`.
    fn overlaps_padded(&self, other: &Room, pad: i32) -> bool {
        self.x0 - pad < other.x0 + other.w
            && self.x0 + self.w + pad > other.x0
            && self.z0 - pad < other.z0 + other.d
            && self.z0 + self.d + pad > other.z0
    }
}

/// A straight staircase connecting `lower_level` (its foot) to `lower_level + 1`
/// (its head). `cells` runs foot → head; the head sits on the upper floor and
/// the foot on the lower floor.
#[derive(Clone, Debug)]
pub(super) struct Stair {
    pub lower_level: usize,
    pub cells: Vec<(i32, i32)>,
}

/// An exterior doorway carved through the perimeter wall of a room. A one-cell
/// floor stub runs from the room out to the grid border; `(i, j)` is that border
/// threshold cell and `(di, dj)` the outward direction whose perimeter wall the
/// emit pass drops to open the door. `level` is the floor it sits on (`0` =
/// ground).
#[derive(Clone, Copy, Debug)]
pub(super) struct Entrance {
    pub level: usize,
    pub i: i32,
    pub j: i32,
    pub di: i32,
    pub dj: i32,
    /// Index into `rooms` of the room the entrance leads into.
    pub room: usize,
}

/// Per-level occupancy. `floor[j*gw + i]` is true where a cell is walkable.
/// `opening[j*gw + i]` marks deck cells punched out for a staircase that rises
/// *into* this level (so its floor deck has a stairwell void).
#[derive(Clone, Debug)]
pub(super) struct LevelGrid {
    pub floor: Vec<bool>,
    pub opening: Vec<bool>,
    /// Stair-shaft cells that read as walkable for wall enclosure even though
    /// they are not room/corridor floor (the step geometry is the surface).
    pub stair_cells: Vec<bool>,
}

pub(super) struct DungeonLayout {
    pub gw: i32,
    pub gd: i32,
    pub levels: usize,
    pub grids: Vec<LevelGrid>,
    pub rooms: Vec<Room>,
    pub stairs: Vec<Stair>,
    /// Corridor connections per room index (into `rooms`); degree 1 == dead end.
    pub room_degree: Vec<u32>,
    /// Exterior doorways, as many per floor as `cfg.entrances_per_floor` asks
    /// for (and as many as fit). Empty when none could be placed.
    pub entrances: Vec<Entrance>,
}

impl DungeonLayout {
    pub fn idx(&self, i: i32, j: i32) -> usize {
        (j * self.gw + i) as usize
    }

    pub fn is_floor(&self, level: usize, i: i32, j: i32) -> bool {
        if i < 0 || j < 0 || i >= self.gw || j >= self.gd {
            return false;
        }
        self.grids[level].floor[self.idx(i, j)]
    }

    pub fn is_walkable(&self, level: usize, i: i32, j: i32) -> bool {
        if i < 0 || j < 0 || i >= self.gw || j >= self.gd {
            return false;
        }
        let k = self.idx(i, j);
        self.grids[level].floor[k] || self.grids[level].stair_cells[k]
    }
}

pub(super) fn generate(cfg: &DungeonCfg) -> DungeonLayout {
    let gw = ((cfg.size[0] / cfg.cell).round() as i32).max(5);
    let gd = ((cfg.size[2] / cfg.cell).round() as i32).max(5);
    let levels = cfg.levels.max(1) as usize;
    let n = (gw * gd) as usize;

    let mut grids: Vec<LevelGrid> = (0..levels)
        .map(|_| LevelGrid {
            floor: vec![false; n],
            opening: vec![false; n],
            stair_cells: vec![false; n],
        })
        .collect();
    let mut rooms: Vec<Room> = Vec::new();
    let mut room_degree: Vec<u32> = Vec::new();

    // --- rooms + corridors, per level -------------------------------------
    for level in 0..levels {
        let level_room_start = rooms.len();
        place_rooms(cfg, level, gw, gd, &mut rooms);
        for r in &rooms[level_room_start..] {
            stamp_room(&mut grids[level], gw, r);
        }
        room_degree.resize(rooms.len(), 0);

        let level_rooms = &rooms[level_room_start..];
        connect_rooms(
            cfg,
            level,
            gw,
            level_rooms,
            level_room_start,
            &mut grids[level],
            &mut room_degree,
        );
    }

    let mut layout = DungeonLayout {
        gw,
        gd,
        levels,
        grids,
        rooms,
        stairs: Vec::new(),
        room_degree,
        entrances: Vec::new(),
    };

    // --- staircases between adjacent levels -------------------------------
    place_stairs(cfg, &mut layout);

    // --- exterior entrances -----------------------------------------------
    layout.entrances = place_entrances(cfg, &mut layout);

    layout
}

/// Carve exterior doorways per floor from `cfg.entrances_per_floor` (index 0 =
/// ground). Each is a one-cell floor stub from a room's nearest side out to the
/// grid border; the emit pass drops the perimeter wall on that edge and the POI
/// pass marks the threshold.
fn place_entrances(cfg: &DungeonCfg, layout: &mut DungeonLayout) -> Vec<Entrance> {
    let mut out = Vec::new();
    for level in 0..layout.levels {
        let want = cfg.entrances_per_floor.get(level).copied().unwrap_or(0) as usize;
        place_level_entrances(layout, level, want, &mut out);
    }
    out
}

/// Place up to `want` doorways on `level`, preferring dead-end rooms (so an
/// entrance is a defensible vestibule) and shorter border stubs, spreading
/// across distinct rooms before reusing one. Carves each stub and appends the
/// resulting `Entrance`s to `out`.
fn place_level_entrances(
    layout: &mut DungeonLayout,
    level: usize,
    want: usize,
    out: &mut Vec<Entrance>,
) {
    if want == 0 {
        return;
    }
    let gw = layout.gw;
    let gd = layout.gd;
    let rooms: Vec<usize> = layout
        .rooms
        .iter()
        .enumerate()
        .filter(|(_, r)| r.level == level)
        .map(|(i, _)| i)
        .collect();
    if rooms.is_empty() {
        return;
    }

    // Every (room, side) candidate with its gap to the border. Dead-end rooms
    // sort first so they are picked before through-rooms; then by shortest gap;
    // then by room/direction for a stable, deterministic order.
    let mut cands: Vec<(bool, i32, usize, i32, i32)> = Vec::new(); // (not_dead_end, gap, room, di, dj)
    for &ri in &rooms {
        let r = layout.rooms[ri];
        let dead_end = layout.room_degree[ri] == 1;
        let sides = [
            (-1, 0, r.x0),                 // west
            (1, 0, gw - 1 - (r.x0 + r.w)), // east
            (0, -1, r.z0),                 // north
            (0, 1, gd - 1 - (r.z0 + r.d)), // south
        ];
        for (di, dj, gap) in sides {
            if gap < 0 {
                continue;
            }
            cands.push((!dead_end, gap, ri, di, dj));
        }
    }
    cands.sort();

    // Greedy pick: first pass takes one doorway per distinct room; a second pass
    // allows a room to host another (distinct side) if we still need more.
    let mut chosen: Vec<(usize, i32, i32)> = Vec::new();
    let mut used_rooms = std::collections::BTreeSet::new();
    for &(_, _, ri, di, dj) in &cands {
        if chosen.len() >= want {
            break;
        }
        if used_rooms.insert(ri) {
            chosen.push((ri, di, dj));
        }
    }
    if chosen.len() < want {
        for &(_, _, ri, di, dj) in &cands {
            if chosen.len() >= want {
                break;
            }
            if !chosen.iter().any(|&(cr, cdi, cdj)| cr == ri && cdi == di && cdj == dj) {
                chosen.push((ri, di, dj));
            }
        }
    }

    for (ri, di, dj) in chosen {
        let (ti, tj) = carve_stub(layout, level, ri, di, dj);
        out.push(Entrance { level, i: ti, j: tj, di, dj, room: ri });
    }
}

/// Carve a one-cell floor stub from room `ri` on `level` straight out to the
/// grid border along the chosen axis, returning the border threshold cell.
fn carve_stub(
    layout: &mut DungeonLayout,
    level: usize,
    ri: usize,
    di: i32,
    dj: i32,
) -> (i32, i32) {
    let gw = layout.gw;
    let gd = layout.gd;
    let r = layout.rooms[ri];
    let grid = &mut layout.grids[level];
    if di != 0 {
        let j = r.z0 + r.d / 2;
        let (lo, hi) = if di < 0 {
            (0, r.x0 - 1)
        } else {
            (r.x0 + r.w, gw - 1)
        };
        for i in lo..=hi {
            grid.floor[(j * gw + i) as usize] = true;
        }
        (if di < 0 { 0 } else { gw - 1 }, j)
    } else {
        let i = r.x0 + r.w / 2;
        let (lo, hi) = if dj < 0 {
            (0, r.z0 - 1)
        } else {
            (r.z0 + r.d, gd - 1)
        };
        for j in lo..=hi {
            grid.floor[(j * gw + i) as usize] = true;
        }
        (i, if dj < 0 { 0 } else { gd - 1 })
    }
}

/// Best-effort random placement of `cfg.rooms` non-overlapping rectangles inside
/// the grid, keeping a one-cell border for outer walls and `spacing` cells
/// between rooms.
fn place_rooms(cfg: &DungeonCfg, level: usize, gw: i32, gd: i32, out: &mut Vec<Room>) {
    let mut state = sub_seed(cfg.seed, 0x0400_0001u32.wrapping_add(level as u32));
    let level_start = out.len();
    let rmin = cfg.room_min.max(1) as i32;
    let rmax = cfg.room_max.max(cfg.room_min).max(1) as i32;
    let pad = cfg.spacing as i32;

    // Cap attempts so a dense grid can't spin forever.
    let attempts = (cfg.rooms * 12).max(24);
    for _ in 0..attempts {
        if (out.len() - level_start) as u32 >= cfg.rooms {
            break;
        }
        let w = rmin + rand_range(&mut state, (rmax - rmin + 1) as u32) as i32;
        let d = rmin + rand_range(&mut state, (rmax - rmin + 1) as u32) as i32;
        if w + 2 >= gw || d + 2 >= gd {
            continue;
        }
        let x0 = 1 + rand_range(&mut state, (gw - w - 2).max(1) as u32) as i32;
        let z0 = 1 + rand_range(&mut state, (gd - d - 2).max(1) as u32) as i32;
        let candidate = Room {
            level,
            x0,
            z0,
            w,
            d,
        };
        let clashes = out[level_start..]
            .iter()
            .any(|r| candidate.overlaps_padded(r, pad));
        if !clashes {
            out.push(candidate);
        }
    }
}

fn stamp_room(grid: &mut LevelGrid, gw: i32, r: &Room) {
    for j in r.z0..r.z0 + r.d {
        for i in r.x0..r.x0 + r.w {
            grid.floor[(j * gw + i) as usize] = true;
        }
    }
}

/// Join the level's rooms with corridors: a nearest-neighbour spanning tree
/// guarantees every room is reachable, then `cfg.loops` extra random edges add
/// cycles. `room_base` offsets into the shared `rooms`/`room_degree` arrays.
fn connect_rooms(
    cfg: &DungeonCfg,
    level: usize,
    gw: i32,
    level_rooms: &[Room],
    room_base: usize,
    grid: &mut LevelGrid,
    room_degree: &mut [u32],
) {
    let count = level_rooms.len();
    if count < 2 {
        return;
    }
    let mut state = sub_seed(cfg.seed, 0x000C_0DEEu32.wrapping_add(level as u32));

    let centers: Vec<(i32, i32)> = level_rooms.iter().map(|r| r.center_cell()).collect();

    // Prim-style spanning tree over room centres (squared cell distance).
    let mut connected = vec![false; count];
    connected[0] = true;
    for _ in 1..count {
        let mut best: Option<(usize, usize, i64)> = None;
        for (a, &ca) in centers.iter().enumerate() {
            if !connected[a] {
                continue;
            }
            for (b, &cb) in centers.iter().enumerate() {
                if connected[b] {
                    continue;
                }
                let dx = (ca.0 - cb.0) as i64;
                let dz = (ca.1 - cb.1) as i64;
                let dist = dx * dx + dz * dz;
                if best.map(|(_, _, bd)| dist < bd).unwrap_or(true) {
                    best = Some((a, b, dist));
                }
            }
        }
        if let Some((a, b, _)) = best {
            connected[b] = true;
            carve_corridor(cfg, gw, centers[a], centers[b], grid, &mut state);
            room_degree[room_base + a] += 1;
            room_degree[room_base + b] += 1;
        }
    }

    // Extra loop edges between random distinct rooms.
    for _ in 0..cfg.loops {
        let a = rand_range(&mut state, count as u32) as usize;
        let b = rand_range(&mut state, count as u32) as usize;
        if a == b {
            continue;
        }
        carve_corridor(cfg, gw, centers[a], centers[b], grid, &mut state);
        room_degree[room_base + a] += 1;
        room_degree[room_base + b] += 1;
    }
}

/// Carve an L-shaped corridor of width `cfg.corridor_width` between two cells.
/// A coin flip orders the horizontal and vertical legs so the elbow varies.
fn carve_corridor(
    cfg: &DungeonCfg,
    gw: i32,
    a: (i32, i32),
    b: (i32, i32),
    grid: &mut LevelGrid,
    state: &mut u32,
) {
    let horizontal_first = rand_range(state, 2) == 0;
    if horizontal_first {
        carve_h(cfg, gw, a.0, b.0, a.1, grid);
        carve_v(cfg, gw, a.1, b.1, b.0, grid);
    } else {
        carve_v(cfg, gw, a.1, b.1, a.0, grid);
        carve_h(cfg, gw, a.0, b.0, b.1, grid);
    }
}

fn carve_h(cfg: &DungeonCfg, gw: i32, x0: i32, x1: i32, z: i32, grid: &mut LevelGrid) {
    let (lo, hi) = (x0.min(x1), x0.max(x1));
    for i in lo..=hi {
        carve_wide(cfg, gw, i, z, true, grid);
    }
}

fn carve_v(cfg: &DungeonCfg, gw: i32, z0: i32, z1: i32, x: i32, grid: &mut LevelGrid) {
    let (lo, hi) = (z0.min(z1), z0.max(z1));
    for j in lo..=hi {
        carve_wide(cfg, gw, x, j, false, grid);
    }
}

/// Mark a corridor cell plus the `corridor_width - 1` cells perpendicular to the
/// run direction as floor, staying inside the wall border.
fn carve_wide(cfg: &DungeonCfg, gw: i32, i: i32, j: i32, horizontal: bool, grid: &mut LevelGrid) {
    let gd = grid.floor.len() as i32 / gw;
    let width = cfg.corridor_width.max(1) as i32;
    for w in 0..width {
        let off = w - width / 2;
        let (ci, cj) = if horizontal { (i, j + off) } else { (i + off, j) };
        if ci >= 1 && ci < gw - 1 && cj >= 1 && cj < gd - 1 {
            grid.floor[(cj * gw + ci) as usize] = true;
        }
    }
}

/// Thread `cfg.stairs` staircases between each pair of adjacent levels. Each
/// stair is a straight run whose foot sits on lower-level floor and whose head
/// sits on upper-level floor; the run punches a deck opening through the upper
/// level and registers its cells as walkable on the upper level.
fn place_stairs(cfg: &DungeonCfg, layout: &mut DungeonLayout) {
    if layout.levels < 2 || cfg.stairs == 0 {
        return;
    }
    // Run length in cells: enough to climb one storey at a gentle pitch, but at
    // least 2 cells so the staircase reads as a flight rather than a single step.
    let run = 3i32;
    let gw = layout.gw;
    let gd = layout.gd;
    // Four cardinal run directions.
    let dirs = [(1, 0), (-1, 0), (0, 1), (0, -1)];

    for lower in 0..layout.levels - 1 {
        let upper = lower + 1;
        let mut state = sub_seed(cfg.seed, 0x57A1_2000u32.wrapping_add(lower as u32));
        let mut placed = 0u32;
        let max_tries = (cfg.stairs * 40).max(60);
        for _ in 0..max_tries {
            if placed >= cfg.stairs {
                break;
            }
            let fi = rand_range(&mut state, gw as u32) as i32;
            let fj = rand_range(&mut state, gd as u32) as i32;
            // Foot must rest on the lower floor.
            if !layout.is_floor(lower, fi, fj) {
                continue;
            }
            let (di, dj) = dirs[rand_range(&mut state, 4) as usize];
            let head = (fi + di * (run - 1), fj + dj * (run - 1));
            // Head must rest on the upper floor and the whole run stay in bounds.
            if !layout.is_floor(upper, head.0, head.1) {
                continue;
            }
            let cells: Vec<(i32, i32)> =
                (0..run).map(|s| (fi + di * s, fj + dj * s)).collect();
            if cells
                .iter()
                .any(|&(i, j)| i < 0 || j < 0 || i >= gw || j >= gd)
            {
                continue;
            }
            // Reserve the shaft: open the upper deck and register the run as a
            // walkable stair surface on the upper level.
            for &(i, j) in &cells {
                let k = layout.idx(i, j);
                layout.grids[upper].opening[k] = true;
                layout.grids[upper].stair_cells[k] = true;
            }
            layout.stairs.push(Stair {
                lower_level: lower,
                cells,
            });
            placed += 1;
        }
    }
}
