use std::f32::consts::PI;

use mogen_core::{Mesh, UvMode};

use super::plane::quad_mesh;

/// Cross-plane "leaf card" — N alpha-cutout quads arranged as a fan around the
/// +Y axis. Each quad lies in a vertical plane with its bottom edge at y=0, so
/// the leaf hangs from / grows out of the local origin and a stem connector
/// can mount it directly to a branch tip.
///
/// `cards = 2` produces the classic perpendicular cross used everywhere in
/// game foliage; `cards = 3` gives a 60°-spaced fan that defeats the
/// edge-on-disappears artefact when the camera circles the model.
///
/// The mesh is a single combined `Mesh` (one draw call) with single-winding
/// triangles. Pair it with a `material (alpha_mode="mask", double_sided=1)`
/// so the renderer draws both sides — duplicating the winding here on top of
/// `doubleSided` would Z-fight against itself.
pub fn leaf_card_mesh(size: [f32; 2], cards: u32, mode: UvMode) -> Mesh {
    let cards = cards.max(1);
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let height = size[1];
    for k in 0..cards {
        // Distribute cards evenly across [0, π) — a rotation by π puts the
        // card back on top of itself, so cards=2 → {0, π/2}, cards=3 →
        // {0, π/3, 2π/3}.
        let angle = (k as f32) * PI / (cards as f32);
        let (sa, ca) = angle.sin_cos();
        let card = quad_mesh(size, mode);
        let base = positions.len() as u32;
        for (i, p) in card.positions.iter().enumerate() {
            // quad_mesh sits in XY plane centred on origin facing +Z. Lift its
            // bottom edge to y=0 (so the stem mounts at origin), then rotate
            // around Y by `angle`.
            let x = p[0];
            let y = p[1] + height * 0.5;
            let z = p[2];
            let (rx, rz) = (x * ca + z * sa, -x * sa + z * ca);
            positions.push([rx, y, rz]);
            let n = card.normals[i];
            let (nx, nz) = (n[0] * ca + n[2] * sa, -n[0] * sa + n[2] * ca);
            normals.push([nx, n[1], nz]);
            uvs.push(card.uvs[i]);
        }
        for tri in card.indices.chunks_exact(3) {
            indices.extend_from_slice(&[base + tri[0], base + tri[1], base + tri[2]]);
        }
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}
