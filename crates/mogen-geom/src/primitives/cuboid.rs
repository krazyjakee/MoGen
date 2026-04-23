use mogen_core::{Mesh, UvMode};

pub fn box_mesh(size: [f32; 3], mode: UvMode) -> Mesh {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let hz = size[2] * 0.5;
    let sx = size[0];
    let sy = size[1];
    let sz = size[2];

    // Each face: outward normal, four CCW corners, the (u_axis_size, v_axis_size)
    // that defines the face's tile-space dimensions, and per-corner local 2D
    // coordinates in [0, u_size]×[0, v_size] for tile mode.
    let faces: [([f32; 3], [[f32; 3]; 4], [f32; 2], [[f32; 2]; 4]); 6] = [
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
    ];

    let mut positions = Vec::with_capacity(24);
    let mut normals = Vec::with_capacity(24);
    let mut uvs = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    // Fit-mode UVs collapse every face into the unit square so each face shows
    // the texture once. Tile-mode UVs are world-space metres.
    const FIT_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    for (normal, verts, _size, tile_uvs) in faces {
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
