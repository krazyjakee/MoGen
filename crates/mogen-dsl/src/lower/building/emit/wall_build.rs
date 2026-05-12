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

    // Merge X-overlapping holes. Should not happen in practice, but rounding
    // noise across BSP/grid boundaries makes it cheap insurance — overlapping
    // holes that collide on X would otherwise emit zero-width piers between
    // them.
    let mut merged: Vec<(f32, f32, f32, f32)> = Vec::new();
    for s in spans {
        if let Some(last) = merged.last_mut() {
            if s.0 <= last.1 + 1e-3 {
                last.1 = last.1.max(s.1);
                last.2 = last.2.min(s.2);
                last.3 = last.3.max(s.3);
                continue;
            }
        }
        merged.push(s);
    }

    let mut acc = Mesh::default();
    let mut cursor = -half_x;
    for &(x0, x1, y0, y1) in &merged {
        if x0 - cursor > 1e-3 {
            push_box(
                &mut acc,
                [0.5 * (cursor + x0), 0.0, 0.0],
                [x0 - cursor, height, thickness],
            );
        }
        let lintel_h = half_y - y1;
        if lintel_h > 1e-3 {
            push_box(
                &mut acc,
                [0.5 * (x0 + x1), 0.5 * (y1 + half_y), 0.0],
                [x1 - x0, lintel_h, thickness],
            );
        }
        let sill_h = y0 + half_y;
        if sill_h > 1e-3 {
            push_box(
                &mut acc,
                [0.5 * (x0 + x1), 0.5 * (-half_y + y0), 0.0],
                [x1 - x0, sill_h, thickness],
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
    fn out_of_bounds_hole_collapses_to_solid_wall() {
        // Hole sits entirely outside the wall: the wall stays solid.
        let m = wall_with_holes([4.0, 2.5, 0.1], &[[10.0, 0.0, 1.0, 1.0]]);
        assert_eq!(m.positions.len(), 24);
    }
}
