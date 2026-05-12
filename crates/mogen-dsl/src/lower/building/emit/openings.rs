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

/// Window class chosen for the whole storey. Mixing classes on the same
/// facade reads as visual noise; picking one keeps every window the same
/// size so the resulting row reads as a regular fenestration band.
const STOREY_WINDOW_CLASS: WindowClass = WindowClass::Medium;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpeningKind {
    Entrance,
    InteriorDoor,
    Window(WindowClass),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // `Small` / `Large` stay in the enum so the
                    // `cfg.windows_mod.small` / `.large` routing in
                    // `modules.rs` keeps compiling. Placement uses
                    // `STOREY_WINDOW_CLASS` (Medium) for every window.
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
    place_elevator_doors(cfg, plate, &mut plan);

    let storey_windows = windows_for_storey(cfg, ctx);
    if storey_windows > 0 {
        place_windows(cfg, plate, storey_windows, &mut plan);
    }
    plan
}

/// Drop one elevator doorway on every elevator's west face, 1.5 ×
/// `cfg.door_w` wide. The Z defaults to the elevator's centre but we
/// shift per storey to land entirely within a single adjacent room when
/// a room-room interior wall would otherwise T-junction the elevator's
/// west face inside the door cutout — the wall body's wall-thickness
/// intrusion reads as a wall blocking the elevator door. These entries
/// drive both the door-panel module instances in `emit_module_instances`
/// and the matching shaft-wall cutouts in `emit/circulation.rs`, so
/// panel and cutout always line up.
fn place_elevator_doors(cfg: &BuildingCfg, plate: &Floorplate, plan: &mut OpeningPlan) {
    for cell in &plate.rooms {
        if !matches!(cell.kind, CellKind::Elevator) {
            continue;
        }
        let z = elevator_door_z(cfg, plate, cell);
        plan.interior_doors.push(Opening {
            kind: OpeningKind::InteriorDoor,
            x: cell.rect.x_min,
            z,
            sill: 0.0,
            width: cfg.door_w * 1.5,
            height: cfg.door_h,
            side: None,
            facing: [-1.0, 0.0, 0.0],
        });
    }
}

