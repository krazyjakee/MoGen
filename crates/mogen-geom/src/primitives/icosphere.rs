use std::collections::HashMap;
use std::f32::consts::{PI, TAU};

use mogen_core::{Mesh, UvMode};

/// Geodesic sphere built by subdividing an icosahedron. Produces more uniform
/// triangle area than `sphere_mesh` and avoids UV sphere pole pinching.
pub fn icosphere_mesh(radius: f32, subdivisions: u32, mode: UvMode) -> Mesh {
    let phi = (1.0 + 5.0_f32.sqrt()) * 0.5;
    let inv = 1.0 / (1.0 + phi * phi).sqrt();
    let mut verts: Vec<[f32; 3]> = [
        [-1.0,  phi, 0.0], [ 1.0,  phi, 0.0], [-1.0, -phi, 0.0], [ 1.0, -phi, 0.0],
        [ 0.0, -1.0,  phi], [ 0.0,  1.0,  phi], [ 0.0, -1.0, -phi], [ 0.0,  1.0, -phi],
        [ phi, 0.0, -1.0], [ phi, 0.0,  1.0], [-phi, 0.0, -1.0], [-phi, 0.0,  1.0],
    ]
    .iter()
    .map(|v| [v[0] * inv, v[1] * inv, v[2] * inv])
    .collect();

    // 20 canonical icosahedron faces, wound CCW viewed from outside.
    let mut faces: Vec<[u32; 3]> = vec![
        [0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11],
        [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8],
        [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9],
        [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1],
    ];

    for _ in 0..subdivisions {
        let mut cache: HashMap<(u32, u32), u32> = HashMap::new();
        let mut next = Vec::with_capacity(faces.len() * 4);
        for f in &faces {
            let ab = icosphere_midpoint(&mut verts, &mut cache, f[0], f[1]);
            let bc = icosphere_midpoint(&mut verts, &mut cache, f[1], f[2]);
            let ca = icosphere_midpoint(&mut verts, &mut cache, f[2], f[0]);
            next.push([f[0], ab, ca]);
            next.push([f[1], bc, ab]);
            next.push([f[2], ca, bc]);
            next.push([ab, bc, ca]);
        }
        faces = next;
    }

    let positions: Vec<[f32; 3]> =
        verts.iter().map(|v| [v[0] * radius, v[1] * radius, v[2] * radius]).collect();
    let normals = verts.clone();
    // Equirectangular UVs derived from per-vertex unit normals. Has a known
    // seam at theta=0 and pole pinching at y=±1 — standard for a UV sphere
    // and acceptable for a first-pass texture pipeline. Tile mode scales by
    // arc lengths so texel density matches a sphere/cylinder of the same
    // radius.
    let (u_scale, v_scale) = match mode {
        UvMode::Fit => (1.0, 1.0),
        UvMode::Tile => (TAU * radius, PI * radius),
    };
    let uvs: Vec<[f32; 2]> = verts
        .iter()
        .map(|v| {
            let u = v[2].atan2(v[0]) / TAU + 0.5;
            let vv = v[1].clamp(-1.0, 1.0).acos() / PI;
            [u * u_scale, vv * v_scale]
        })
        .collect();
    let indices: Vec<u32> = faces.into_iter().flat_map(|f| f.into_iter()).collect();

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

fn icosphere_midpoint(
    verts: &mut Vec<[f32; 3]>,
    cache: &mut HashMap<(u32, u32), u32>,
    a: u32,
    b: u32,
) -> u32 {
    let key = if a < b { (a, b) } else { (b, a) };
    if let Some(&i) = cache.get(&key) {
        return i;
    }
    let va = verts[a as usize];
    let vb = verts[b as usize];
    let m = [(va[0] + vb[0]) * 0.5, (va[1] + vb[1]) * 0.5, (va[2] + vb[2]) * 0.5];
    let len = (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt().max(1e-12);
    let idx = verts.len() as u32;
    verts.push([m[0] / len, m[1] / len, m[2] / len]);
    cache.insert(key, idx);
    idx
}
