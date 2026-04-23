use mogen_core::{Mesh, UvMode};

use crate::cleanup::recompute_normals;

/// XZ plane centered at origin, facing +Y.
pub fn plane_mesh(size: [f32; 2], mode: UvMode) -> Mesh {
    let hx = size[0] * 0.5;
    let hz = size[1] * 0.5;
    let sx = size[0];
    let sz = size[1];
    let n = [0.0, 1.0, 0.0];
    let uvs = match mode {
        // U follows +X, V follows +Z so "up" in texture space matches "back" in world space.
        UvMode::Fit => vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        UvMode::Tile => vec![[0.0, 0.0], [sx, 0.0], [sx, sz], [0.0, sz]],
    };
    Mesh {
        positions: vec![[-hx, 0.0, -hz], [hx, 0.0, -hz], [hx, 0.0, hz], [-hx, 0.0, hz]],
        normals: vec![n; 4],
        uvs,
        indices: vec![0, 3, 2, 0, 2, 1],
        ..Default::default()
    }
}

/// XY quad centered at origin, facing +Z. Useful as a billboard / decal plane,
/// complements `plane_mesh` which lies flat.
pub fn quad_mesh(size: [f32; 2], mode: UvMode) -> Mesh {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let sx = size[0];
    let sy = size[1];
    let n = [0.0, 0.0, 1.0];
    let uvs = match mode {
        // Fit-mode origin is top-left so the texture reads upright when V
        // points down (image convention). Tile-mode mirrors that orientation.
        UvMode::Fit => vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        UvMode::Tile => vec![[0.0, sy], [sx, sy], [sx, 0.0], [0.0, 0.0]],
    };
    Mesh {
        positions: vec![[-hx, -hy, 0.0], [hx, -hy, 0.0], [hx, hy, 0.0], [-hx, hy, 0.0]],
        normals: vec![n; 4],
        uvs,
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    }
}

/// Bent-plane patch, useful for petals, leaves, fish fins, roof tiles. The
/// unbent plane lies flat in XZ facing +Y. `size = [x, z]`. `bend_u` is the
/// total bend angle (radians) along the X axis — the left and right edges
/// curl toward +Y as the angle grows. `bend_v` does the same along Z.
/// `segments_u` / `segments_v` subdivide the patch so the bend looks smooth.
/// The mesh is single-sided (facing +Y when unbent); wrap in `mirror` or pair
/// with a flipped copy if you need a double-sided leaf.
pub fn curved_plane_mesh(
    size: [f32; 2],
    bend_u: f32,
    bend_v: f32,
    segments_u: u32,
    segments_v: u32,
    mode: UvMode,
) -> Mesh {
    let sx = size[0];
    let sz = size[1];
    let su = segments_u.max(1);
    let sv = segments_v.max(1);
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Arc-length parameterization: when bend is non-zero, the radius R = s / θ
    // makes the total arc length equal to the declared dimension, so an
    // unbent size=[1,1] patch has the same extent as a bent one. Positive
    // bend lifts both edges toward +Y.
    let pos_for = |u: f32, v: f32| -> [f32; 3] {
        let (x, y_u) = arc_offset(u, sx, bend_u);
        let (z, y_v) = arc_offset(v, sz, bend_v);
        [x, y_u + y_v, z]
    };

    let (u_scale, v_scale) = match mode {
        UvMode::Fit => (1.0, 1.0),
        // Tile by surface arc length, which equals declared (sx, sz) under the
        // arc-length parameterisation above.
        UvMode::Tile => (sx, sz),
    };
    for iv in 0..=sv {
        let tv = iv as f32 / sv as f32;
        let centered_v = tv - 0.5;
        for iu in 0..=su {
            let tu = iu as f32 / su as f32;
            let centered_u = tu - 0.5;
            positions.push(pos_for(centered_u, centered_v));
            uvs.push([tu * u_scale, tv * v_scale]);
        }
    }
    let cols = su + 1;
    for iv in 0..sv {
        for iu in 0..su {
            let a = iv * cols + iu;
            let b = a + 1;
            let d = a + cols;
            let c = d + 1;
            indices.extend_from_slice(&[a, d, c, a, c, b]);
        }
    }

    // Face-normal averaging yields correct smooth normals regardless of bend.
    let mesh = Mesh {
        positions,
        normals: vec![[0.0, 1.0, 0.0]; ((su + 1) * (sv + 1)) as usize],
        uvs,
        indices,
        ..Default::default()
    };
    recompute_normals(&mesh)
}

/// Offset from the flat parameter `t ∈ [-0.5, 0.5]` to `(axis, y)` coordinates
/// after a bend of `angle` radians across the full extent `s`. Handles the
/// `angle ≈ 0` case by collapsing to a flat line.
fn arc_offset(t: f32, s: f32, angle: f32) -> (f32, f32) {
    if angle.abs() < 1e-5 {
        return (t * s, 0.0);
    }
    let r = s / angle;
    let phi = t * angle;
    (r * phi.sin(), r * (1.0 - phi.cos()))
}
