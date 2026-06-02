//! Furnishing pass: drops transform-only POI markers into each room naming
//! the props a game engine should place there.
//!
//! `building` emits empty shells — this pass does *not* add geometry. Each
//! marker is a marker node (`kind="poi"`, `role=<prop>`, `tags=["building",
//! "poi", "furniture", <prop>]`) parented under a `furniture` group inside the
//! room cell, exactly the cave-POI contract: the exporter stamps role + tags
//! into `node.extras`, and a Godot importer drops a prefab at each transform.
//!
//! Placement is a pure, deterministic function of `seed` + the room rect:
//! wall props pack around the perimeter facing inward, corner props tuck into
//! corners, centre props sit mid-room, scatter props land at seeded interior
//! points, and ceiling fixtures grid out at ceiling height. Which props a room
//! gets is decided by [`catalog::classify`] from the author's `room_type`
//! name (`"kitchen"`, `"server room"`, …).

mod catalog;

use std::path::Path;

use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Transform};

use crate::lower::poi::{emit_poi_group, PoiDebug, PoiMarker};

use super::super::config::{BuildingCfg, RoomKind};
use super::super::layout::Rect2;
use super::super::rng::{rand_f01, step};
use super::openings::{Opening, OpeningPlan};
use catalog::{Category, Item, Place};

/// Gap left between adjacent wall props, metres.
const WALL_GAP: f32 = 0.15;
/// Keep wall props this far from the room corners so two perpendicular runs
/// don't overlap at the join.
const CORNER_MARGIN: f32 = 0.35;
/// Clearance kept around each opening, beyond its half-width, so a wall prop
/// never butts directly against a door or window.
const OPENING_CLEAR: f32 = 0.3;

/// Emit furnishing markers for one room cell. `rect` is the room's world-space
/// rectangle; `cell_id` is the cell group, whose local origin sits at the room
/// centre with `y=0` on the floor — so every marker transform below is in that
/// local frame.
pub(super) fn emit_room_furnishings(
    cfg: &BuildingCfg,
    rect: Rect2,
    room_name: &str,
    room_kind: RoomKind,
    plan: &OpeningPlan,
    cell_id: NodeId,
    origin: Option<&Path>,
    graph: &mut SceneGraph,
) {
    let w = rect.width();
    let d = rect.depth();
    let area = w * d;

    // Usable interior half-extents (room minus wall thickness + a margin).
    let inset = cfg.wall_thickness + 0.08;
    let hw = 0.5 * w - inset;
    let hd = 0.5 * d - inset;
    if hw <= 0.25 || hd <= 0.25 {
        return; // cupboard-sized: nothing fits cleanly
    }

    let cat = catalog::classify(room_name, room_kind);
    let items = catalog::items(cat);

    let keepouts = wall_keepouts(rect, plan);
    let mut state = furn_seed(cfg.seed, rect);
    let markers = lay_out(items, area, hw, hd, cfg.ceiling_height, &keepouts, &mut state);
    if markers.is_empty() {
        return;
    }

    // One emissive debug colour per category, so the debug spheres read as a
    // heat-map of room function. Shared across every marker in this room.
    let dbg_mat = debug_mat_name(cat);
    let dbg_color = debug_color(cat);
    let poi_markers: Vec<PoiMarker> = markers
        .into_iter()
        .map(|m| PoiMarker {
            name_key: m.role.to_string(),
            role: m.role.to_string(),
            tags: vec![
                "building".to_string(),
                "poi".to_string(),
                "furniture".to_string(),
                m.role.to_string(),
            ],
            transform: Transform::from_trs(
                Vec3::new(m.x, m.y, m.z),
                Quat::from_rotation_y(m.yaw),
                Vec3::ONE,
            ),
            debug: Some(PoiDebug {
                mat_name: dbg_mat.clone(),
                color: dbg_color,
                radius: 0.12,
            }),
        })
        .collect();

    emit_poi_group(
        graph,
        cell_id,
        origin,
        "furniture",
        &[
            "building".to_string(),
            "furniture".to_string(),
            format!("cat={}", cat.tag()),
        ],
        cfg.debug_show_poi,
        poi_markers,
    );
}

struct Marker {
    role: &'static str,
    x: f32,
    y: f32,
    z: f32,
    yaw: f32,
}

#[derive(Clone, Copy)]
enum Side {
    South,
    North,
    West,
    East,
}

