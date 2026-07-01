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

/// An authored per-face UV transform: UVs are baked as
/// `offset + scale * face_local`, optionally with the two in-plane axes swapped
/// first. Emitted verbatim (never clamped or wrapped) so tiling / mirroring is
/// left to the sampler. When a face carries one of these it bypasses the
/// procedural Fit/Tile projection entirely.
#[derive(Debug, Clone, Copy)]
pub struct FaceUvXform {
    pub scale: [f32; 2],
    pub offset: [f32; 2],
    pub swap: bool,
}

/// Build a box made of only the requested faces (indices into [`box_faces`]'s
/// canonical `+X,-X,+Y,-Y,+Z,-Z` order). Used to split a box into per-face
/// quad groups so each group can carry its own material. Out-of-range indices
/// are ignored.
pub fn box_faces_mesh(size: [f32; 3], mode: UvMode, include: &[usize]) -> Mesh {
    let pairs: Vec<(usize, Option<FaceUvXform>)> =
        include.iter().map(|&fi| (fi, None)).collect();
    box_faces_mesh_authored(size, mode, &pairs)
}

/// Like [`box_faces_mesh`] but each requested face may carry an optional
/// authored UV transform. Faces with `Some(xform)` bake face-local UVs
/// (`offset + scale * local`) instead of the `mode` projection; faces with
/// `None` fall back to the usual Fit/Tile UVs, so bare-string faces are
/// byte-identical to the old path.
pub fn box_faces_mesh_authored(
    size: [f32; 3],
    mode: UvMode,
    include: &[(usize, Option<FaceUvXform>)],
) -> Mesh {
    let faces = box_faces(size);
    let mut positions = Vec::with_capacity(include.len() * 4);
    let mut normals = Vec::with_capacity(include.len() * 4);
    let mut uvs = Vec::with_capacity(include.len() * 4);
    let mut indices = Vec::with_capacity(include.len() * 6);
    // Fit-mode UVs collapse every face into the unit square so each face shows
    // the texture once. Tile-mode UVs are world-space metres.
    const FIT_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    for &(fi, xform) in include {
        let Some((normal, verts, _size, tile_uvs)) = faces.get(fi).copied() else {
            continue;
        };
        let base = positions.len() as u32;
        for (i, v) in verts.into_iter().enumerate() {
            positions.push(v);
            normals.push(normal);
            uvs.push(match xform {
                Some(x) => authored_face_uv(fi, v, size, x),
                None => match mode {
                    UvMode::Fit => FIT_UVS[i],
                    UvMode::Tile => tile_uvs[i],
                },
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Face-local authored UV for one corner. The face's normal axis (X/Y/Z) picks
/// the two in-plane axes in ascending index order (X→(Y,Z), Y→(X,Z), Z→(X,Y));
/// the local coordinate on each is the vertex offset from the box's min corner
/// on that axis (`v[ax] + size[ax]/2`). Then `offset + scale * (swap ? …)`.
fn authored_face_uv(fi: usize, v: [f32; 3], size: [f32; 3], x: FaceUvXform) -> [f32; 2] {
    let (u_ax, v_ax) = match fi / 2 {
        0 => (1, 2), // ±X → (Y, Z)
        1 => (0, 2), // ±Y → (X, Z)
        _ => (0, 1), // ±Z → (X, Y)
    };
    let local_u = v[u_ax] + size[u_ax] * 0.5;
    let local_v = v[v_ax] + size[v_ax] * 0.5;
    let (lu, lv) = if x.swap { (local_v, local_u) } else { (local_u, local_v) };
    [x.offset[0] + x.scale[0] * lu, x.offset[1] + x.scale[1] * lv]
}
