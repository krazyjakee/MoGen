//! `style="maze"` layout: a recursive-backtracker spanning tree on an
//! integer grid that selects a single long axial corridor and emits
//! every other grid cell as an individual room. The corridor's axis
//! (horizontal vs. vertical) and exact row/column are driven by the
//! spanning tree's diameter — so different seeds choose different
//! corridor placements — but the corridor itself is always a clean
//! rectangle running the full length of one axis so the emit pass gets
//! axis-aligned input it can carve walls around.
//!
//! Reads as a maze in plan view: one long thin corridor threading many
//! small dead-end rooms.
//!
//! Deterministic by construction (only the `rng::step` LCG is used; the
//! search uses an explicit `Vec<u32>` stack — no `HashMap` iteration
//! order). Falls back to a uniform grid when the plate is too small.

use super::super::rng::step;
use super::{CellKind, Rect2, RoomCell};

const TARGET_CELL: f32 = 2.0;
const MIN_PLATE_EXTENT: f32 = 8.0;

pub(super) fn layout(
    bounds: Rect2,
    assigned_types: &[usize],
    state: &mut u32,
) -> Vec<RoomCell> {
    let rooms = assigned_types.len();
    if rooms == 0 {
        return Vec::new();
    }
    if bounds.width().min(bounds.depth()) < MIN_PLATE_EXTENT {
        return super::grid::layout(bounds, assigned_types, state);
    }

    let cols = ((bounds.width() / TARGET_CELL).round() as usize).max(3);
    let rows = ((bounds.depth() / TARGET_CELL).round() as usize).max(3);

    let parents = recursive_backtracker(cols, rows, state);
    let longest = diameter_path(cols, rows, &parents);

    // Decide corridor axis from the longest path: count how many cells
    // share each row vs. column index. The axis with the densest single
    // index wins; that row/column becomes the corridor.
    let mut row_counts = vec![0u32; rows];
    let mut col_counts = vec![0u32; cols];
    for &idx in &longest {
        row_counts[idx / cols] += 1;
        col_counts[idx % cols] += 1;
    }
    let (best_row, &best_row_count) = row_counts
        .iter()
        .enumerate()
        .max_by_key(|(_, &c)| c)
        .unwrap_or((0, &0));
    let (best_col, &best_col_count) = col_counts
        .iter()
        .enumerate()
        .max_by_key(|(_, &c)| c)
        .unwrap_or((0, &0));

    let cell_w = bounds.width() / cols as f32;
    let cell_d = bounds.depth() / rows as f32;
    let to_rect = |idx: usize| -> Rect2 {
        let r = idx / cols;
        let c = idx % cols;
        let x_min = bounds.x_min + c as f32 * cell_w;
        let z_min = bounds.z_min + r as f32 * cell_d;
        Rect2 {
            x_min,
            x_max: x_min + cell_w,
            z_min,
            z_max: z_min + cell_d,
        }
    };

    let horizontal_corridor = best_col_count <= best_row_count;
    let (corridor_rect, on_corridor): (Rect2, Box<dyn Fn(usize) -> bool>) = if horizontal_corridor {
        let z_min = bounds.z_min + best_row as f32 * cell_d;
        let rect = Rect2 {
            x_min: bounds.x_min,
            x_max: bounds.x_max,
            z_min,
            z_max: z_min + cell_d,
        };
        (rect, Box::new(move |idx: usize| idx / cols == best_row))
    } else {
        let x_min = bounds.x_min + best_col as f32 * cell_w;
        let rect = Rect2 {
            x_min,
            x_max: x_min + cell_w,
            z_min: bounds.z_min,
            z_max: bounds.z_max,
        };
        (rect, Box::new(move |idx: usize| idx % cols == best_col))
    };

    // Tile the entire floorplate: emit the corridor first, then every
    // remaining grid square as a leaf room. Room-type indices cycle
    // modulo `rooms` — the natural cell count of a maze grid usually
    // exceeds the requested room count, and leaving cells unemitted
    // would leave visible gaps inside the perimeter walls.
    let mut cells: Vec<RoomCell> = Vec::with_capacity(cols * rows);
    cells.push(RoomCell {
        rect: corridor_rect,
        room_type_index: assigned_types[0],
        kind: CellKind::Room,
    });

    let mut emitted = 1usize;
    for idx in 0..(cols * rows) {
        if on_corridor(idx) {
            continue;
        }
        let type_idx = assigned_types[emitted % rooms];
        emitted += 1;
        cells.push(RoomCell {
            rect: to_rect(idx),
            room_type_index: type_idx,
            kind: CellKind::Room,
        });
    }

    cells
}

/// Recursive backtracker spanning tree. Returns `parents[i] = Some(j)`
/// where `j` is the index of the cell visited just before `i`, or `None`
/// for the root.
fn recursive_backtracker(cols: usize, rows: usize, state: &mut u32) -> Vec<Option<usize>> {
    let n = cols * rows;
    let mut parents: Vec<Option<usize>> = vec![None; n];
    let mut visited = vec![false; n];
    let start = (step(state) as usize) % n;
    let mut stack: Vec<usize> = vec![start];
    visited[start] = true;
    while let Some(&here) = stack.last() {
        let r = here / cols;
        let c = here % cols;
        let mut candidates: Vec<usize> = Vec::with_capacity(4);
        if c + 1 < cols && !visited[here + 1] {
            candidates.push(here + 1);
        }
        if c > 0 && !visited[here - 1] {
            candidates.push(here - 1);
        }
        if r + 1 < rows && !visited[here + cols] {
            candidates.push(here + cols);
        }
        if r > 0 && !visited[here - cols] {
            candidates.push(here - cols);
        }
        if candidates.is_empty() {
            stack.pop();
            continue;
        }
        let pick = candidates[(step(state) as usize) % candidates.len()];
        visited[pick] = true;
        parents[pick] = Some(here);
        stack.push(pick);
    }
    parents
}

