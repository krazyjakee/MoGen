//! `style="hotel-corridor"` layout: a single straight corridor along the
//! longer axis with uniformly tiled side rooms on both long sides. Each
//! side room shares a full edge with the corridor, so the interior-door
//! BFS (rooted at the corridor by `pick_door_tree_root`) gives every room
//! a door directly onto the corridor — exactly what a hotel/dorm wants.
//!
//! Distinct from `bsp.rs` (the `apartment-block` style, which subdivides
//! the whole floorplate without a corridor): hotel rooms are uniformly
//! sized along the corridor instead of being recursively BSP'd, so the
//! result reads as the regular rhythm of identical rooms typical of
//! hotels rather than the irregular flat layouts BSP produces.

use super::common::{filter_types_excluding, split_with_corridor};
use super::{grid, CellKind, Rect2, RoomCell};

/// Corridor strip width.
const CORRIDOR_WIDTH: f32 = 1.8;
/// Minimum depth a hotel room is allowed to have (perpendicular to the
/// corridor). Below this the floorplate is too thin to carry rooms on
/// both sides; we fall back to grid.
const MIN_ROOM_DEPTH: f32 = 2.4;
/// Minimum length a hotel room is allowed to have along the corridor.
/// Below this we drop slots until every emitted room is at least this
/// wide.
const MIN_ROOM_RUN: f32 = 2.4;

pub(super) fn layout(
    bounds: Rect2,
    assigned_types: &[usize],
    corridor_type_idx: usize,
    state: &mut u32,
) -> Vec<RoomCell> {
    layout_with_target(
        bounds,
        assigned_types,
        corridor_type_idx,
        MIN_ROOM_RUN,
        state,
    )
}

/// Shared implementation used by both `hotel-corridor` and `office-core`.
/// `target_run` is the minimum (and roughly preferred) run length of a
/// single side cell along the corridor — hotels want longer runs, offices
/// shorter ones.
pub(super) fn layout_with_target(
    bounds: Rect2,
    assigned_types: &[usize],
    corridor_type_idx: usize,
    target_run: f32,
    state: &mut u32,
) -> Vec<RoomCell> {
    let along_x = bounds.width() >= bounds.depth();
    let cross_extent = if along_x { bounds.depth() } else { bounds.width() };
    if cross_extent < CORRIDOR_WIDTH + 2.0 * MIN_ROOM_DEPTH {
        return grid::layout(bounds, assigned_types, state);
    }

    let side_types = filter_types_excluding(assigned_types, corridor_type_idx);
    if side_types.is_empty() {
        return grid::layout(bounds, assigned_types, state);
    }

    let split = split_with_corridor(bounds, CORRIDOR_WIDTH);

    // Cap the per-side count so that every cell's run along the corridor
    // is at least `target_run`. Without this, requesting 30 rooms on a 5 m
    // corridor produces 15 paper-thin slits per side.
    let along_extent = if along_x { bounds.width() } else { bounds.depth() };
    let max_per_side = ((along_extent / target_run).floor() as usize).max(1);

    let half_n = side_types.len().div_ceil(2).min(max_per_side);
    let other_n = (side_types.len() - half_n).min(max_per_side);

    let types_a = &side_types[..half_n];
    let types_b_full = &side_types[half_n..];
    let types_b = &types_b_full[..other_n.min(types_b_full.len())];

    let mut cells: Vec<RoomCell> = Vec::with_capacity(1 + types_a.len() + types_b.len());
    cells.push(RoomCell {
        rect: split.corridor,
        room_type_index: corridor_type_idx,
        kind: CellKind::Room,
        door_slots: Vec::new(),
    });
    cells.extend(tile_along(split.half_a, types_a, along_x));
    cells.extend(tile_along(split.half_b, types_b, along_x));
    cells
}

fn tile_along(half: Rect2, types: &[usize], along_x: bool) -> Vec<RoomCell> {
    if types.is_empty() {
        return Vec::new();
    }
    let n = types.len();
    let mut out = Vec::with_capacity(n);
    if along_x {
        let step = half.width() / n as f32;
        for (i, &t) in types.iter().enumerate() {
            let x0 = half.x_min + i as f32 * step;
            let x1 = if i + 1 == n { half.x_max } else { x0 + step };
            out.push(RoomCell {
                rect: Rect2 {
                    x_min: x0,
                    x_max: x1,
                    z_min: half.z_min,
                    z_max: half.z_max,
                },
                room_type_index: t,
                kind: CellKind::Room,
                door_slots: Vec::new(),
            });
        }
    } else {
        let step = half.depth() / n as f32;
        for (i, &t) in types.iter().enumerate() {
            let z0 = half.z_min + i as f32 * step;
            let z1 = if i + 1 == n { half.z_max } else { z0 + step };
            out.push(RoomCell {
                rect: Rect2 {
                    x_min: half.x_min,
                    x_max: half.x_max,
                    z_min: z0,
                    z_max: z1,
                },
                room_type_index: t,
                kind: CellKind::Room,
                door_slots: Vec::new(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotel_corridor_runs_full_length() {
        let bounds = Rect2 { x_min: -10.0, x_max: 10.0, z_min: -4.0, z_max: 4.0 };
        let mut s = 1u32;
        let cells = layout(bounds, &[1, 1, 1, 1, 0], 0, &mut s);
        let c = &cells[0];
        assert_eq!(c.room_type_index, 0);
        // Corridor spans the full long axis.
        assert!((c.rect.x_max - c.rect.x_min) >= 19.9);
        assert!((c.rect.z_max - c.rect.z_min) - 1.8 < 0.01);
    }

    #[test]
    fn hotel_side_rooms_each_touch_corridor() {
        let bounds = Rect2 { x_min: -10.0, x_max: 10.0, z_min: -4.0, z_max: 4.0 };
        let mut s = 17u32;
        let cells = layout(bounds, &[1, 2, 1, 2, 1, 2], 0, &mut s);
        let corridor = cells[0].rect;
        for side in &cells[1..] {
            let edge = side.rect.shared_edge_length(&corridor);
            assert!(edge > 0.1, "side cell {:?} doesn't share an edge with corridor", side.rect);
        }
    }

    #[test]
    fn hotel_falls_back_to_grid_on_thin_floor() {
        let bounds = Rect2 { x_min: -3.0, x_max: 3.0, z_min: -1.5, z_max: 1.5 };
        let mut s = 1u32;
        let cells = layout(bounds, &[1, 2], 0, &mut s);
        // No corridor cell — fell back to grid.
        assert!(cells.iter().all(|c| c.room_type_index != 0));
    }
}
