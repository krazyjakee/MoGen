//! `style="grid"` layout: uniform grid subdivision sized to the room count.
//!
//! Picks `cols × rows` such that `cols * rows == rooms` (or the closest
//! product that fits) and tiles the floorplate into equal rectangles. Each
//! tile is assigned a room type from the sampled distribution.
//!
//! No adjacency-aware swapping happens at this stage — the scoring pass
//! later picks the best of N attempts, and the random ordering of
//! `assigned_types` (already shuffled by `assign_room_types`) is what gives
//! the solver something to optimise over.

use super::{Floorplate, Rect2, RoomCell};
use super::super::config::BuildingCfg;
use super::floor_dims;

pub(super) fn layout(
    cfg: &BuildingCfg,
    assigned_types: &[usize],
    _state: &mut u32,
) -> Floorplate {
    let rooms = assigned_types.len().max(1);
    let (cols, rows) = pick_grid(rooms);
    let (w, d) = floor_dims(cfg.floor_area);
    let bounds = Rect2 {
        x_min: -0.5 * w,
        x_max: 0.5 * w,
        z_min: -0.5 * d,
        z_max: 0.5 * d,
    };

    let cell_w = w / cols as f32;
    let cell_d = d / rows as f32;
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
            });
            idx += 1;
        }
    }

    Floorplate { bounds, rooms: cells }
}

/// Pick `(cols, rows)` such that `cols * rows >= rooms` and the aspect is
/// as close to √2 as possible. Walks candidates around √rooms outward.
fn pick_grid(rooms: usize) -> (usize, usize) {
    if rooms <= 1 {
        return (1, 1);
    }
    let root = (rooms as f32).sqrt().round() as usize;
    let root = root.max(1);
    // Try (root-2 .. root+2) × (root-2 .. root+2), keep the smallest product
    // that covers `rooms` with the aspect closest to √2.
    let target = std::f32::consts::SQRT_2;
    let mut best: Option<(f32, usize, usize)> = None;
    for c in root.saturating_sub(2).max(1)..=(root + 2) {
        for r in root.saturating_sub(2).max(1)..=(root + 2) {
            if c * r < rooms {
                continue;
            }
            let aspect = c as f32 / r as f32;
            let aspect_err = (aspect - target).abs();
            // Penalty for excess cells beyond rooms (prefer tight grids).
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
        // Aspect prior favours √2: 3×2 (aspect 1.5) wins over the square 2×2.
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
}
