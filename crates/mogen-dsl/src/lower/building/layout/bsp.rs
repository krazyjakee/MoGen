//! `style="apartment-block"` layout: recursive binary-space-partition over
//! the room-layout rectangle (which excludes the circulation column on
//! multi-storey buildings).

use super::super::rng::rand_f01;
use super::common::type_at;
use super::{CellKind, Rect2, RoomCell};

const MIN_CELL_EXTENT: f32 = 1.6;
const MAX_ASPECT: f32 = 2.6;

pub(super) fn layout(
    bounds: Rect2,
    assigned_types: &[usize],
    state: &mut u32,
) -> Vec<RoomCell> {
    let target_rooms = assigned_types.len().max(1);
    let mut leaves: Vec<Rect2> = vec![bounds];
    while leaves.len() < target_rooms {
        let Some(idx) = pick_splittable(&leaves) else {
            break;
        };
        let rect = leaves[idx];
        if let Some((a, b)) = split(&rect, state) {
            leaves[idx] = a;
            leaves.push(b);
        } else {
            break;
        }
    }
    let mut cells: Vec<RoomCell> = Vec::with_capacity(leaves.len());
    for (i, rect) in leaves.into_iter().enumerate() {
        cells.push(RoomCell {
            rect,
            room_type_index: type_at(i, assigned_types),
            kind: CellKind::Room,
            door_slots: Vec::new(),
        });
    }
    cells
}

fn pick_splittable(leaves: &[Rect2]) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for (i, r) in leaves.iter().enumerate() {
        let long = r.width().max(r.depth());
        if long < 2.0 * MIN_CELL_EXTENT {
            continue;
        }
        let score = r.area();
        match best {
            None => best = Some((score, i)),
            Some((bs, _)) if score > bs => best = Some((score, i)),
            _ => {}
        }
    }
    best.map(|(_, i)| i)
}

fn split(rect: &Rect2, state: &mut u32) -> Option<(Rect2, Rect2)> {
    let along_x = rect.width() >= rect.depth();
    let extent = if along_x { rect.width() } else { rect.depth() };
    if extent < 2.0 * MIN_CELL_EXTENT {
        return None;
    }
    let mut t = 0.3 + rand_f01(state) * 0.4;
    let split_pos = if along_x {
        rect.x_min + t * extent
    } else {
        rect.z_min + t * extent
    };
    let mut a;
    let mut b;
    if along_x {
        a = *rect;
        a.x_max = split_pos;
        b = *rect;
        b.x_min = split_pos;
    } else {
        a = *rect;
        a.z_max = split_pos;
        b = *rect;
        b.z_min = split_pos;
    }
    if !cell_ok(&a) || !cell_ok(&b) {
        t = 0.5;
        let split_pos = if along_x {
            rect.x_min + t * extent
        } else {
            rect.z_min + t * extent
        };
        if along_x {
            a = *rect;
            a.x_max = split_pos;
            b = *rect;
            b.x_min = split_pos;
        } else {
            a = *rect;
            a.z_max = split_pos;
            b = *rect;
            b.z_min = split_pos;
        }
        if !cell_ok(&a) || !cell_ok(&b) {
            return None;
        }
    }
    Some((a, b))
}

fn cell_ok(r: &Rect2) -> bool {
    if r.width() < MIN_CELL_EXTENT || r.depth() < MIN_CELL_EXTENT {
        return false;
    }
    let aspect = r.width().max(r.depth()) / r.width().min(r.depth());
    aspect <= MAX_ASPECT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bsp_produces_at_least_one_cell() {
        let bounds = Rect2 { x_min: -5.0, x_max: 5.0, z_min: -5.0, z_max: 5.0 };
        let mut s = 12345u32;
        let cells = layout(bounds, &[0, 0, 0, 0], &mut s);
        assert!(!cells.is_empty());
    }

    #[test]
    fn bsp_cells_do_not_overlap() {
        let bounds = Rect2 { x_min: -6.0, x_max: 6.0, z_min: -5.0, z_max: 5.0 };
        let mut s = 77u32;
        let cells = layout(bounds, &[0; 6], &mut s);
        for (i, a) in cells.iter().enumerate() {
            for b in cells.iter().skip(i + 1) {
                let overlap = !(a.rect.x_max <= b.rect.x_min + 1e-4
                    || b.rect.x_max <= a.rect.x_min + 1e-4
                    || a.rect.z_max <= b.rect.z_min + 1e-4
                    || b.rect.z_max <= a.rect.z_min + 1e-4);
                assert!(!overlap, "rooms overlap: {:?} {:?}", a.rect, b.rect);
            }
        }
    }
}