/// Diameter of the spanning tree: longest path between any two cells.
fn diameter_path(cols: usize, rows: usize, parents: &[Option<usize>]) -> Vec<usize> {
    let n = cols * rows;
    let adj = build_adj(cols, rows, parents);
    let (far_a, _) = bfs_farthest(0, &adj, n);
    let (far_b, prev) = bfs_farthest_with_prev(far_a, &adj, n);
    let mut path: Vec<usize> = Vec::new();
    let mut cur = Some(far_b);
    while let Some(c) = cur {
        path.push(c);
        if c == far_a {
            break;
        }
        cur = prev[c];
    }
    path.reverse();
    path
}

fn build_adj(cols: usize, rows: usize, parents: &[Option<usize>]) -> Vec<Vec<usize>> {
    let n = cols * rows;
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (child, parent) in parents.iter().enumerate() {
        if let Some(p) = parent {
            adj[child].push(*p);
            adj[*p].push(child);
        }
    }
    adj
}

fn bfs_farthest(src: usize, adj: &[Vec<usize>], n: usize) -> (usize, usize) {
    let mut dist: Vec<i32> = vec![-1; n];
    let mut queue: Vec<usize> = Vec::with_capacity(n);
    dist[src] = 0;
    queue.push(src);
    let mut head = 0usize;
    let mut far_node = src;
    let mut far_dist = 0i32;
    while head < queue.len() {
        let u = queue[head];
        head += 1;
        for &v in &adj[u] {
            if dist[v] < 0 {
                dist[v] = dist[u] + 1;
                if dist[v] > far_dist {
                    far_dist = dist[v];
                    far_node = v;
                }
                queue.push(v);
            }
        }
    }
    (far_node, far_dist.max(0) as usize)
}

fn bfs_farthest_with_prev(
    src: usize,
    adj: &[Vec<usize>],
    n: usize,
) -> (usize, Vec<Option<usize>>) {
    let mut dist: Vec<i32> = vec![-1; n];
    let mut prev: Vec<Option<usize>> = vec![None; n];
    let mut queue: Vec<usize> = Vec::with_capacity(n);
    dist[src] = 0;
    queue.push(src);
    let mut head = 0usize;
    let mut far_node = src;
    let mut far_dist = 0i32;
    while head < queue.len() {
        let u = queue[head];
        head += 1;
        for &v in &adj[u] {
            if dist[v] < 0 {
                dist[v] = dist[u] + 1;
                prev[v] = Some(u);
                if dist[v] > far_dist {
                    far_dist = dist[v];
                    far_node = v;
                }
                queue.push(v);
            }
        }
    }
    (far_node, prev)
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
    fn maze_falls_back_on_tiny_plate() {
        let mut s = 1u32;
        let cells = layout(bounds(5.0, 5.0), &[0, 0, 0], &mut s);
        assert!(cells.len() >= 3);
    }

    #[test]
    fn maze_emits_corridor_plus_rooms() {
        let mut s = 12345u32;
        let cells = layout(bounds(16.0, 12.0), &[0; 6], &mut s);
        assert!(
            cells.len() >= 2,
            "expected corridor + at least one room, got {}",
            cells.len()
        );
        let corridor = &cells[0];
        // Corridor is the first cell and must span the full extent of one
        // axis (the corridor's longer dimension equals the plate's longer
        // dimension on that axis).
        let b = bounds(16.0, 12.0);
        let spans_x = (corridor.rect.width() - b.width()).abs() < 0.1;
        let spans_z = (corridor.rect.depth() - b.depth()).abs() < 0.1;
        assert!(spans_x || spans_z, "corridor should span one full axis");
    }

    #[test]
    fn maze_tiles_full_floorplate() {
        let mut s = 12345u32;
        let b = bounds(16.0, 12.0);
        let cells = layout(b, &[0; 6], &mut s);
        let sum: f32 = cells.iter().map(|c| c.rect.width() * c.rect.depth()).sum();
        let plate = b.width() * b.depth();
        assert!(
            (sum - plate).abs() < 0.05 * plate,
            "maze cells {sum:.2} m² leave a gap inside the floorplate {plate:.2} m²"
        );
    }

    #[test]
    fn maze_is_deterministic_under_same_seed() {
        let b = bounds(16.0, 12.0);
        let mut s1 = 99u32;
        let mut s2 = 99u32;
        let a = layout(b, &[0; 6], &mut s1);
        let bb = layout(b, &[0; 6], &mut s2);
        assert_eq!(a.len(), bb.len());
        for (ca, cb) in a.iter().zip(bb.iter()) {
            assert!((ca.rect.x_min - cb.rect.x_min).abs() < 1e-6);
            assert!((ca.rect.z_min - cb.rect.z_min).abs() < 1e-6);
        }
    }
}
