//! Apartment-block layout variant with an explicit central corridor.
//!
//! Cuts a 1.5 m wide strip along the longer axis of the floorplate, BSP's
//! the two remaining halves independently, and emits the corridor as a
//! first-class `RoomCell` carrying the `corridor` room_type index. Every
//! side room ends up sharing one full edge with the corridor, so the
//! interior-door BFS — rooted at the corridor — produces a hub-and-spoke
//! door layout instead of chaining rooms via shared internal walls.
//!
//! Falls back to plain BSP if the caller doesn't pass a corridor index
//! (used by `layout::solve_storey` when no `corridor` room_type is
//! declared, or when only enough room budget exists for the corridor and
//! one room).

use super::common::{filter_types_excluding, split_with_corridor};
use super::{bsp, CellKind, Rect2, RoomCell};

const CORRIDOR_WIDTH: f32 = 1.5;

pub(super) fn layout(
    bounds: Rect2,
    assigned_types: &[usize],
    corridor_type_idx: usize,
    state: &mut u32,
) -> Vec<RoomCell> {
    // The corridor needs enough length to actually be useful; if the
    // floorplate is too small, fall back to plain BSP so we don't carve
    // a dead corridor through a one-room shed.
    let along_x = bounds.width() >= bounds.depth();
    let cross_extent = if along_x { bounds.depth() } else { bounds.width() };
    if cross_extent < CORRIDOR_WIDTH + 3.2 {
        return bsp::layout(bounds, assigned_types, state);
    }

    // Strip any corridor instances out of the type list — the BSP'd halves
    // are room-only. We then keep `assigned_types.len()` total cells
    // (corridor + side rooms summing back to the requested count).
    let side_types = filter_types_excluding(assigned_types, corridor_type_idx);
    if side_types.is_empty() {
        return bsp::layout(bounds, assigned_types, state);
    }

    let split = split_with_corridor(bounds, CORRIDOR_WIDTH);

    // Split the side-room types across the two halves. The half closer to
    // the south entrance gets the slight majority when the count is odd —
    // that bias matches typical apartment plans (front-of-house clustered
    // near the entrance).
    let count_a = side_types.len().div_ceil(2);
    let count_b = side_types.len() - count_a;
    let (types_a, types_b) = side_types.split_at(count_a);

    let mut cells: Vec<RoomCell> = Vec::new();
    cells.push(RoomCell {
        rect: split.corridor,
        room_type_index: corridor_type_idx,
        kind: CellKind::Room,
        door_slots: Vec::new(),
    });
    if count_a > 0 {
        cells.extend(bsp::layout(split.half_a, types_a, state));
    }
    if count_b > 0 {
        cells.extend(bsp::layout(split.half_b, types_b, state));
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corridor_runs_along_longer_axis() {
        let bounds = Rect2 { x_min: -6.0, x_max: 6.0, z_min: -3.0, z_max: 3.0 };
        let mut s = 1u32;
        let cells = layout(bounds, &[1, 1, 2, 2, 0], 0, &mut s);
        // First cell is the corridor.
        let c = &cells[0];
        assert_eq!(c.room_type_index, 0);
        // Width >= depth, so corridor spans full X and is thin in Z.
        assert!((c.rect.x_max - c.rect.x_min) > 10.0);
        assert!((c.rect.z_max - c.rect.z_min) - 1.5 < 0.01);
    }

    #[test]
    fn corridor_falls_back_to_bsp_on_small_floor() {
        let bounds = Rect2 { x_min: -2.0, x_max: 2.0, z_min: -1.5, z_max: 1.5 };
        let mut s = 1u32;
        let cells = layout(bounds, &[1, 2], 0, &mut s);
        // No cell should be the corridor type — BSP fallback engaged.
        assert!(cells.iter().all(|c| c.room_type_index != 0));
    }

    #[test]
    fn side_rooms_touch_corridor() {
        let bounds = Rect2 { x_min: -6.0, x_max: 6.0, z_min: -3.0, z_max: 3.0 };
        let mut s = 7u32;
        let cells = layout(bounds, &[1, 2, 1, 2, 1], 0, &mut s);
        let corridor = cells[0].rect;
        for side in &cells[1..] {
            let edge = side.rect.shared_edge_length(&corridor);
            assert!(
                edge > 0.1,
                "side cell {:?} doesn't share a usable edge with corridor {:?}",
                side.rect,
                corridor
            );
        }
    }
}
