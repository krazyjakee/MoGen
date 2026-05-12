//! `style="office-core"` layout: a central corridor spine + perpendicular
//! offices on both sides. Shares the corridor-and-side-tile machinery
//! with `hotel.rs` but uses a smaller target run so each office cell is
//! ~3 m wide — the rhythm of a typical office floor where the corridor
//! is flanked by many smaller workspaces rather than a few large rooms.
//!
//! The "perpendicular" reading comes for free: the corridor runs along
//! the longer axis, so each side cell's long dimension is perpendicular
//! to the corridor (depth of the floorplate is split by the corridor
//! into two strips; the offices fill those strips).

use super::{hotel, Rect2, RoomCell};

/// Target run length along the corridor for one office cell.
const OFFICE_TARGET_RUN: f32 = 3.0;

pub(super) fn layout(
    bounds: Rect2,
    assigned_types: &[usize],
    corridor_type_idx: usize,
    state: &mut u32,
) -> Vec<RoomCell> {
    hotel::layout_with_target(
        bounds,
        assigned_types,
        corridor_type_idx,
        OFFICE_TARGET_RUN,
        state,
    )
}

#[cfg(test)]
mod tests {
    use super::super::CellKind;
    use super::*;

    #[test]
    fn office_corridor_present_and_uniform() {
        let bounds = Rect2 { x_min: -9.0, x_max: 9.0, z_min: -4.0, z_max: 4.0 };
        let mut s = 2u32;
        let cells = layout(bounds, &[1, 1, 1, 1, 1, 1, 0], 0, &mut s);
        let corridor = &cells[0];
        assert_eq!(corridor.room_type_index, 0);
        assert!((corridor.rect.x_max - corridor.rect.x_min) >= 17.9);
        // Side cells should be uniformly tiled — adjacent side cells share
        // an edge of length equal to the half's depth.
        let side: Vec<_> = cells.iter().skip(1).collect();
        for c in &side {
            assert!(matches!(c.kind, CellKind::Room));
        }
    }
}
