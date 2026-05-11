//! `style="grid"` layout: uniform grid subdivision sized to the room count.
//!
//! Picks `cols × rows` such that `cols * rows >= rooms` (closest to a √2
//! aspect) and tiles the room-layout rectangle into equal cells. Each tile
//! is assigned a room type from the sampled distribution.
//!
//! Operates on whatever rectangle the caller passes — the floorplate minus
//! the circulation column, if any — so multi-storey buildings with a
//! reserved stair column place rooms only in the room area.

use super::{CellKind, Rect2, RoomCell};

pub(super) fn layout(
    bounds: Rect2,
    assigned_types: &[usize],
    _state: &mut u32,
) -> Vec<RoomCell> {
    let rooms = assigned_types.len().max(1);
    let (cols, rows) = pick_grid(rooms);

    let cell_w = bounds.width() / cols as f32;
    let cell_d = bounds.depth() / rows as f32;
    let mut cells: Vec<RoomCell> = Vec::with_capacity(rooms);
    let mut idx = 0usize;
    for r in 0..rows {
        for c in 0..cols {
            if idx >= rooms {
                break;
            }
            let x0 = bounds.x_min + c as f32 * cell_w;
            let z0 = bounds.z_min + r as f32 * cell_d;
            cells.push(RoomCell {
                rect: Rect2 {
                    x_min: x0,
                    x_max: x0 + cell_w,
                    z_min: z0,
                    z_max: z0 + cell_d,
                },
                room_type_index: assigned_types[idx],
                kind: CellKind::Room,
            });
            idx += 1;
        }
    }
    cells
}

/// Pick `(cols, rows)` such that `cols * rows >= rooms` and the aspect is
/// as close to √2 as possible. Walks candidates around √rooms outward.
fn pick_grid(rooms: usize) -> (usize, usize) {
    if rooms <= 1 {
        return (1, 1);
    }
    let root = (rooms as f32).sqrt().round() as usize;
    let root = root.max(1);
    let target = std::f32::consts::SQRT_2;
    let mut best: Option<(f32, usize, usize)> = None;
    for c in root.saturating_sub(2).max(1)..=(root + 2) {
        for r in root.saturating_sub(2).max(1)..=(root + 2) {
            if c * r < rooms {
                continue;
            }
            let aspect = c as f32 / r as f32;
            let aspect_err = (aspect - target).abs();
            let waste = ((c * r) - rooms) as f32 * 0.05;
            let cost = aspect_err + waste;
            match best {
                None => best = Some((cost, c, r)),
                Some((bc, _, _)) if cost < bc => best = Some((cost, c, r)),
                _ => {}
            }
        }
    }
    let (_, c, r) = best.unwrap_or((0.0, root, root));
    (c, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_grid_4_covers_at_least_four_cells() {
        let (c, r) = pick_grid(4);
        assert!(c * r >= 4, "grid {c}x{r} does not cover 4 rooms");
        assert!(c >= r, "expected width ≥ depth, got {c}x{r}");
    }

    #[test]
    fn pick_grid_6_returns_3x2() {
        let (c, r) = pick_grid(6);
        assert_eq!(c * r, 6);
        assert!(c >= r);
    }

    #[test]
    fn pick_grid_handles_one() {
        assert_eq!(pick_grid(1), (1, 1));
    }

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
