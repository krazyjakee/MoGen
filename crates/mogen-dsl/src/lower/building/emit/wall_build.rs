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
//!
//! The *planning* — deciding which solid rectangles survive — lives in
//! [`crate::lower::arch::openings::solid_panels`], because a mitred or curved
//! wall needs the same answer without wanting axis-aligned boxes. What remains
//! here is the box builder.

use glam::Vec3;

use mogen_core::{Mesh, UvMode};
use mogen_geom::{box_mesh, transform_mesh};

use crate::lower::arch::openings::solid_panels;

pub(super) fn wall_with_holes(size: [f32; 3], holes: &[[f32; 4]]) -> Mesh {
    let [length, height, thickness] = size;
    if length <= 0.0 || height <= 0.0 || thickness <= 0.0 {
        return Mesh::default();
    }

    let panels = solid_panels(length, height, holes);
    if panels.is_empty() {
        return Mesh::default();
    }
    // An unbroken wall stays a single untransformed box, exactly as before.
    if panels.len() == 1 && panels[0].covers(length, height) {
        return box_mesh(size, UvMode::Tile);
    }

    let mut acc = Mesh::default();
    for p in &panels {
        push_box(
            &mut acc,
            [p.centre_x(), p.centre_y(), 0.0],
            [p.width(), p.height(), thickness],
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
