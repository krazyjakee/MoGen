use std::f32::consts::TAU;

use mogen_core::{Mesh, UvMode};

use super::common::{disc_center_uv, disc_rim_uv};

/// Flat disc lying on the XZ plane, facing +Y. One-sided; CSG on a disc alone
/// is ill-defined but it is handy as a terminator cap or decal.
pub fn disc_mesh(radius: f32, segments: u32, mode: UvMode) -> Mesh {
    let segments = segments.max(3);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();
    let n = [0.0, 1.0, 0.0];

    let center = positions.len() as u32;
    positions.push([0.0, 0.0, 0.0]);
    normals.push(n);
    uvs.push(disc_center_uv(mode));
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, 0.0, sa * radius]);
        normals.push(n);
        uvs.push(disc_rim_uv(ca * radius, sa * radius, radius, mode));
    }
    // Winding matches cylinder's +Y cap: centre → next_ring → this_ring.
    for i in 0..segments {
        indices.extend_from_slice(&[center, center + 2 + i, center + 1 + i]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}
