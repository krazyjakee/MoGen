//! Watertight wall mesh builder used by both perimeter (`shell.rs`) and
//! interior (`rooms.rs`) walls.
//!
//! Walls are built as a union of axis-aligned boxes carved around their
//! openings — no CSG, no boolean repair pass. A wall with N holes turns
//! into ≤ 1 pier + (left pier + lintel + sill + right pier) per hole. The
//! result is closed and convex-per-face by construction, so Manifold and
//! `clean_csg_output` never need to touch it. This eliminates the coplanar-
//! cutout-face pathologies (degenerate output, vertex welding artifacts)
//! that the CSG path produced for door openings whose bottom edge sat on
//! the wall's bottom face.
//!
//! Wall-local frame: long axis on local X, vertical on local Y, thickness
//! on local Z. A hole is `[along, cy, w, h]` — centre on local X, centre on
//! local Y, plus the opening's width and height. Holes that fall fully
//! outside the wall are dropped; X-overlapping holes are merged.

use glam::Vec3;

use mogen_core::{Mesh, UvMode};
use mogen_geom::{box_mesh, transform_mesh};

pub(super) fn wall_with_holes(size: [f32; 3], holes: &[[f32; 4]]) -> Mesh {
    let [length, height, thickness] = size;
    if length <= 0.0 || height <= 0.0 || thickness <= 0.0 {
        return Mesh::default();
    }
    if holes.is_empty() {
        return box_mesh(size, UvMode::Tile);
    }

    let half_x = 0.5 * length;
    let half_y = 0.5 * height;

    let mut spans: Vec<(f32, f32, f32, f32)> = Vec::new();
    for &[along, cy, w, h] in holes {
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        let x0 = (along - 0.5 * w).max(-half_x);
        let x1 = (along + 0.5 * w).min(half_x);
        if x1 - x0 < 0.02 {
            continue;
        }
        let y0 = (cy - 0.5 * h).max(-half_y);
        let y1 = (cy + 0.5 * h).min(half_y);
        if y1 - y0 < 0.02 {
            continue;
        }
        spans.push((x0, x1, y0, y1));
    }
    if spans.is_empty() {
        return box_mesh(size, UvMode::Tile);
    }
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // Group X-overlapping holes into "columns". Within a column the X-range
    // is shared; each entry contributes its own Y-range. Stacked holes
    // (same X, different Y — e.g. an elevator shaft's per-storey doorways)
    // must keep their lintels/sills between them, so we preserve every
    // Y-span instead of collapsing them into one.
    let mut columns: Vec<(f32, f32, Vec<(f32, f32)>)> = Vec::new();
    for (x0, x1, y0, y1) in spans {
        if let Some(last) = columns.last_mut() {
            if x0 <= last.1 + 1e-3 {
                last.1 = last.1.max(x1);
                last.2.push((y0, y1));
                continue;
            }
        }
        columns.push((x0, x1, vec![(y0, y1)]));
    }

    let mut acc = Mesh::default();
    let mut cursor = -half_x;
    for (x0, x1, mut ys) in columns {
        // Merge any Y-overlapping holes within the column (rare; same
        // insurance against rounding noise that the X pass already gave).
        ys.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut y_merged: Vec<(f32, f32)> = Vec::new();
        for (y0, y1) in ys {
            if let Some(last) = y_merged.last_mut() {
                if y0 <= last.1 + 1e-3 {
                    last.1 = last.1.max(y1);
                    continue;
                }
            }
            y_merged.push((y0, y1));
        }

        if x0 - cursor > 1e-3 {
            push_box(
                &mut acc,
                [0.5 * (cursor + x0), 0.0, 0.0],
                [x0 - cursor, height, thickness],
            );
        }

        // Walk bottom-to-top inside the column, emitting a horizontal
        // strip wherever there is wall: from the floor up to the first
        // hole (sill), between each adjacent pair of stacked holes, and
        // from the last hole up to the ceiling (lintel).
        let mut prev_y = -half_y;
        for &(y0, y1) in &y_merged {
            let strip_h = y0 - prev_y;
            if strip_h > 1e-3 {
                push_box(
                    &mut acc,
                    [0.5 * (x0 + x1), 0.5 * (prev_y + y0), 0.0],
                    [x1 - x0, strip_h, thickness],
                );
            }
            prev_y = y1;
        }
        let lintel_h = half_y - prev_y;
        if lintel_h > 1e-3 {
            push_box(
                &mut acc,
                [0.5 * (x0 + x1), 0.5 * (prev_y + half_y), 0.0],
                [x1 - x0, lintel_h, thickness],
            );
        }

        cursor = x1;
    }
    if half_x - cursor > 1e-3 {
        push_box(
            &mut acc,
            [0.5 * (cursor + half_x), 0.0, 0.0],
            [half_x - cursor, height, thickness],
        );
    }
    acc
}

