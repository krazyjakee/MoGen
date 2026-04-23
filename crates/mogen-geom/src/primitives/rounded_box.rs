use std::f32::consts::PI;

use mogen_core::{Mesh, UvMode};

use super::common::push_patch;
use super::cuboid::box_mesh;

/// Box of `size` with corners rounded to `radius`. Built as the Minkowski sum
/// of an interior "core" box with a sphere: six flat face rectangles, twelve
/// quarter-cylinder edge strips, and eight spherical-octant corner patches.
/// Seams along the interior-box silhouette are vertex-duplicated, but normals
/// at the seam match on both sides so shading is smooth.
pub fn rounded_box_mesh(size: [f32; 3], radius: f32, segments: u32, mode: UvMode) -> Mesh {
    let sx = size[0].max(0.0);
    let sy = size[1].max(0.0);
    let sz = size[2].max(0.0);
    let r = radius.min(sx * 0.5).min(sy * 0.5).min(sz * 0.5).max(0.0);
    if r < 1e-6 {
        return box_mesh([sx, sy, sz], mode);
    }
    let hx = sx * 0.5;
    let hy = sy * 0.5;
    let hz = sz * 0.5;
    let cx = hx - r;
    let cy = hy - r;
    let cz = hz - r;
    let seg = segments.max(1);

    let mut mesh = Mesh::default();

    // Six flat face rectangles (shrunken from full size so they don't bleed
    // past the rounded edges).
    let flat_faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        ([1.0, 0.0, 0.0],  [[ hx, -cy, -cz], [ hx,  cy, -cz], [ hx,  cy,  cz], [ hx, -cy,  cz]]),
        ([-1.0, 0.0, 0.0], [[-hx, -cy,  cz], [-hx,  cy,  cz], [-hx,  cy, -cz], [-hx, -cy, -cz]]),
        ([0.0, 1.0, 0.0],  [[-cx,  hy,  cz], [ cx,  hy,  cz], [ cx,  hy, -cz], [-cx,  hy, -cz]]),
        ([0.0, -1.0, 0.0], [[-cx, -hy, -cz], [ cx, -hy, -cz], [ cx, -hy,  cz], [-cx, -hy,  cz]]),
        ([0.0, 0.0, 1.0],  [[-cx, -cy,  hz], [ cx, -cy,  hz], [ cx,  cy,  hz], [-cx,  cy,  hz]]),
        ([0.0, 0.0, -1.0], [[ cx, -cy, -hz], [-cx, -cy, -hz], [-cx,  cy, -hz], [ cx,  cy, -hz]]),
    ];
    for (n, quad) in flat_faces {
        let base = mesh.positions.len() as u32;
        for v in quad {
            mesh.positions.push(v);
            mesh.normals.push(n);
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // Twelve quarter-cylinder edges, one per (axis, perp1_sign, perp2_sign).
    let edges: [(usize, f32, f32); 12] = [
        (0,  1.0,  1.0), (0, -1.0,  1.0), (0, -1.0, -1.0), (0,  1.0, -1.0),
        (1,  1.0,  1.0), (1, -1.0,  1.0), (1, -1.0, -1.0), (1,  1.0, -1.0),
        (2,  1.0,  1.0), (2, -1.0,  1.0), (2, -1.0, -1.0), (2,  1.0, -1.0),
    ];
    for (axis, s1, s2) in edges {
        let (along_half, cp1, cp2) = match axis {
            0 => (cx, cy, cz),
            1 => (cy, cx, cz),
            2 => (cz, cx, cy),
            _ => unreachable!(),
        };
        let rows = (seg + 1) as usize;
        let cols = 2usize;
        let mut patch_pos: Vec<[f32; 3]> = Vec::with_capacity(rows * cols);
        let mut patch_n: Vec<[f32; 3]> = Vec::with_capacity(rows * cols);
        for ri in 0..=seg {
            let t = (ri as f32 / seg as f32) * (PI * 0.5);
            let (sin_t, cos_t) = t.sin_cos();
            let np1 = s1 * cos_t;
            let np2 = s2 * sin_t;
            for c in 0..cols {
                let along = if c == 0 { -along_half } else { along_half };
                let (pos, n) = match axis {
                    0 => (
                        [along, s1 * cp1 + r * np1, s2 * cp2 + r * np2],
                        [0.0, np1, np2],
                    ),
                    1 => (
                        [s1 * cp1 + r * np1, along, s2 * cp2 + r * np2],
                        [np1, 0.0, np2],
                    ),
                    2 => (
                        [s1 * cp1 + r * np1, s2 * cp2 + r * np2, along],
                        [np1, np2, 0.0],
                    ),
                    _ => unreachable!(),
                };
                patch_pos.push(pos);
                patch_n.push(n);
            }
        }
        push_patch(&mut mesh, &patch_pos, &patch_n, rows, cols);
    }

    // Eight sphere-octant corner patches.
    let corners: [[f32; 3]; 8] = [
        [ 1.0,  1.0,  1.0], [-1.0,  1.0,  1.0], [-1.0, -1.0,  1.0], [ 1.0, -1.0,  1.0],
        [ 1.0,  1.0, -1.0], [-1.0,  1.0, -1.0], [-1.0, -1.0, -1.0], [ 1.0, -1.0, -1.0],
    ];
    for s in corners {
        let sx_s = s[0]; let sy_s = s[1]; let sz_s = s[2];
        let rows = (seg + 1) as usize;
        let cols = (seg + 1) as usize;
        let mut patch_pos: Vec<[f32; 3]> = Vec::with_capacity(rows * cols);
        let mut patch_n: Vec<[f32; 3]> = Vec::with_capacity(rows * cols);
        for ri in 0..=seg {
            let phi = (ri as f32 / seg as f32) * (PI * 0.5);
            let (sin_phi, cos_phi) = phi.sin_cos();
            for ci in 0..=seg {
                let theta = (ci as f32 / seg as f32) * (PI * 0.5);
                let (sin_th, cos_th) = theta.sin_cos();
                let nx = sx_s * sin_phi * cos_th;
                let ny = sy_s * cos_phi;
                let nz = sz_s * sin_phi * sin_th;
                patch_pos.push([sx_s * cx + r * nx, sy_s * cy + r * ny, sz_s * cz + r * nz]);
                patch_n.push([nx, ny, nz]);
            }
        }
        push_patch(&mut mesh, &patch_pos, &patch_n, rows, cols);
    }

    // Triplanar UV projection: each vertex picks the dominant normal axis and
    // projects its position onto the remaining two. Box-like surfaces get
    // predictable per-face unwraps; rounded edges and corners blend smoothly
    // because neighbouring verts pick the same axis until the normal crosses
    // 45°.
    mesh.uvs = triplanar_uvs_for_box(&mesh.positions, &mesh.normals, [sx, sy, sz], mode);
    mesh
}

/// Triplanar UVs for a box-like mesh. `Tile` mode uses world-space coordinates
/// (so texel density is identical across faces and matches a flat box of the
/// same dimensions); `Fit` mode normalises each axis to `[0, 1]` based on the
/// bounding extent.
fn triplanar_uvs_for_box(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    size: [f32; 3],
    mode: UvMode,
) -> Vec<[f32; 2]> {
    let [sx, sy, sz] = size;
    let (ix, iy, iz, uo, vo) = match mode {
        UvMode::Tile => (1.0, 1.0, 1.0, 0.0, 0.0),
        UvMode::Fit => (
            if sx > 1e-6 { 1.0 / sx } else { 1.0 },
            if sy > 1e-6 { 1.0 / sy } else { 1.0 },
            if sz > 1e-6 { 1.0 / sz } else { 1.0 },
            0.5,
            0.5,
        ),
    };
    positions
        .iter()
        .zip(normals.iter())
        .map(|(p, n)| {
            let (ax, ay, az) = (n[0].abs(), n[1].abs(), n[2].abs());
            if ax >= ay && ax >= az {
                [(p[2] * iz) + uo, (p[1] * iy) + vo]
            } else if ay >= az {
                [(p[0] * ix) + uo, (p[2] * iz) + vo]
            } else {
                [(p[0] * ix) + uo, (p[1] * iy) + vo]
            }
        })
        .collect()
}
