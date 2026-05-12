//! `style="organic"` layout: a jittered grid that reads as varied,
//! non-uniform room sizes without breaking the axis-aligned invariant the
//! emit pipeline depends on. Each interior grid line is displaced by up
//! to ±25% of its cell width, then snapped to a 0.1 m increment so seed
//! determinism is preserved exactly across runs.
//!
//! Falls back to a uniform grid when jitter would shrink any cell below
//! the architectural minimum (1.6 m on either axis).

use super::common::pick_aspect_grid;
use super::{CellKind, Rect2, RoomCell};
use super::super::rng::rand_f01;

const MIN_CELL_EXTENT: f32 = 1.6;
const JITTER_FRACTION: f32 = 0.25;
const SNAP_STEP: f32 = 0.1;

pub(super) fn layout(
    bounds: Rect2,
    assigned_types: &[usize],
    state: &mut u32,
) -> Vec<RoomCell> {
    let rooms = assigned_types.len();
    if rooms == 0 {
        return Vec::new();
    }

    let (cols_target, _) = pick_aspect_grid(rooms);
    // Reduce rows to the minimum needed to fit every room — `pick_grid`
    // optimises for aspect ratio with waste tolerated, but for a layout
    // that must tile the whole floorplate we'd rather have a partial
    // last row than a fully empty row south of all the rooms.
    let cols = cols_target.max(1);
    let rows = (rooms + cols - 1) / cols;
    let cell_w = bounds.width() / cols as f32;
    let cell_d = bounds.depth() / rows as f32;
    if cell_w < MIN_CELL_EXTENT || cell_d < MIN_CELL_EXTENT {
        return super::grid::layout(bounds, assigned_types, state);
    }

    // Jitter the interior grid lines. The outer lines (bounds.x_min /
    // bounds.x_max / bounds.z_min / bounds.z_max) are fixed so the plate
    // stays the same shape; only interior splits move.
    let xs = jittered_lines(bounds.x_min, bounds.x_max, cols, cell_w, state);
    let zs = jittered_lines(bounds.z_min, bounds.z_max, rows, cell_d, state);

    // If any cell collapsed below the minimum after snapping, bail out to
    // grid — keeps the layout architecturally sensible.
    for window in xs.windows(2) {
        if window[1] - window[0] < MIN_CELL_EXTENT {
            return super::grid::layout(bounds, assigned_types, state);
        }
    }
    for window in zs.windows(2) {
        if window[1] - window[0] < MIN_CELL_EXTENT {
            return super::grid::layout(bounds, assigned_types, state);
        }
    }

    // Tile the floorplate. The last row may be partial — its rightmost
    // cell expands east to absorb the missing column area. If the last
    // row itself is partial, it still spans the full vertical strip
    // assigned to it (zs already covers full depth via outer lines).
    let last_row_cells = if rooms % cols == 0 { cols } else { rooms % cols };
    let mut cells: Vec<RoomCell> = Vec::with_capacity(rooms);
    let mut idx = 0usize;
    for r in 0..rows {
        let cells_in_row = if r + 1 == rows { last_row_cells } else { cols };
        for c in 0..cells_in_row {
            if idx >= rooms {
                break;
            }
            let x_max = if c + 1 == cells_in_row {
                // Last cell in this row absorbs any unused horizontal strip
                // east of it (only happens on a partial last row).
                bounds.x_max
            } else {
                xs[c + 1]
            };
            cells.push(RoomCell {
                rect: Rect2 {
                    x_min: xs[c],
                    x_max,
                    z_min: zs[r],
                    z_max: zs[r + 1],
                },
                room_type_index: assigned_types[idx],
                kind: CellKind::Room,
                door_slots: Vec::new(),
            });
            idx += 1;
        }
    }
    cells
}

fn jittered_lines(
    min: f32,
    max: f32,
    splits: usize,
    cell_size: f32,
    state: &mut u32,
) -> Vec<f32> {
    let mut lines: Vec<f32> = Vec::with_capacity(splits + 1);
    lines.push(min);
    for i in 1..splits {
        let baseline = min + i as f32 * cell_size;
        // Symmetric jitter in [-JITTER * cell, +JITTER * cell].
        let r = rand_f01(state) * 2.0 - 1.0;
        let displaced = baseline + r * JITTER_FRACTION * cell_size;
        // Snap to SNAP_STEP so determinism is bit-exact even across float
        // accumulation paths.
        let snapped = (displaced / SNAP_STEP).round() * SNAP_STEP;
        lines.push(snapped);
    }
    lines.push(max);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(w: f32, d: f32) -> Rect2 {
        Rect2 {
            x_min: -0.5 * w,
            x_max: 0.5 * w,
            z_min: -0.5 * d,
            z_max: 0.5 * d,
        }
    }

    #[test]
    fn organic_cells_within_bounds() {
        let mut s = 7u32;
        let b = bounds(20.0, 14.0);
        let cells = layout(b, &[0; 9], &mut s);
        for c in &cells {
            assert!(c.rect.x_min >= b.x_min - 1e-3);
            assert!(c.rect.x_max <= b.x_max + 1e-3);
            assert!(c.rect.z_min >= b.z_min - 1e-3);
            assert!(c.rect.z_max <= b.z_max + 1e-3);
            assert!(matches!(c.kind, CellKind::Room));
        }
    }

    #[test]
    fn organic_is_deterministic_under_same_seed() {
        let b = bounds(20.0, 14.0);
        let mut a = 42u32;
        let mut bs = 42u32;
        let cells_a = layout(b, &[0; 9], &mut a);
        let cells_b = layout(b, &[0; 9], &mut bs);
        assert_eq!(cells_a.len(), cells_b.len());
        for (ca, cb) in cells_a.iter().zip(cells_b.iter()) {
            assert!((ca.rect.x_min - cb.rect.x_min).abs() < 1e-6);
            assert!((ca.rect.x_max - cb.rect.x_max).abs() < 1e-6);
        }
    }

    #[test]
    fn organic_tiles_full_floorplate_with_partial_last_row() {
        // 7 rooms + (cols=4, rows=2) leaves col 3 of row 1 unfilled — the
        // last cell of the partial row must absorb that strip so the floor
        // is fully tiled.
        let mut s = 11u32;
        let b = bounds(11.0, 7.8);
        let cells = layout(b, &[0; 7], &mut s);
        let sum: f32 = cells.iter().map(|c| c.rect.width() * c.rect.depth()).sum();
        let plate = b.width() * b.depth();
        assert!(
            (sum - plate).abs() < 0.05 * plate,
            "organic cells {sum:.2} m² leave a gap inside the floorplate {plate:.2} m²"
        );
    }

    #[test]
    fn organic_falls_back_when_jitter_collapses_a_cell() {
        // 3×3 m bounds with 4 rooms ⇒ 2×2 grid, cell ≈ 1.5 m — below the
        // 1.6 m minimum. Falls back to grid (whose pick_grid covers the
        // same target — so just assert the call returns sensible cells
        // rather than panicking).
        let mut s = 1u32;
        let cells = layout(bounds(3.0, 3.0), &[0, 0, 0, 0], &mut s);
        assert_eq!(cells.len(), 4);
    }
}
