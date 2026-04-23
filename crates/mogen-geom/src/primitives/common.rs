use mogen_core::{Mesh, UvMode};

/// UV for a disc cap centre vertex. Fit-mode is `(0.5, 0.5)` (centre of the
/// unit square); tile-mode uses world origin so rim verts lie at world (x, z).
#[inline]
pub(super) fn disc_center_uv(mode: UvMode) -> [f32; 2] {
    match mode {
        UvMode::Fit => [0.5, 0.5],
        UvMode::Tile => [0.0, 0.0],
    }
}

/// UV for a disc cap rim vertex at world (x, z) with bounding `radius`. Fit
/// projects the disc into `[0, 1]²` (`0.5 + xy/(2r)`); tile keeps world units.
#[inline]
pub(super) fn disc_rim_uv(x: f32, z: f32, radius: f32, mode: UvMode) -> [f32; 2] {
    match mode {
        UvMode::Fit => {
            let s = if radius > 1e-6 { 0.5 / radius } else { 0.0 };
            [x * s + 0.5, z * s + 0.5]
        }
        UvMode::Tile => [x, z],
    }
}

/// Add a row×col patch to `mesh`, triangulating into quads. The winding of
/// all triangles is flipped together if the first triangle's geometric normal
/// disagrees with the stored vertex normal — this lets callers emit patches in
/// whichever parameter order is convenient and get consistent outward-facing
/// triangles regardless.
pub(super) fn push_patch(
    mesh: &mut Mesh,
    patch_pos: &[[f32; 3]],
    patch_n: &[[f32; 3]],
    rows: usize,
    cols: usize,
) {
    let base = mesh.positions.len() as u32;
    mesh.positions.extend_from_slice(patch_pos);
    mesh.normals.extend_from_slice(patch_n);

    if rows < 2 || cols < 2 {
        return;
    }
    let mut idx: Vec<u32> = Vec::with_capacity((rows - 1) * (cols - 1) * 6);
    for r in 0..rows - 1 {
        for c in 0..cols - 1 {
            let a = base + (r * cols + c) as u32;
            let b = base + (r * cols + c + 1) as u32;
            let d = base + ((r + 1) * cols + c) as u32;
            let e = base + ((r + 1) * cols + c + 1) as u32;
            idx.extend_from_slice(&[a, b, e, a, e, d]);
        }
    }

    // First non-degenerate triangle determines winding against its vertex normal.
    let mut flip = false;
    for tri in idx.chunks(3) {
        let p0 = mesh.positions[tri[0] as usize];
        let p1 = mesh.positions[tri[1] as usize];
        let p2 = mesh.positions[tri[2] as usize];
        let n0 = mesh.normals[tri[0] as usize];
        let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
        let tn = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let len2 = tn[0] * tn[0] + tn[1] * tn[1] + tn[2] * tn[2];
        if len2 < 1e-20 {
            continue;
        }
        let dot = tn[0] * n0[0] + tn[1] * n0[1] + tn[2] * n0[2];
        flip = dot < 0.0;
        break;
    }
    if flip {
        for i in (0..idx.len()).step_by(3) {
            idx.swap(i + 1, i + 2);
        }
    }
    mesh.indices.extend(idx);
}
