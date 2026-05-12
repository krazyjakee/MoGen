//! Helpers shared across the per-style layout backends.
//!
//! Kept deliberately small — anything style-specific lives in the
//! style module so each backend stays self-contained. The helpers here
//! are the genuine cross-cutting operations: grid-shape picking, room-type
//! index cycling, type-list filtering, and the corridor split that both
//! `corridor.rs` and `hotel.rs` use.

use super::Rect2;

/// Pick a `(cols, rows)` grid shape whose capacity is `>= rooms` and whose
/// aspect ratio (`cols / rows`) is closest to √2. The plate itself is
/// sized to √2 in [`super::floor_dims`], so matching that aspect keeps
/// cells roughly square in plan view.
///
/// Always returns at least `(1, 1)`. Capacity may exceed `rooms`; callers
/// that don't want empty cells collapse the last row themselves (see
/// [`super::grid::layout`] and [`super::organic::layout`]).
pub(super) fn pick_aspect_grid(rooms: usize) -> (usize, usize) {
    let rooms = rooms.max(1);
    let target_aspect = std::f32::consts::SQRT_2;
    // Search every (cols, rows) with cols * rows >= rooms up to a sensible
    // bound; pick the one closest to √2 aspect, breaking ties on smallest
    // total capacity (least waste) and then on smallest `cols`.
    let max_side = (rooms as f32).sqrt().ceil() as usize + 2;
    let mut best: Option<(f32, usize, usize, usize)> = None;
    for cols in 1..=max_side {
        for rows in 1..=max_side {
            if cols * rows < rooms {
                continue;
            }
            let aspect = cols as f32 / rows as f32;
            let err = (aspect - target_aspect).abs();
            let waste = cols * rows;
            let candidate = (err, waste, cols, rows);
            match best {
                None => best = Some(candidate),
                Some((be, bw, bc, _)) => {
                    if err < be
                        || (err == be && waste < bw)
                        || (err == be && waste == bw && cols < bc)
                    {
                        best = Some(candidate);
                    }
                }
            }
        }
    }
    let (_, _, cols, rows) = best.unwrap_or((0.0, 1, 1, 1));
    (cols, rows)
}

/// Cycle through `types` by index, returning a usable type for the `i`th
/// cell even when the layout emits more cells than `assigned_types.len()`.
/// Falls back to `0` for empty input — only reachable when the caller
/// already short-circuited the zero-room case, but the fallback keeps the
/// helper total.
pub(super) fn type_at(i: usize, types: &[usize]) -> usize {
    if types.is_empty() {
        0
    } else {
        types[i % types.len()]
    }
}

/// Return a copy of `types` with every occurrence of `excluded` removed.
/// Used to strip the corridor room-type out of the side-room list before
/// the per-side tilers run (the corridor is emitted as its own cell, not
/// by the BSP/grid backends).
pub(super) fn filter_types_excluding(types: &[usize], excluded: usize) -> Vec<usize> {
    types.iter().copied().filter(|t| *t != excluded).collect()
}

/// Result of [`split_with_corridor`]: the corridor strip plus the two
/// rectangles on either side of it, all sharing edges within the
/// `EDGE_EPS` tolerance used by [`Rect2::shared_edge_length`].
pub(super) struct CorridorSplit {
    pub corridor: Rect2,
    pub half_a: Rect2,
    pub half_b: Rect2,
}

