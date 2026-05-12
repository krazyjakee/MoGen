//! `style="radial"` layout: concentric rectangular rings around a central
//! cell. The downstream emit pipeline only accepts axis-aligned cells, so
//! a literal polar subdivision would require non-trivial reprojection. We
//! approximate "radial" with concentric rectangular bands — the result
//! reads as a rotunda from above and produces the same visual hierarchy
//! (small inner room, larger outer perimeter rooms) without breaking the
//! axis-aligned invariant.
//!
//! Ring counts come from the number of sampled rooms. Falls back to a
//! plain grid when the floorplate is too small to support the outer ring
//! at a sensible room width.

use super::common::type_at;
use super::{CellKind, Rect2, RoomCell};

/// Below this short-side extent (m) the rings collapse into rooms thinner
/// than a real architectural corridor; fall back to `grid` instead.
const MIN_PLATE_EXTENT: f32 = 6.0;

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

    // `rings_outside_centre` = number of concentric bands around the central
    // cell. Each ring contributes 4 cells (N/E/S/W strips). The central
    // cell counts as 1. So total = 1 + 4 * rings.
    //
    // We pick the smallest `rings` whose natural capacity (1 + 4*rings)
    // covers `rooms`, then ALWAYS emit every cell in every ring so the
    // floorplate is fully tiled. When the natural capacity exceeds
    // `rooms`, room-type indices cycle modulo `rooms` — a couple of
    // adjacent cells share a kind, which still reads naturally on a
    // rotunda plan.
    let rings = ((rooms.saturating_sub(1) as f32) / 4.0).ceil().max(1.0) as usize;
    let bands = rings + 1; // includes the centre band

    // Equal-width radial bands along each axis. The centre cell takes one
    // band; each outer ring takes one band on each side, so the total span
    // is `2 * rings + 1` band-widths.
    let band_w = bounds.width() / (2.0 * rings as f32 + 1.0);
    let band_d = bounds.depth() / (2.0 * rings as f32 + 1.0);

    let mut cells: Vec<RoomCell> = Vec::with_capacity(1 + 4 * rings);
    let mut emitted = 0usize;
    let push = |rect: Rect2, cells: &mut Vec<RoomCell>, emitted: &mut usize| {
        cells.push(RoomCell {
            rect,
            room_type_index: type_at(*emitted, assigned_types),
            kind: CellKind::Room,
        });
        *emitted += 1;
    };

    // Centre cell, then expand outward ring by ring (S/N/W/E per ring).
    for ring in 0..bands {
        if ring == 0 {
            let half_w = 0.5 * band_w;
            let half_d = 0.5 * band_d;
            let cx = 0.5 * (bounds.x_min + bounds.x_max);
            let cz = 0.5 * (bounds.z_min + bounds.z_max);
            push(
                Rect2 {
                    x_min: cx - half_w,
                    x_max: cx + half_w,
                    z_min: cz - half_d,
                    z_max: cz + half_d,
                },
                &mut cells,
                &mut emitted,
            );
            continue;
        }
        // Outer ring `ring` (1-indexed) spans from the inner edge of the
        // previous band to the outer edge of this band. The inner rect is
        // the previously-placed cells' axis-aligned bounding box.
        let inner_x_min = bounds.x_min + (rings as f32 - (ring as f32 - 1.0)) * band_w;
        let inner_x_max = bounds.x_max - (rings as f32 - (ring as f32 - 1.0)) * band_w;
        let inner_z_min = bounds.z_min + (rings as f32 - (ring as f32 - 1.0)) * band_d;
        let inner_z_max = bounds.z_max - (rings as f32 - (ring as f32 - 1.0)) * band_d;
        let outer_x_min = bounds.x_min + (rings as f32 - ring as f32) * band_w;
        let outer_x_max = bounds.x_max - (rings as f32 - ring as f32) * band_w;
        let outer_z_min = bounds.z_min + (rings as f32 - ring as f32) * band_d;
        let outer_z_max = bounds.z_max - (rings as f32 - ring as f32) * band_d;

        // South strip (along -Z, spans the full outer X width).
        push(
            Rect2 {
                x_min: outer_x_min,
                x_max: outer_x_max,
                z_min: outer_z_min,
                z_max: inner_z_min,
            },
            &mut cells,
            &mut emitted,
        );
        // North strip (along +Z, full outer X width).
        push(
            Rect2 {
                x_min: outer_x_min,
                x_max: outer_x_max,
                z_min: inner_z_max,
                z_max: outer_z_max,
            },
            &mut cells,
            &mut emitted,
        );
        // West strip (along -X, between the two latitudinal strips so it
        // doesn't overlap them).
        push(
            Rect2 {
                x_min: outer_x_min,
                x_max: inner_x_min,
                z_min: inner_z_min,
                z_max: inner_z_max,
            },
            &mut cells,
            &mut emitted,
        );
        // East strip (along +X).
        push(
            Rect2 {
                x_min: inner_x_max,
                x_max: outer_x_max,
                z_min: inner_z_min,
                z_max: inner_z_max,
            },
            &mut cells,
            &mut emitted,
        );
    }
    cells
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
    fn radial_falls_back_to_grid_on_small_plates() {
        let mut s = 1u32;
        let cells = layout(bounds(4.0, 4.0), &[0, 0, 0, 0], &mut s);
        // Same length as a grid layout (4 cells) — confirms fallthrough.
        assert_eq!(cells.len(), 4);
    }

    #[test]
    fn radial_emits_centre_plus_ring() {
        let mut s = 1u32;
        let cells = layout(bounds(15.0, 15.0), &[0; 5], &mut s);
        // 1 centre + up to 4 ring cells = 5.
        assert_eq!(cells.len(), 5);
        // First cell is the central one — its rect is contained in every
        // outer ring cell's bounding box.
        let centre = &cells[0].rect;
        let cx = 0.5 * (centre.x_min + centre.x_max);
        let cz = 0.5 * (centre.z_min + centre.z_max);
        assert!(cx.abs() < 1e-3 && cz.abs() < 1e-3);
    }

    #[test]
    fn radial_tiles_full_floorplate_when_room_count_is_off_a_ring() {
        // 7 rooms = 1 centre + 2 rings (capacity 1 + 4*2 = 9). Every cell
        // must still be emitted so the floorplate is tiled — extra cells
        // beyond `rooms` cycle through the room-type indices.
        let mut s = 7u32;
        let b = bounds(18.0, 18.0);
        let cells = layout(b, &[0; 7], &mut s);
        assert_eq!(cells.len(), 9, "expected full 1+4*2 cells, got {}", cells.len());
        let sum: f32 = cells.iter().map(|c| c.rect.width() * c.rect.depth()).sum();
        let plate = b.width() * b.depth();
        assert!(
            (sum - plate).abs() < 0.05 * plate,
            "radial cells {sum:.2} m² leave a gap inside the floorplate {plate:.2} m²"
        );
    }

    #[test]
    fn radial_cells_are_inside_bounds_and_non_overlapping() {
        let mut s = 1u32;
        let b = bounds(18.0, 18.0);
        let cells = layout(b, &[0; 9], &mut s);
        for c in &cells {
            assert!(c.rect.x_min >= b.x_min - 1e-3);
            assert!(c.rect.x_max <= b.x_max + 1e-3);
            assert!(c.rect.z_min >= b.z_min - 1e-3);
            assert!(c.rect.z_max <= b.z_max + 1e-3);
        }
        for i in 0..cells.len() {
            for j in (i + 1)..cells.len() {
                let a = cells[i].rect;
                let b = cells[j].rect;
                let overlap_x = a.x_min.max(b.x_min) < a.x_max.min(b.x_max) - 1e-3;
                let overlap_z = a.z_min.max(b.z_min) < a.z_max.min(b.z_max) - 1e-3;
                assert!(
                    !(overlap_x && overlap_z),
                    "radial cells {i} and {j} overlap: {a:?} vs {b:?}"
                );
            }
        }
    }
}