impl Side {
    fn yaw(self) -> f32 {
        use std::f32::consts::{FRAC_PI_2, PI};
        match self {
            Side::South => 0.0,
            Side::North => PI,
            Side::West => FRAC_PI_2,
            Side::East => -FRAC_PI_2,
        }
    }
    /// World-local (x, z) for a prop at along-axis offset `along`, sitting
    /// against this wall and offset inward so the prop body clears the wall.
    fn place(self, along: f32, hw: f32, hd: f32) -> (f32, f32) {
        let off = |perp_half: f32| 0.3_f32.min(0.4 * perp_half);
        match self {
            Side::South => (along, -hd + off(hd)),
            Side::North => (along, hd - off(hd)),
            Side::West => (-hw + off(hw), along),
            Side::East => (hw - off(hw), along),
        }
    }
}

/// A contiguous run of free wall — the gaps left between openings. Props pack
/// into it left-to-right starting at `cursor` (an absolute along-axis coord).
struct FreeSpan {
    hi: f32,
    cursor: f32,
}

struct Wall {
    side: Side,
    spans: Vec<FreeSpan>,
}

impl Wall {
    fn new(side: Side, half: f32, keepouts: &[(f32, f32)]) -> Self {
        let lo = -(half - CORNER_MARGIN);
        let hi = half - CORNER_MARGIN;
        Wall { side, spans: free_spans(lo, hi, keepouts) }
    }
}

/// Subtract the opening keep-out intervals from `[lo, hi]`, returning the runs
/// of wall left free for props. Keep-outs are clipped to the range and merged
/// so overlapping or adjacent openings leave one gap, not several slivers.
fn free_spans(lo: f32, hi: f32, keepouts: &[(f32, f32)]) -> Vec<FreeSpan> {
    if hi <= lo {
        return Vec::new();
    }
    let mut ks: Vec<(f32, f32)> = keepouts
        .iter()
        .map(|&(a, b)| (a.max(lo), b.min(hi)))
        .filter(|&(a, b)| b > a)
        .collect();
    ks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut spans = Vec::new();
    let mut cur = lo;
    for (a, b) in ks {
        if a > cur {
            spans.push(FreeSpan { hi: a, cursor: cur });
        }
        cur = cur.max(b);
    }
    if cur < hi {
        spans.push(FreeSpan { hi, cursor: cur });
    }
    spans
}

fn lay_out(
    items: &[Item],
    area: f32,
    hw: f32,
    hd: f32,
    ceiling_h: f32,
    keepouts: &[Vec<(f32, f32)>; 4],
    state: &mut u32,
) -> Vec<Marker> {
    let mut markers = Vec::new();

    let mut walls = [
        Wall::new(Side::South, hw, &keepouts[0]),
        Wall::new(Side::North, hw, &keepouts[1]),
        Wall::new(Side::West, hd, &keepouts[2]),
        Wall::new(Side::East, hd, &keepouts[3]),
    ];

    // Corner anchors: inset diagonally so the prop clears both walls. Inward
    // yaw faces the room centre.
    let cc = 0.45_f32.min(0.4 * hw.min(hd));
    let corners = [
        (-(hw - cc), -(hd - cc)),
        (hw - cc, -(hd - cc)),
        (hw - cc, hd - cc),
        (-(hw - cc), hd - cc),
    ];
    let mut corner_cursor = 0usize;

    let mut centre_queue: Vec<&Item> = Vec::new();
    let mut scatter_queue: Vec<&Item> = Vec::new();
    let mut ceiling_queue: Vec<&Item> = Vec::new();

    for it in items {
        if area < it.min_area {
            continue;
        }
        let n = it.count(area);
        match it.place {
            Place::Wall => {
                for _ in 0..n {
                    if let Some((wi, si)) = pick_wall(&walls, it.width) {
                        let side = walls[wi].side;
                        let span = &mut walls[wi].spans[si];
                        let along = span.cursor + 0.5 * it.width;
                        span.cursor += it.width + WALL_GAP;
                        let (x, z) = side.place(along, hw, hd);
                        markers.push(Marker { role: it.role, x, y: it.y, z, yaw: side.yaw() });
                    }
                }
            }
            Place::Corner => {
                for _ in 0..n {
                    let (cx, cz) = corners[corner_cursor % 4];
                    corner_cursor += 1;
                    let yaw = (-cx).atan2(-cz); // face the centre
                    markers.push(Marker { role: it.role, x: cx, y: it.y, z: cz, yaw });
                }
            }
            Place::Centre => {
                for _ in 0..n {
                    centre_queue.push(it);
                }
            }
            Place::Scatter => {
                for _ in 0..n {
                    scatter_queue.push(it);
                }
            }
            Place::Ceiling => {
                for _ in 0..n {
                    ceiling_queue.push(it);
                }
            }
        }
    }

    place_centre(&centre_queue, hw, hd, &mut markers);
    place_scatter(&scatter_queue, hw, hd, state, &mut markers);
    place_ceiling(&ceiling_queue, hw, hd, ceiling_h, &mut markers);

    markers
}

