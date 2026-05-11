//! `style="apartment-block"` layout: recursive binary-space-partition.
//!
//! Start from the floorplate rectangle. At each step, pick the longer axis
//! and split it at a random position that respects a minimum cell size and
//! a max aspect ratio cap. Stop when we have `rooms` leaves or no further
//! split is viable.

use super::{Floorplate, Rect2, RoomCell};
use super::super::config::BuildingCfg;
use super::super::rng::rand_f01;
use super::floor_dims;

/// Minimum room cell extent (m) on either axis. Keeps the BSP from yielding
/// hallway-sized slivers when `rooms` is large.
const MIN_CELL_EXTENT: f32 = 1.6;
/// Max aspect ratio cap (long/short). Splits that would push the larger
/// child past this cap are biased toward the centre.
const MAX_ASPECT: f32 = 2.6;

pub(super) fn layout(
    cfg: &BuildingCfg,
    assigned_types: &[usize],
    state: &mut u32,
) -> Floorplate {
    let target_rooms = assigned_types.len().max(1);
    let (w, d) = floor_dims(cfg.floor_area);
    let bounds = Rect2 {
        x_min: -0.5 * w,
        x_max: 0.5 * w,
        z_min: -0.5 * d,
        z_max: 0.5 * d,
    };

    let mut leaves: Vec<Rect2> = vec![bounds];
    // Repeatedly split the most-splittable leaf until target_rooms is
    // reached or no leaf can be split.
    while leaves.len() < target_rooms {
        let Some(idx) = pick_splittable(&leaves) else {
            break;
        };
        let rect = leaves[idx];
        if let Some((a, b)) = split(&rect, state) {
            leaves[idx] = a;
            leaves.push(b);
        } else {
            // Splitting failed even though it looked splittable — bail to
            // avoid an infinite loop. The scoring pass will rank a smaller
            // layout lower so other attempts may still win.
            break;
        }
    }

    // Pair each leaf with an assigned type. Truncate or repeat as needed.
    let mut cells: Vec<RoomCell> = Vec::with_capacity(leaves.len());
    for (i, rect) in leaves.into_iter().enumerate() {
        let room_type_index = if assigned_types.is_empty() {
            0
        } else {
            assigned_types[i % assigned_types.len()]
        };
        cells.push(RoomCell {
            rect,
            room_type_index,
        });
    }
    Floorplate { bounds, rooms: cells }
}

/// Index of the leaf with the largest minimum-cell margin (i.e. the one most
/// comfortably splittable). Returns None if no leaf can be split further.
fn pick_splittable(leaves: &[Rect2]) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for (i, r) in leaves.iter().enumerate() {
        let long = r.width().max(r.depth());
        if long < 2.0 * MIN_CELL_EXTENT {
            continue;
        }
        // Prefer splitting the largest cell first — keeps the layout from
        // degenerating into many thin slivers off a single big leaf.
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
    // t ∈ [0.3, 0.7] biased toward centre; respect both min-cell and the
    // aspect cap on each side.
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
        // Nudge the split to the centre as a fallback.
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
    use crate::lower::building::config::{BuildingCfg, RoomKind, RoomType, Roof, Style, WindowModules};

    fn mock_cfg(rooms: u32, area: f32) -> BuildingCfg {
        BuildingCfg {
            seed: 1,
            style: Style::ApartmentBlock,
            mat_style: String::new(),
            floor_area: area,
            rooms,
            floors_above: 1,
            floors_below: 0,
            windows: 0,
            skylights: 0,
            roof: Roof::Flat,
            ceiling_height: 2.6,
            door_w: 0.9,
            door_h: 2.1,
            window_w: 1.2,
            window_h: 1.4,
            wall_thickness: 0.12,
            ceiling_thickness: 0.2,
            entrances: 1,
            external_door: "door_simple".into(),
            internal_door: "door_simple".into(),
            windows_mod: WindowModules {
                small: "window_simple".into(),
                medium: "window_simple".into(),
                large: "window_simple".into(),
            },
            skylight_mod: "skylight_simple".into(),
            elevators: 0,
            staircases: 0,
            room_types: vec![RoomType {
                name: "any".into(),
                kind: RoomKind::Public,
                density: 1.0,
                mat: None,
                min_area: None,
                max_area: None,
            }],
            adjacencies: vec![],
        }
    }

    #[test]
    fn bsp_produces_at_least_one_cell() {
        let cfg = mock_cfg(4, 100.0);
        let mut s = 12345u32;
        let plate = layout(&cfg, &[0, 0, 0, 0], &mut s);
        assert!(!plate.rooms.is_empty());
    }

    #[test]
    fn bsp_cells_do_not_overlap() {
        let cfg = mock_cfg(6, 120.0);
        let mut s = 77u32;
        let plate = layout(&cfg, &[0; 6], &mut s);
        for (i, a) in plate.rooms.iter().enumerate() {
            for b in plate.rooms.iter().skip(i + 1) {
                let overlap = !(a.rect.x_max <= b.rect.x_min + 1e-4
                    || b.rect.x_max <= a.rect.x_min + 1e-4
                    || a.rect.z_max <= b.rect.z_min + 1e-4
                    || b.rect.z_max <= a.rect.z_min + 1e-4);
                assert!(!overlap, "rooms overlap: {:?} {:?}", a.rect, b.rect);
            }
        }
    }
}
