use std::f32::consts::TAU;

use mogen_core::{Mesh, UvMode};

use super::common::disc_rim_uv;

/// Symmetric isoceles triangular prism. `size = [width_x, height_y, depth_z]`.
/// Base sits on -Y at y = -h/2, apex is a ridge along +Z at y = +h/2.
pub fn prism_mesh(size: [f32; 3], mode: UvMode) -> Mesh {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let hz = size[2] * 0.5;
    let sx = size[0];
    let sy = size[1];
    let sz = size[2];
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Slant from a bottom corner up to the ridge: half-width hx in X, full
    // height 2*hy in Y. Used both for the slant face normals and for the V
    // axis on the slant faces in tile mode.
    let slant_len = ((2.0 * hy).powi(2) + hx.powi(2)).sqrt().max(1e-6);
    let n_left = [-(2.0 * hy) / slant_len, hx / slant_len, 0.0];
    let n_right = [(2.0 * hy) / slant_len, hx / slant_len, 0.0];

    // Each face: push its verts with per-face normals and UVs, then fan-triangulate.
    let push_face = |verts: &[[f32; 3]], n: [f32; 3], face_uvs: &[[f32; 2]],
                         positions: &mut Vec<[f32; 3]>,
                         normals: &mut Vec<[f32; 3]>,
                         uvs: &mut Vec<[f32; 2]>,
                         indices: &mut Vec<u32>| {
        let base = positions.len() as u32;
        for (v, uv) in verts.iter().zip(face_uvs.iter()) {
            positions.push(*v);
            normals.push(n);
            uvs.push(*uv);
        }
        for i in 1..(verts.len() as u32 - 1) {
            indices.extend_from_slice(&[base, base + i, base + i + 1]);
        }
    };

    // Per-face UV tables: fit-mode keeps the legacy [0,1]² packing; tile-mode
    // uses world dimensions (sx for the triangle base, sy for its height,
    // slant_len along the slant).
    let (back_uvs, front_uvs, bottom_uvs, left_uvs, right_uvs) = match mode {
        UvMode::Fit => (
            [[0.0, 0.0], [0.5, 1.0], [1.0, 0.0]],
            [[0.0, 0.0], [1.0, 0.0], [0.5, 1.0]],
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
        ),
        UvMode::Tile => (
            // Triangle face: U = x along the base, V = y up to the ridge.
            [[0.0, 0.0], [hx, sy], [sx, 0.0]],
            [[0.0, 0.0], [sx, 0.0], [hx, sy]],
            // Bottom: U = x, V = z.
            [[0.0, 0.0], [sx, 0.0], [sx, sz], [0.0, sz]],
            // Slants: U = z along the ridge, V = slant length from base to ridge.
            [[0.0, 0.0], [sz, 0.0], [sz, slant_len], [0.0, slant_len]],
            [[0.0, 0.0], [0.0, slant_len], [sz, slant_len], [sz, 0.0]],
        ),
    };

    // Back triangle (normal -Z). Triangle apex at (0.5, 1.0) in fit mode.
    push_face(
        &[[-hx, -hy, -hz], [0.0, hy, -hz], [hx, -hy, -hz]],
        [0.0, 0.0, -1.0],
        &back_uvs,
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Front triangle (normal +Z).
    push_face(
        &[[-hx, -hy, hz], [hx, -hy, hz], [0.0, hy, hz]],
        [0.0, 0.0, 1.0],
        &front_uvs,
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Bottom quad (normal -Y).
    push_face(
        &[[-hx, -hy, -hz], [hx, -hy, -hz], [hx, -hy, hz], [-hx, -hy, hz]],
        [0.0, -1.0, 0.0],
        &bottom_uvs,
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Left slant quad.
    push_face(
        &[[-hx, -hy, -hz], [-hx, -hy, hz], [0.0, hy, hz], [0.0, hy, -hz]],
        n_left,
        &left_uvs,
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Right slant quad.
    push_face(
        &[[hx, -hy, -hz], [0.0, hy, -hz], [0.0, hy, hz], [hx, -hy, hz]],
        n_right,
        &right_uvs,
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Regular N-sided pyramid: base polygon on -Y circumscribed by `radius`,
/// apex directly above the centre at +height/2.
pub fn pyramid_mesh(radius: f32, height: f32, sides: u32, mode: UvMode) -> Mesh {
    let sides = sides.max(3);
    let hy = height * 0.5;
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let mut base_verts: Vec<[f32; 3]> = Vec::with_capacity(sides as usize);
    for i in 0..sides {
        let a = (i as f32 / sides as f32) * TAU;
        base_verts.push([a.cos() * radius, -hy, a.sin() * radius]);
    }

    // Base face (normal -Y) with disc UV projection.
    let base_start = positions.len() as u32;
    for (i, v) in base_verts.iter().enumerate() {
        positions.push(*v);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push(disc_rim_uv(v[0], v[2], radius, mode));
        let _ = i;
    }
    for i in 1..(sides - 1) {
        indices.extend_from_slice(&[base_start, base_start + i, base_start + i + 1]);
    }

    // Side faces: each triangular face is unwrapped flat. Fit-mode places the
    // apex at the centre-top of a unit square; tile-mode uses the actual
    // base-edge length for U and the slant height (apex distance from the
    // edge midpoint) for V, so texel density matches a flat plane of the
    // same dimensions.
    let apex = [0.0, hy, 0.0];
    for i in 0..sides {
        let j = (i + 1) % sides;
        let v0 = base_verts[i as usize];
        let v1 = base_verts[j as usize];
        let e1 = [apex[0] - v0[0], apex[1] - v0[1], apex[2] - v0[2]];
        let e2 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
        let n = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
        let n = [n[0] / len, n[1] / len, n[2] / len];

        let (uv0, uv_apex, uv1) = match mode {
            UvMode::Fit => ([0.0, 0.0], [0.5, 1.0], [1.0, 0.0]),
            UvMode::Tile => {
                let edge = ((v1[0] - v0[0]).powi(2)
                    + (v1[1] - v0[1]).powi(2)
                    + (v1[2] - v0[2]).powi(2))
                .sqrt();
                let mid = [(v0[0] + v1[0]) * 0.5, (v0[1] + v1[1]) * 0.5, (v0[2] + v1[2]) * 0.5];
                let slant = ((apex[0] - mid[0]).powi(2)
                    + (apex[1] - mid[1]).powi(2)
                    + (apex[2] - mid[2]).powi(2))
                .sqrt();
                ([0.0, 0.0], [edge * 0.5, slant], [edge, 0.0])
            }
        };

        let base_idx = positions.len() as u32;
        positions.push(v0); normals.push(n); uvs.push(uv0);
        positions.push(apex); normals.push(n); uvs.push(uv_apex);
        positions.push(v1); normals.push(n); uvs.push(uv1);
        indices.extend_from_slice(&[base_idx, base_idx + 1, base_idx + 2]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Right triangular prism ("doorstop" wedge). `size = [x, y, z]`: bottom
/// rectangle sits on -Y (full x×z), back wall is at -Z (tall, reaching to +hy).
/// The top edge is a single line at y=+hy, z=-hz, so the slope rises from the
/// front-bottom edge (at y=-hy, z=+hz) up to the back-top edge. Slope face
/// normal has +Y and +Z components.
pub fn wedge_mesh(size: [f32; 3], mode: UvMode) -> Mesh {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let hz = size[2] * 0.5;
    let sx = size[0];
    let sy = size[1];
    let sz = size[2];
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let push_face = |verts: &[[f32; 3]], n: [f32; 3], face_uvs: &[[f32; 2]],
                     positions: &mut Vec<[f32; 3]>,
                     normals: &mut Vec<[f32; 3]>,
                     uvs: &mut Vec<[f32; 2]>,
                     indices: &mut Vec<u32>| {
        let base = positions.len() as u32;
        for (v, uv) in verts.iter().zip(face_uvs.iter()) {
            positions.push(*v);
            normals.push(n);
            uvs.push(*uv);
        }
        for i in 1..(verts.len() as u32 - 1) {
            indices.extend_from_slice(&[base, base + i, base + i + 1]);
        }
    };

    // Slope length for both the slope-face normal and the V axis on the slope
    // face in tile mode.
    let slant_len = ((2.0 * hy).powi(2) + (2.0 * hz).powi(2)).sqrt().max(1e-6);
    let n_slope = [0.0, (2.0 * hz) / slant_len, (2.0 * hy) / slant_len];

    // Per-face UV tables. Fit mirrors legacy behaviour; tile uses world
    // dimensions on each face's local 2 axes.
    let (back_uvs, bottom_uvs, left_uvs, right_uvs, slope_uvs) = match mode {
        UvMode::Fit => (
            [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            [[0.0, 0.0], [0.0, 1.0], [1.0, 0.0]],
            [[0.0, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
        ),
        UvMode::Tile => (
            // Back wall: U=x, V=y.
            [[0.0, 0.0], [0.0, sy], [sx, sy], [sx, 0.0]],
            // Bottom: U=x, V=z.
            [[0.0, 0.0], [sx, 0.0], [sx, sz], [0.0, sz]],
            // Left tri: spans z and y. v0 at (-hy,-hz), v1 at (-hy,+hz), v2 at (+hy,-hz).
            [[0.0, 0.0], [sz, 0.0], [0.0, sy]],
            // Right tri: v0 at (-hy,-hz), v1 at (+hy,-hz), v2 at (-hy,+hz).
            [[0.0, 0.0], [0.0, sy], [sz, 0.0]],
            // Slope: U=x, V=slant length. Verts are top-back, front-bottom,
            // front-bottom (other x), top-back (other x), so V swaps as we
            // cross the slope.
            [[0.0, slant_len], [0.0, 0.0], [sx, 0.0], [sx, slant_len]],
        ),
    };

    // Back wall (rectangle, normal -Z).
    push_face(
        &[[-hx, -hy, -hz], [-hx, hy, -hz], [hx, hy, -hz], [hx, -hy, -hz]],
        [0.0, 0.0, -1.0],
        &back_uvs,
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Bottom (rectangle, normal -Y).
    push_face(
        &[[-hx, -hy, -hz], [hx, -hy, -hz], [hx, -hy, hz], [-hx, -hy, hz]],
        [0.0, -1.0, 0.0],
        &bottom_uvs,
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Left triangle (normal -X).
    push_face(
        &[[-hx, -hy, -hz], [-hx, -hy, hz], [-hx, hy, -hz]],
        [-1.0, 0.0, 0.0],
        &left_uvs,
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Right triangle (normal +X).
    push_face(
        &[[hx, -hy, -hz], [hx, hy, -hz], [hx, -hy, hz]],
        [1.0, 0.0, 0.0],
        &right_uvs,
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Slope (the hypotenuse face). Normal has +Y and +Z components.
    push_face(
        &[[-hx, hy, -hz], [-hx, -hy, hz], [hx, -hy, hz], [hx, hy, -hz]],
        n_slope,
        &slope_uvs,
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}
