use mogen_core::{Mesh, UvMode};

/// The six box faces in canonical order: `+X, -X, +Y, -Y, +Z, -Z`. This order
/// is the public contract for per-face material assignment (`box (faces=[…])`
/// in the DSL), so it must stay stable.
///
/// Each entry is `(outward normal, four CCW corners, (u_size, v_size) tile
/// dimensions, per-corner tile-space coords)` for a unit-centred box of the
/// given `size`.
fn box_faces(size: [f32; 3]) -> [([f32; 3], [[f32; 3]; 4], [f32; 2], [[f32; 2]; 4]); 6] {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let hz = size[2] * 0.5;
    let sx = size[0];
    let sy = size[1];
    let sz = size[2];
    [
        // +X: U=Z, V=Y. Corner order matches positions.
        ([1.0, 0.0, 0.0],  [[ hx, -hy, -hz], [ hx,  hy, -hz], [ hx,  hy,  hz], [ hx, -hy,  hz]],
            [sz, sy], [[0.0, 0.0], [0.0, sy], [sz, sy], [sz, 0.0]]),
        // -X: U=Z (flipped), V=Y.
        ([-1.0, 0.0, 0.0], [[-hx, -hy,  hz], [-hx,  hy,  hz], [-hx,  hy, -hz], [-hx, -hy, -hz]],
            [sz, sy], [[0.0, 0.0], [0.0, sy], [sz, sy], [sz, 0.0]]),
        // +Y: U=X, V=Z.
        ([0.0, 1.0, 0.0],  [[-hx,  hy,  hz], [ hx,  hy,  hz], [ hx,  hy, -hz], [-hx,  hy, -hz]],
            [sx, sz], [[0.0, 0.0], [sx, 0.0], [sx, sz], [0.0, sz]]),
        // -Y: U=X, V=Z.
        ([0.0, -1.0, 0.0], [[-hx, -hy, -hz], [ hx, -hy, -hz], [ hx, -hy,  hz], [-hx, -hy,  hz]],
            [sx, sz], [[0.0, 0.0], [sx, 0.0], [sx, sz], [0.0, sz]]),
        // +Z: U=X, V=Y.
        ([0.0, 0.0, 1.0],  [[-hx, -hy,  hz], [ hx, -hy,  hz], [ hx,  hy,  hz], [-hx,  hy,  hz]],
            [sx, sy], [[0.0, 0.0], [sx, 0.0], [sx, sy], [0.0, sy]]),
        // -Z: U=X (flipped), V=Y.
        ([0.0, 0.0, -1.0], [[ hx, -hy, -hz], [-hx, -hy, -hz], [-hx,  hy, -hz], [ hx,  hy, -hz]],
            [sx, sy], [[0.0, 0.0], [sx, 0.0], [sx, sy], [0.0, sy]]),
    ]
}

pub fn box_mesh(size: [f32; 3], mode: UvMode) -> Mesh {
    box_faces_mesh(size, mode, &[0, 1, 2, 3, 4, 5])
}

/// Build a box made of only the requested faces (indices into [`box_faces`]'s
/// canonical `+X,-X,+Y,-Y,+Z,-Z` order). Used to split a box into per-face
/// quad groups so each group can carry its own material. Out-of-range indices
/// are ignored.
pub fn box_faces_mesh(size: [f32; 3], mode: UvMode, include: &[usize]) -> Mesh {
    let faces = box_faces(size);
    let mut positions = Vec::with_capacity(include.len() * 4);
    let mut normals = Vec::with_capacity(include.len() * 4);
    let mut uvs = Vec::with_capacity(include.len() * 4);
    let mut indices = Vec::with_capacity(include.len() * 6);
    // Fit-mode UVs collapse every face into the unit square so each face shows
    // the texture once. Tile-mode UVs are world-space metres.
    const FIT_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    for &fi in include {
        let Some((normal, verts, _size, tile_uvs)) = faces.get(fi).copied() else {
            continue;
        };
        let base = positions.len() as u32;
        for (i, v) in verts.into_iter().enumerate() {
            positions.push(v);
            normals.push(normal);
            uvs.push(match mode {
                UvMode::Fit => FIT_UVS[i],
                UvMode::Tile => tile_uvs[i],
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Mesh { positions, normals, uvs, indices, ..Default::default() }
}