fn push_box(acc: &mut Mesh, centre: [f32; 3], size: [f32; 3]) {
    let piece = box_mesh(size, UvMode::Tile);
    let placed = transform_mesh(
        &piece,
        glam::Mat4::from_translation(Vec3::new(centre[0], centre[1], centre[2])),
    );
    append_mesh(acc, &placed);
}

fn append_mesh(acc: &mut Mesh, src: &Mesh) {
    let base = acc.positions.len() as u32;
    acc.positions.extend_from_slice(&src.positions);
    acc.normals.extend_from_slice(&src.normals);
    for &i in &src.indices {
        acc.indices.push(base + i);
    }
    if !src.uvs.is_empty() {
        acc.uvs.extend_from_slice(&src.uvs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_holes_returns_single_box() {
        let m = wall_with_holes([4.0, 2.5, 0.1], &[]);
        // A unit box from box_mesh has 24 vertices (4 per face × 6 faces).
        assert_eq!(m.positions.len(), 24);
    }

    #[test]
    fn one_hole_produces_three_pieces() {
        // Hole centred on the wall: left pier + right pier + lintel + sill.
        let m = wall_with_holes([4.0, 2.5, 0.1], &[[0.0, 0.0, 1.0, 1.4]]);
        // 24 verts per box × 4 pieces.
        assert_eq!(m.positions.len(), 24 * 4);
    }

    #[test]
    fn door_to_floor_drops_sill() {
        // 2.1 m door, wall 2.6 m, sill at floor. The hole's bottom is the
        // wall's bottom, so the sill piece must be omitted (no zero-height
        // box).
        let h = 2.6f32;
        let door_h = 2.1f32;
        let cy = 0.5 * door_h - 0.5 * h; // -0.25
        let m = wall_with_holes([4.0, h, 0.1], &[[0.0, cy, 0.9, door_h]]);
        // 3 pieces: left pier, right pier, lintel. No sill.
        assert_eq!(m.positions.len(), 24 * 3);
    }

    #[test]
    fn stacked_holes_keep_intermediate_wall_strips() {
        // Two holes at the same X but separated on Y — e.g. an elevator
        // shaft's per-storey doorways. The wall between them (the lintel
        // above the lower opening / sill below the upper) must survive.
        // Wall is 6 m tall; lower hole at y∈[-2.9, -1.4], upper at
        // y∈[0.1, 1.6]. Expected pieces around the single column:
        //   left pier + right pier + sill (below lower) + middle strip
        //   (between holes) + lintel (above upper) = 5 boxes.
        let h = 6.0f32;
        let door_h = 1.5f32;
        let cy_lo = -2.9 + 0.5 * door_h; // -2.15
        let cy_hi = 0.1 + 0.5 * door_h;  //  0.85
        let m = wall_with_holes(
            [4.0, h, 0.1],
            &[
                [0.0, cy_lo, 1.0, door_h],
                [0.0, cy_hi, 1.0, door_h],
            ],
        );
        assert_eq!(m.positions.len(), 24 * 5);
    }

    #[test]
    fn out_of_bounds_hole_collapses_to_solid_wall() {
        // Hole sits entirely outside the wall: the wall stays solid.
        let m = wall_with_holes([4.0, 2.5, 0.1], &[[10.0, 0.0, 1.0, 1.0]]);
        assert_eq!(m.positions.len(), 24);
    }
}