/// Pick the world Z for an elevator's west-face doorway on this storey.
///
/// We look at each room whose east edge sits on the elevator's west face
/// and compute its overlap with the elevator's z range. The widest such
/// overlap is the room the door should open into. If the 1.5×-wide door
/// fits inside that overlap with a wall-thickness corner margin (so the
/// flanking room-room walls' z-thickness sits clear of the cutout), we
/// nudge the door toward the elevator's centre inside that room. If no
/// adjacent room is wide enough, we fall back to the elevator's centre
/// — the residual wall-in-cutout artifact on that storey is preferable
/// to narrowing the door (which would fail
/// `elevator_has_a_1p5x_door_on_every_floor`).
pub(super) fn elevator_door_z(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    elevator: &RoomCell,
) -> f32 {
    let elev_z_min = elevator.rect.z_min;
    let elev_z_max = elevator.rect.z_max;
    let elev_x_west = elevator.rect.x_min;
    let centre_z = 0.5 * (elev_z_min + elev_z_max);
    let width = cfg.door_w * 1.5;
    let half_w = 0.5 * width;
    let margin = cfg.wall_thickness;

    let mut adj: Vec<(f32, f32)> = Vec::new();
    for cell in &plate.rooms {
        if !matches!(cell.kind, CellKind::Room) {
            continue;
        }
        if (cell.rect.x_max - elev_x_west).abs() > 1e-3 {
            continue;
        }
        let z_lo = cell.rect.z_min.max(elev_z_min);
        let z_hi = cell.rect.z_max.min(elev_z_max);
        if z_hi - z_lo > 1e-3 {
            adj.push((z_lo, z_hi));
        }
    }
    if adj.is_empty() {
        return centre_z;
    }
    adj.sort_by(|a, b| {
        (b.1 - b.0)
            .partial_cmp(&(a.1 - a.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for &(z_lo, z_hi) in &adj {
        if z_hi - z_lo < width + 2.0 * margin {
            continue;
        }
        let lo_clamp = z_lo + half_w + margin;
        let hi_clamp = z_hi - half_w - margin;
        return centre_z.clamp(lo_clamp, hi_clamp);
    }
    centre_z
}

/// Place exterior entrances across the building's perimeter.
///
/// One entrance always lands on the south wall — that's the canonical
/// "front" the layout solver scores rooms against, and the corridor /
/// hotel styles pivot their long axis to match it. Additional entrances
/// fan out round-robin to the other facades in N → E → W order so a
/// multi-door building (corner shop, courtyard house, public building
/// with street-facing entries on more than one side) reads with doors on
/// every facade rather than a south-wall row.
fn place_entrances(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    plan: &mut OpeningPlan,
    state: &mut u32,
) {
    let count = cfg.entrances.max(1) as usize;
    const ORDER: [WallSide; 4] = [
        WallSide::South,
        WallSide::North,
        WallSide::East,
        WallSide::West,
    ];
    let mut per_side: [usize; 4] = [0; 4];
    for i in 0..count {
        per_side[i % 4] += 1;
    }
    for (side_idx, &side) in ORDER.iter().enumerate() {
        let n = per_side[side_idx];
        if n == 0 {
            continue;
        }
        place_entrances_on_side(cfg, plate, side, n, plan, state);
    }
}

fn place_entrances_on_side(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    side: WallSide,
    count: usize,
    plan: &mut OpeningPlan,
    state: &mut u32,
) {
    let bounds = &plate.bounds;
    let (span, along_min, fixed, facing) = match side {
        WallSide::South => (
            bounds.x_max - bounds.x_min,
            bounds.x_min,
            bounds.z_min,
            [0.0, 0.0, -1.0],
        ),
        WallSide::North => (
            bounds.x_max - bounds.x_min,
            bounds.x_min,
            bounds.z_max,
            [0.0, 0.0, 1.0],
        ),
        WallSide::East => (
            bounds.z_max - bounds.z_min,
            bounds.z_min,
            bounds.x_max,
            [1.0, 0.0, 0.0],
        ),
        WallSide::West => (
            bounds.z_max - bounds.z_min,
            bounds.z_min,
            bounds.x_min,
            [-1.0, 0.0, 0.0],
        ),
    };
    let usable = (span - 2.0 * cfg.door_w).max(0.1);
    for i in 0..count {
        let t = (i as f32 + 1.0) / (count as f32 + 1.0);
        let jitter = (rand_f01(state) - 0.5) * 0.4 / (count as f32 + 1.0);
        let along = along_min + cfg.door_w + (t + jitter).clamp(0.05, 0.95) * usable;
        let (x, z) = match side {
            WallSide::South | WallSide::North => (along, fixed),
            WallSide::East | WallSide::West => (fixed, along),
        };
        plan.entrances.push(Opening {
            kind: OpeningKind::Entrance,
            x,
            z,
            sill: 0.0,
            width: cfg.door_w,
            height: cfg.door_h,
            side: Some(side),
            facing,
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
    let mut edges: Vec<(usize, usize, f32, [f32; 2], f32)> = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            // Never carve a door between two circulation cells — you
            // can't step out of a stairwell into an elevator shaft, and
            // wasting a tree edge here can leave the elevator/stair
            // disconnected from the rest of the rooms.
            if is_circulation(&plate.rooms[i].kind)
                && is_circulation(&plate.rooms[j].kind)
            {
                continue;
            }
            // Elevators get their door directly from the shaft (see
            // `place_elevator_doors` + `emit_shaft_enclosure`). The
            // BFS shouldn't try to reach the elevator through a room
            // edge — that would add a second, smaller door at the
            // overlap midpoint, fighting the shaft's centred cutout.
            if matches!(plate.rooms[i].kind, CellKind::Elevator)
                || matches!(plate.rooms[j].kind, CellKind::Elevator)
            {
                continue;
            }
            let edge = plate.rooms[i].rect.shared_edge_length(&plate.rooms[j].rect);
            // Eligibility uses the standard door width so the BFS can
            // still reach an elevator whose neighbour is too narrow to
            // hold the wider 1.5× doorway — connectivity beats ideal
            // sizing. Width selection happens per-edge below.
            if edge >= cfg.door_w * 1.1 {
                let door_w = edge_door_width(cfg, &plate.rooms[i], &plate.rooms[j], edge);
                let mid = shared_edge_door_anchor(
                    &plate.rooms[i],
                    &plate.rooms[j],
                    cfg,
                );
                edges.push((i, j, edge, mid, door_w));
            }
        }
    }
    let root = pick_door_tree_root(cfg, plate, plan);
    let mut visited = vec![false; n];
    let mut queue = std::collections::VecDeque::new();
    visited[root] = true;
    queue.push_back(root);
    while let Some(u) = queue.pop_front() {
        let mut neighbours: Vec<usize> = edges
            .iter()
            .filter_map(|(a, b, _, _, _)| {
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
            if let Some((_, _, _, mid, door_w)) = edges
                .iter()
                .find(|(a, b, _, _, _)| (*a == u && *b == v) || (*a == v && *b == u))
            {
                let facing = interior_facing(&plate.rooms[u].rect, &plate.rooms[v].rect);
                // `shared_edge_midpoint` already gave us the geometric
                // midpoint along the shared edge; we additionally clamp it
                // away from the corner so the door clears any perpendicular
                // wall sitting at a T-junction. Corner margin =
                // door_w/2 + wall_thickness so the door's edge stops at
                // least a wall-thickness short of the adjacent corner.
                let corner_margin = 0.5 * door_w + cfg.wall_thickness;
                let (x, z) = clamp_door_to_edge(
                    &plate.rooms[u].rect,
                    &plate.rooms[v].rect,
                    *mid,
                    corner_margin,
                );
                plan.interior_doors.push(Opening {
                    kind: OpeningKind::InteriorDoor,
                    x,
                    z,
                    sill: 0.0,
                    width: *door_w,
                    height: cfg.door_h,
                    side: None,
                    facing,
                });
            }
            queue.push_back(v);
        }
    }
}

/// Clamp a door's anchor along its shared edge so it stops at least
/// `corner_margin` from each end. If the edge is shorter than 2× the
/// margin (a very small shared edge from BSP), we centre the door and
/// accept the corner overlap — the door has to land somewhere.
fn clamp_door_to_edge(a: &Rect2, b: &Rect2, mid: [f32; 2], corner_margin: f32) -> (f32, f32) {
    // Vertical shared edge (along Z) at x = a.x_max or b.x_max.
    if (a.x_max - b.x_min).abs() < 1e-3 || (b.x_max - a.x_min).abs() < 1e-3 {
        let lo = a.z_min.max(b.z_min);
        let hi = a.z_max.min(b.z_max);
        let z = clamp_centre(mid[1], lo, hi, corner_margin);
        return (mid[0], z);
    }
    // Horizontal shared edge (along X).
    if (a.z_max - b.z_min).abs() < 1e-3 || (b.z_max - a.z_min).abs() < 1e-3 {
        let lo = a.x_min.max(b.x_min);
        let hi = a.x_max.min(b.x_max);
        let x = clamp_centre(mid[0], lo, hi, corner_margin);
        return (x, mid[1]);
    }
    (mid[0], mid[1])
}

fn is_circulation(kind: &CellKind) -> bool {
    matches!(kind, CellKind::Staircase | CellKind::Elevator)
}

/// Doorways onto an elevator cell are widened to 1.5× the standard door
/// to read as proper elevator-lobby openings, but only when the shared
/// edge can actually hold the wider opening with a wall-thickness corner
/// margin on each side. When the layout produces a narrower shared edge
/// (e.g. the grid style where rooms aren't guaranteed to span the full
/// 2 m elevator face), we fall back to `cfg.door_w` so the elevator
/// still gets a doorway on that storey — connectivity outranks the
/// ideal width.
fn edge_door_width(cfg: &BuildingCfg, a: &RoomCell, b: &RoomCell, edge: f32) -> f32 {
    let elev_involved =
        matches!(a.kind, CellKind::Elevator) || matches!(b.kind, CellKind::Elevator);
    if !elev_involved {
        return cfg.door_w;
    }
    let wide = cfg.door_w * 1.5;
    if edge >= wide + 2.0 * cfg.wall_thickness {
        wide
    } else {
        cfg.door_w
    }
}

fn clamp_centre(value: f32, lo: f32, hi: f32, margin: f32) -> f32 {
    let span = hi - lo;
    if span <= 2.0 * margin {
        return 0.5 * (lo + hi);
    }
    value.clamp(lo + margin, hi - margin)
}

/// Place exterior windows symmetrically across each room's exterior wall
/// segment, with overlap-proof spacing.
///
/// Placement is deterministic — no RNG. For each room cell × exterior
/// side, we treat the cell's segment of that wall as a self-contained
/// run and drop `n` evenly-spaced windows on it at fractions
/// `(j+1) / (n+1)`. That subdivision is the textbook symmetric layout:
/// every window sits at the centre of its share of the run, the gaps at
/// each end equal half the inter-window pitch, and the whole row is
/// mirror-symmetric about the segment's centre.
///
/// Overlap is avoided by capping each segment at a maximum number of
/// windows whose centres can be spaced at least `2·cw` apart. Smaller
/// segments accept fewer windows; the leftover budget either falls onto
/// other segments or is silently dropped — better than the previous
/// behaviour, which would cycle the same segment list and stack windows
/// on top of one another whenever `count > segments`.
///
/// The whole storey shares one window class (`STOREY_WINDOW_CLASS`) so
/// every window on a given floor reads as the same size, which is what
/// makes the facade scan as a consistent row rather than a mix of small
/// and large punctures.
fn place_windows(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    count: usize,
    plan: &mut OpeningPlan,
) {
    if count == 0 {
        return;
    }

    let class = STOREY_WINDOW_CLASS;
    let (cw, ch) = window_size(cfg, class);
    // Minimum centre-to-centre pitch — one window width of solid wall
    // between adjacent openings. Two `cw`-wide windows at this pitch
    // leave a `cw`-wide pier, which also keeps the wall mesh's
    // `wall_with_holes` cutouts from merging.
    let pitch = 2.0 * cw;

    let mut segments: Vec<ExtSeg> = Vec::new();
    for cell in &plate.rooms {
        if !matches!(cell.kind, CellKind::Room) {
            // Circulation cells get no exterior windows — stairwells are
            // typically lit by skylights or borrowed light.
            continue;
        }
        for side in [WallSide::North, WallSide::East, WallSide::South, WallSide::West] {
            if !room_touches_exterior(&cell.rect, &plate.bounds, side) {
                continue;
            }
            let (lo, hi, fixed, facing) = exterior_segment(cell, &plate.bounds, side);
            if segment_covered_by_entrance(side, lo, hi, cfg, plan) {
                continue;
            }
            // Need room for at least one window plus the half-pitch
            // margin at each end the `(j+1)/(n+1)` placement gives.
            if hi - lo < pitch {
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

    // Deterministic order so window ids and stamping iteration are
    // stable across runs and across emit-time iteration. Sorting by
    // (side, lo) walks the building clockwise from the north wall.
    segments.sort_by(|a, b| {
        side_order(a.side)
            .cmp(&side_order(b.side))
            .then(a.lo.partial_cmp(&b.lo).unwrap_or(std::cmp::Ordering::Equal))
    });

    let alloc = allocate_windows(&segments, count, pitch);

    for (idx, seg) in segments.iter().enumerate() {
        let n = alloc[idx];
        if n == 0 {
            continue;
        }
        let length = seg.hi - seg.lo;
        for j in 0..n {
            let t = (j as f32 + 1.0) / (n as f32 + 1.0);
            let along = seg.lo + t * length;
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
}

#[derive(Clone, Copy)]
struct ExtSeg {
    side: WallSide,
    lo: f32,
    hi: f32,
    fixed: f32,
    facing: [f32; 3],
}

fn side_order(s: WallSide) -> u8 {
    match s {
        WallSide::North => 0,
        WallSide::East => 1,
        WallSide::South => 2,
        WallSide::West => 3,
    }
}

fn segment_covered_by_entrance(
    side: WallSide,
    lo: f32,
    hi: f32,
    cfg: &BuildingCfg,
    plan: &OpeningPlan,
) -> bool {
    plan.entrances.iter().any(|e| {
        if e.side != Some(side) {
            return false;
        }
        let centre = match side {
            WallSide::South | WallSide::North => e.x,
            WallSide::East | WallSide::West => e.z,
        };
        centre - cfg.door_w >= lo && centre + cfg.door_w <= hi
    })
}

/// Distribute `count` windows across `segments`, weighted by length and
/// capped per segment at the most we can fit at `pitch` centre-to-centre.
/// Returns the per-segment allocation. The sum may be less than `count`
/// if the storey simply doesn't have enough qualifying exterior wall —
/// that's a feature, not a bug: stacking windows is the failure mode we
/// want to prevent.
fn allocate_windows(segments: &[ExtSeg], count: usize, pitch: f32) -> Vec<usize> {
    let max_per: Vec<usize> = segments
        .iter()
        .map(|s| ((s.hi - s.lo) / pitch).floor().max(0.0) as usize)
        .collect();
    let total_capacity: usize = max_per.iter().sum();
    let target = count.min(total_capacity);
    if target == 0 {
        return vec![0; segments.len()];
    }

    let total_len: f32 = segments.iter().map(|s| s.hi - s.lo).sum();
    let raw: Vec<f32> = segments
        .iter()
        .map(|s| target as f32 * (s.hi - s.lo) / total_len)
        .collect();

    let mut alloc: Vec<usize> = raw
        .iter()
        .enumerate()
        .map(|(i, r)| (r.floor() as usize).min(max_per[i]))
        .collect();
    let mut assigned: usize = alloc.iter().sum();

    // Hand out the remainder to segments with the largest fractional
    // part first — standard Hare quota — so the rounding bias goes to
    // the longest walls. Ties break on segment index for determinism.
    let mut residuals: Vec<(usize, f32)> = raw
        .iter()
        .enumerate()
        .map(|(i, r)| (i, r - r.floor()))
        .collect();
    residuals.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    for (i, _) in &residuals {
        if assigned >= target {
            break;
        }
        if alloc[*i] < max_per[*i] {
            alloc[*i] += 1;
            assigned += 1;
        }
    }
    // After Hare, if any segment with spare capacity remains (possible
    // when the residual list ran out before target was met), fill in
    // index order. Bounded by `total_capacity` so the loop always ends.
    let mut sweep = 0usize;
    while assigned < target && sweep < segments.len() {
        for i in 0..segments.len() {
            if assigned >= target {
                break;
            }
            if alloc[i] < max_per[i] {
                alloc[i] += 1;
                assigned += 1;
            }
        }
        sweep += 1;
    }
    alloc
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

/// Pick a door anchor along the shared edge between two cells.
///
/// Defaults to the geometric midpoint of the shared edge, with two
/// per-kind biases:
/// - **Staircase ↔ room:** the door is shifted toward the staircase's
///   south entry zone so it lands on the platform the switchback
///   expects users to step onto (off-centre would put it next to the
///   mid-landing slab edge or over the flight cutout).
/// - **Elevator ↔ room:** the door is anchored on the elevator's
///   geometric centre so it sits midway across the 2 m face regardless
///   of how the neighbouring room is sized along the shared axis. For
///   the common case where the room fully overlaps the elevator's
///   range this is identical to the midpoint; for partial overlaps it
///   pulls the door toward the centred position the elevator presents
///   on its open face, instead of sliding to whichever third of the
///   2 m face the room happens to cover.
fn shared_edge_door_anchor(
    a: &RoomCell,
    b: &RoomCell,
    cfg: &BuildingCfg,
) -> [f32; 2] {
    let mid = shared_edge_midpoint(&a.rect, &b.rect);
    let stair_target = |s: &Rect2| -> Option<f32> {
        // South entry zone runs from s.z_min to s.z_min + STAIR_ENTRY_DEPTH.
        // Anchor at the centre of that zone so the door lands squarely
        // on the platform.
        Some(s.z_min + 0.5 * super::circulation::STAIR_ENTRY_DEPTH)
    };
    let elev_target = |e: &Rect2| -> Option<f32> {
        Some(0.5 * (e.z_min + e.z_max))
    };
    let target_z = match (a.kind, b.kind) {
        (CellKind::Staircase, _) if !matches!(b.kind, CellKind::Staircase) => stair_target(&a.rect),
        (_, CellKind::Staircase) if !matches!(a.kind, CellKind::Staircase) => stair_target(&b.rect),
        (CellKind::Elevator, _) if !matches!(b.kind, CellKind::Elevator) => elev_target(&a.rect),
        (_, CellKind::Elevator) if !matches!(a.kind, CellKind::Elevator) => elev_target(&b.rect),
        _ => None,
    };
    let Some(target_z) = target_z else {
        return mid;
    };
    // Only bias the Z component if the shared edge is vertical (runs
    // along Z) — that's the wall the door sits in. Horizontal edges
    // get the unchanged midpoint.
    let vertical_edge = (a.rect.x_max - b.rect.x_min).abs() < 1e-3
        || (b.rect.x_max - a.rect.x_min).abs() < 1e-3;
    if !vertical_edge {
        return mid;
    }
    let lo = a.rect.z_min.max(b.rect.z_min);
    let hi = a.rect.z_max.min(b.rect.z_max);
    let margin = 0.5 * cfg.door_w + cfg.wall_thickness;
    let z = if hi - lo > 2.0 * margin {
        target_z.clamp(lo + margin, hi - margin)
    } else {
        0.5 * (lo + hi)
    };
    [mid[0], z]
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

/// Pick the cell that anchors the interior-door spanning tree. When a
/// `corridor` room_type is declared, we prefer the corridor cell so the
/// BFS fans doors out from the corridor to every adjacent room (hub-and-
/// spoke). Without a corridor we fall back to the room nearest any
/// entrance — multi-side entrances each pull the BFS root toward whichever
/// facade door is geometrically closest, so an entry-room-first chain
/// still forms. Upper storeys have no entrances; for those we anchor on
/// the south-midpoint baseline so the root stays deterministic.
fn pick_door_tree_root(cfg: &BuildingCfg, plate: &Floorplate, plan: &OpeningPlan) -> usize {
    if let Some(corridor_idx) = cfg.corridor_type_index() {
        for (i, cell) in plate.rooms.iter().enumerate() {
            if matches!(cell.kind, CellKind::Room) && cell.room_type_index == corridor_idx {
                return i;
            }
        }
    }
    let mut probes: Vec<[f32; 2]> = plan
        .entrances
        .iter()
        .map(|e| [e.x, e.z])
        .collect();
    if probes.is_empty() {
        probes.push([
            0.5 * (plate.bounds.x_min + plate.bounds.x_max),
            plate.bounds.z_min,
        ]);
    }
    let mut best = (f32::INFINITY, 0usize);
    for (i, cell) in plate.rooms.iter().enumerate() {
        let c = cell.rect.centre();
        let mut min_d2 = f32::INFINITY;
        for p in &probes {
            let dx = c[0] - p[0];
            let dz = c[1] - p[1];
            let d2 = dx * dx + dz * dz;
            if d2 < min_d2 {
                min_d2 = d2;
            }
        }
        if min_d2 < best.0 {
            best = (min_d2, i);
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