/// Best-fit: across every wall's free spans, the span with the most room left
/// that still fits `width`. Ties break toward the earlier wall/span (south,
/// then north, …) for determinism.
fn pick_wall(walls: &[Wall; 4], width: f32) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    let mut best_rem = -1.0;
    for (wi, w) in walls.iter().enumerate() {
        for (si, sp) in w.spans.iter().enumerate() {
            let rem = sp.hi - sp.cursor;
            if rem + 1e-3 >= width && rem > best_rem {
                best = Some((wi, si));
                best_rem = rem;
            }
        }
    }
    best
}

/// For each wall (indexed South, North, West, East to match `lay_out`'s wall
/// array), the along-axis intervals blocked by a door or window so wall props
/// pack around them. Coordinates are in the room-local frame (origin at the
/// room centre), matching `Side::place`'s `along` axis: local X for
/// South/North walls, local Z for West/East walls.
fn wall_keepouts(rect: Rect2, plan: &OpeningPlan) -> [Vec<(f32, f32)>; 4] {
    const EDGE_EPS: f32 = 0.06;
    let [cx, cz] = rect.centre();
    let mut out: [Vec<(f32, f32)>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut block = |o: &Opening| {
        let half = 0.5 * o.width + OPENING_CLEAR;
        let in_x = o.x >= rect.x_min - EDGE_EPS && o.x <= rect.x_max + EDGE_EPS;
        let in_z = o.z >= rect.z_min - EDGE_EPS && o.z <= rect.z_max + EDGE_EPS;
        if in_x && (o.z - rect.z_min).abs() < EDGE_EPS {
            let a = o.x - cx;
            out[0].push((a - half, a + half));
        }
        if in_x && (o.z - rect.z_max).abs() < EDGE_EPS {
            let a = o.x - cx;
            out[1].push((a - half, a + half));
        }
        if in_z && (o.x - rect.x_min).abs() < EDGE_EPS {
            let a = o.z - cz;
            out[2].push((a - half, a + half));
        }
        if in_z && (o.x - rect.x_max).abs() < EDGE_EPS {
            let a = o.z - cz;
            out[3].push((a - half, a + half));
        }
    };
    for o in plan
        .entrances
        .iter()
        .chain(&plan.interior_doors)
        .chain(&plan.windows)
    {
        block(o);
    }
    out
}

fn place_centre(queue: &[&Item], hw: f32, hd: f32, out: &mut Vec<Marker>) {
    if queue.is_empty() {
        return;
    }
    // Spread along the room's longer axis so two centre props (e.g. a dining
    // table and a rug) don't stack on the exact same point.
    let along_x = hw >= hd;
    let span = if along_x { hw } else { hd } * 0.9;
    let n = queue.len();
    for (i, it) in queue.iter().enumerate() {
        let t = if n == 1 {
            0.0
        } else {
            (i as f32 / (n as f32 - 1.0) - 0.5) * 2.0 * span * 0.6
        };
        let (x, z) = if along_x { (t, 0.0) } else { (0.0, t) };
        out.push(Marker { role: it.role, x, y: it.y, z, yaw: 0.0 });
    }
}

fn place_scatter(queue: &[&Item], hw: f32, hd: f32, state: &mut u32, out: &mut Vec<Marker>) {
    let mx = (hw - 0.3).max(0.0);
    let mz = (hd - 0.3).max(0.0);
    for it in queue {
        let x = (rand_f01(state) * 2.0 - 1.0) * mx;
        let z = (rand_f01(state) * 2.0 - 1.0) * mz;
        let yaw = rand_f01(state) * std::f32::consts::TAU;
        out.push(Marker { role: it.role, x, y: it.y, z, yaw });
    }
}

fn place_ceiling(queue: &[&Item], hw: f32, hd: f32, ceiling_h: f32, out: &mut Vec<Marker>) {
    let n = queue.len();
    if n == 0 {
        return;
    }
    // Lay fixtures out on a roughly-square grid centred on the room.
    let cols = (n as f32).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let mut k = 0usize;
    for r in 0..rows {
        for c in 0..cols {
            if k >= n {
                break;
            }
            let fx = if cols == 1 { 0.5 } else { (c as f32 + 0.5) / cols as f32 };
            let fz = if rows == 1 { 0.5 } else { (r as f32 + 0.5) / rows as f32 };
            let x = (fx * 2.0 - 1.0) * hw * 0.8;
            let z = (fz * 2.0 - 1.0) * hd * 0.8;
            out.push(Marker { role: queue[k].role, x, y: ceiling_h, z, yaw: 0.0 });
            k += 1;
        }
    }
}

/// Per-room deterministic seed: the user seed mixed with the room centre so
/// two identically-shaped rooms in different spots furnish differently, but a
/// rebuild at the same seed reproduces every marker exactly.
fn furn_seed(seed: u32, rect: Rect2) -> u32 {
    let [cx, cz] = rect.centre();
    let qx = (cx * 16.0).round() as i32 as u32;
    let qz = (cz * 16.0).round() as i32 as u32;
    let mut s = seed.wrapping_mul(0x9E37_79B9)
        ^ qx.wrapping_mul(0x85EB_CA6B)
        ^ qz.wrapping_mul(0xC2B2_AE35);
    step(&mut s);
    step(&mut s);
    s.max(1)
}

fn debug_mat_name(cat: Category) -> String {
    format!("building_furniture_{}", cat.tag())
}

fn debug_color(cat: Category) -> [f32; 3] {
    use Category::*;
    match cat {
        Bedroom => [0.95, 0.45, 0.65],
        Bathroom => [0.3, 0.75, 0.95],
        Kitchen | Pantry => [1.0, 0.6, 0.2],
        Dining | Restaurant => [0.95, 0.8, 0.25],
        Living => [0.55, 0.85, 0.4],
        Office | Meeting => [0.4, 0.6, 1.0],
        Reception | Lobby => [0.7, 0.5, 0.95],
        Corridor => [0.6, 0.6, 0.6],
        Storage | Closet | Warehouse => [0.8, 0.65, 0.45],
        Garage | Workshop => [0.85, 0.4, 0.3],
        Laundry | Utility | ServerRoom => [0.45, 0.85, 0.8],
        Retail | Bar => [1.0, 0.35, 0.75],
        Classroom | Library | Lab => [0.5, 0.75, 0.95],
        Medical | Ward => [0.95, 0.95, 0.95],
        Gym => [0.95, 0.55, 0.15],
        Cell => [0.45, 0.45, 0.5],
        Generic => [0.75, 0.75, 0.75],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(lo: f32, hi: f32, ks: &[(f32, f32)]) -> Vec<(f32, f32)> {
        free_spans(lo, hi, ks).iter().map(|s| (s.cursor, s.hi)).collect()
    }

    #[test]
    fn free_spans_no_openings_is_one_run() {
        assert_eq!(spans(-2.0, 2.0, &[]), vec![(-2.0, 2.0)]);
    }

    #[test]
    fn free_spans_splits_around_a_central_opening() {
        // A door blocking [-0.5, 0.5] leaves a run on each side.
        assert_eq!(spans(-2.0, 2.0, &[(-0.5, 0.5)]), vec![(-2.0, -0.5), (0.5, 2.0)]);
    }

    #[test]
    fn free_spans_merges_overlapping_keepouts() {
        // Two overlapping openings collapse to a single gap, not slivers.
        assert_eq!(spans(-2.0, 2.0, &[(-0.5, 0.4), (0.2, 0.8)]), vec![(-2.0, -0.5), (0.8, 2.0)]);
    }

    #[test]
    fn free_spans_clips_to_range_and_can_be_empty() {
        // An opening spanning the whole wall leaves nothing to pack into.
        assert!(spans(-1.0, 1.0, &[(-5.0, 5.0)]).is_empty());
    }
}
