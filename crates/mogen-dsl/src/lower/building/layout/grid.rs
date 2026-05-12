//! `style="grid"` layout: uniform grid subdivision sized to the room count.
//!
//! Picks `cols × rows` such that `cols * rows >= rooms` (closest to a √2
//! aspect) and tiles the room-layout rectangle into equal cells. Each tile
//! is assigned a room type from the sampled distribution.
//!
//! Operates on whatever rectangle the caller passes — the floorplate minus
//! the circulation column, if any — so multi-storey buildings with a
//! reserved stair column place rooms only in the room area.

use super::common::pick_aspect_grid;
use super::{CellKind, Rect2, RoomCell};

pub(super) fn layout(
    bounds: Rect2,
    assigned_types: &[usize],
    _state: &mut u32,
) -> Vec<RoomCell> {
    let rooms = assigned_types.len().max(1);
    let (cols, _rows) = pick_aspect_grid(rooms);

    // Use only as many rows as the assigned room count actually needs.
    // The picker may choose e.g. 3×2 for 3 rooms (closer to √2 aspect),
    // which would leave the entire north row empty — and any
    // circulation cell sitting in that empty band ends up without a
    // room neighbour, leaving the BFS unable to place a doorway from
    // a room into the elevator/staircase. Collapsing the empty rows
    // means the assigned rooms span the full floorplate in Z and so
    // always border the circulation column. The last row may still be
    // short on cells (partial fill across X), but that gap sits in the
    // floorplate interior, not under the column.
    let effective_rows = rooms.div_ceil(cols).max(1);

    let cell_d = bounds.depth() / effective_rows as f32;
    let mut cells: Vec<RoomCell> = Vec::with_capacity(rooms);
    let mut idx = 0usize;
    'rows: for r in 0..effective_rows {
        let row_cells = (rooms - idx).min(cols);
        // When the final row can't fill all `cols`, stretch its cells
        // horizontally to span the entire bounds width. This keeps the
        // east edge — where multi-storey circulation columns dock —
        // covered by a room cell on every storey, so the door planner
        // can always carve a doorway from a room into the
        // staircase/elevator.
        let row_cell_w = bounds.width() / row_cells as f32;
        for c in 0..row_cells {
            let x0 = bounds.x_min + c as f32 * row_cell_w;
            let z0 = bounds.z_min + r as f32 * cell_d;
            cells.push(RoomCell {
                rect: Rect2 {
                    x_min: x0,
                    x_max: x0 + row_cell_w,
                    z_min: z0,
                    z_max: z0 + cell_d,
                },
                room_type_index: assigned_types[idx],
                kind: CellKind::Room,
                door_slots: Vec::new(),
            });
            idx += 1;
            if idx >= rooms {
                break 'rows;
            }
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_tiles_passed_bounds() {
        let bounds = Rect2 { x_min: -5.0, x_max: 5.0, z_min: -2.0, z_max: 2.0 };
        let mut s = 1u32;
        let cells = layout(bounds, &[0, 0, 0, 0], &mut s);
        for c in &cells {
            assert!(c.rect.x_min >= bounds.x_min - 1e-3);
            assert!(c.rect.x_max <= bounds.x_max + 1e-3);
            assert!(c.rect.z_min >= bounds.z_min - 1e-3);
            assert!(c.rect.z_max <= bounds.z_max + 1e-3);
            assert!(matches!(c.kind, CellKind::Room));
        }
    }
}