/// Carve a centered corridor of width `corridor_width` along the longer
/// axis of `bounds`. The corridor's perpendicular extent is exactly
/// `corridor_width`; the two halves abut the corridor with no gap so the
/// downstream door planner can find a full shared edge.
///
/// `half_a` is the south/west half (smaller axis value) — the caller in
/// [`super::corridor::layout`] biases the slight majority of side rooms
/// to `half_a` to match the "rooms cluster near the entrance" reading.
pub(super) fn split_with_corridor(bounds: Rect2, corridor_width: f32) -> CorridorSplit {
    let along_x = bounds.width() >= bounds.depth();
    let half_w = 0.5 * corridor_width;
    if along_x {
        // Corridor runs along X; splits the Z extent in two.
        let z_mid = 0.5 * (bounds.z_min + bounds.z_max);
        let corridor = Rect2 {
            x_min: bounds.x_min,
            x_max: bounds.x_max,
            z_min: z_mid - half_w,
            z_max: z_mid + half_w,
        };
        let half_a = Rect2 {
            x_min: bounds.x_min,
            x_max: bounds.x_max,
            z_min: bounds.z_min,
            z_max: corridor.z_min,
        };
        let half_b = Rect2 {
            x_min: bounds.x_min,
            x_max: bounds.x_max,
            z_min: corridor.z_max,
            z_max: bounds.z_max,
        };
        CorridorSplit { corridor, half_a, half_b }
    } else {
        // Corridor runs along Z; splits the X extent in two.
        let x_mid = 0.5 * (bounds.x_min + bounds.x_max);
        let corridor = Rect2 {
            x_min: x_mid - half_w,
            x_max: x_mid + half_w,
            z_min: bounds.z_min,
            z_max: bounds.z_max,
        };
        let half_a = Rect2 {
            x_min: bounds.x_min,
            x_max: corridor.x_min,
            z_min: bounds.z_min,
            z_max: bounds.z_max,
        };
        let half_b = Rect2 {
            x_min: corridor.x_max,
            x_max: bounds.x_max,
            z_min: bounds.z_min,
            z_max: bounds.z_max,
        };
        CorridorSplit { corridor, half_a, half_b }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_aspect_grid_capacity_covers_rooms() {
        for n in 1..=32 {
            let (c, r) = pick_aspect_grid(n);
            assert!(c * r >= n, "n={n} got {c}×{r}");
        }
    }

    #[test]
    fn pick_aspect_grid_prefers_wide_shape() {
        // √2 aspect → cols >= rows for the natural choices.
        for &n in &[2usize, 3, 4, 6, 8, 9, 12] {
            let (c, r) = pick_aspect_grid(n);
            assert!(c >= r, "n={n} got {c}×{r}, expected cols >= rows");
        }
    }

    #[test]
    fn type_at_cycles_and_handles_empty() {
        assert_eq!(type_at(0, &[7, 9]), 7);
        assert_eq!(type_at(1, &[7, 9]), 9);
        assert_eq!(type_at(2, &[7, 9]), 7);
        assert_eq!(type_at(0, &[]), 0);
    }

    #[test]
    fn filter_types_excluding_removes_all_matches() {
        let out = filter_types_excluding(&[1, 0, 2, 0, 3], 0);
        assert_eq!(out, vec![1, 2, 3]);
    }

    #[test]
    fn split_with_corridor_along_longer_axis_x() {
        let b = Rect2 { x_min: -6.0, x_max: 6.0, z_min: -3.0, z_max: 3.0 };
        let s = split_with_corridor(b, 1.5);
        assert!((s.corridor.x_max - s.corridor.x_min - 12.0).abs() < 1e-4);
        assert!((s.corridor.z_max - s.corridor.z_min - 1.5).abs() < 1e-4);
        // Halves abut the corridor exactly.
        assert!((s.half_a.z_max - s.corridor.z_min).abs() < 1e-6);
        assert!((s.half_b.z_min - s.corridor.z_max).abs() < 1e-6);
        // half_a is south (smaller z).
        assert!(s.half_a.z_min < s.half_b.z_min);
    }

    #[test]
    fn split_with_corridor_along_longer_axis_z() {
        let b = Rect2 { x_min: -3.0, x_max: 3.0, z_min: -6.0, z_max: 6.0 };
        let s = split_with_corridor(b, 1.5);
        assert!((s.corridor.z_max - s.corridor.z_min - 12.0).abs() < 1e-4);
        assert!((s.corridor.x_max - s.corridor.x_min - 1.5).abs() < 1e-4);
        assert!((s.half_a.x_max - s.corridor.x_min).abs() < 1e-6);
        assert!((s.half_b.x_min - s.corridor.x_max).abs() < 1e-6);
    }
}
