//! Bicubic Bézier patch — a 4×4 grid of control points evaluated as a
//! tensor-product cubic Bézier surface.
//!
//! The patch is the standard CAD/animation building block for organic skin
//! panels: faces, masks, fenders, hoods, sails, fabric, pillows, leaves with
//! controlled silhouette, soft plates. Authors specify 16 control points
//! row-major (u rows × v columns) — the corners of the 4×4 grid pin down
//! the patch corners exactly, the inner 4 control how steep the bulge is,
//! and the edge-but-not-corner 8 control the curvature along each edge.

use mogen_core::{Mesh, UvMode};

use crate::cleanup::recompute_normals;

/// Build a bicubic Bézier patch from `points`, a 16-element list of vec3
/// control points stored row-major: `points[u_row * 4 + v_col]`.
///
/// `segments_u` / `segments_v` are the parametric tessellation along U/V;
/// the resulting mesh has `(segments_u+1) × (segments_v+1)` vertices and
/// `2 · segments_u · segments_v` triangles.
///
/// `points.len() != 16` produces an empty mesh — caller is expected to
/// validate that before reaching here. The DSL lowering layer enforces the
/// 16-point requirement and reports a friendly error.
pub fn bezier_patch_mesh(
    points: &[[f32; 3]],
    segments_u: u32,
    segments_v: u32,
    mode: UvMode,
) -> Mesh {
    if points.len() != 16 {
        return Mesh::default();
    }
    let su = segments_u.max(1);
    let sv = segments_v.max(1);
    let nu = su as usize + 1;
    let nv = sv as usize + 1;

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(nu * nv);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(nu * nv);
    for i in 0..nu {
        let u = i as f32 / su as f32;
        let bu = bernstein4(u);
        for j in 0..nv {
            let v = j as f32 / sv as f32;
            let bv = bernstein4(v);
            // Tensor-product evaluation: P(u,v) = Σᵢ Σⱼ Bᵢ(u) · Bⱼ(v) · Pᵢⱼ
            let mut acc = [0.0_f32; 3];
            for ki in 0..4 {
                for kj in 0..4 {
                    let p = points[ki * 4 + kj];
                    let w = bu[ki] * bv[kj];
                    acc[0] += p[0] * w;
                    acc[1] += p[1] * w;
                    acc[2] += p[2] * w;
                }
            }
            positions.push(acc);
            uvs.push(match mode {
                UvMode::Fit => [u, v],
                // Tile mode: use parametric coords scaled by patch chord
                // length is overkill; emit `u`/`v` like fit but multiplied
                // by the corner-to-corner chord on each axis so texel
                // density is consistent across patches of different sizes.
                UvMode::Tile => [u * chord_length_u(points), v * chord_length_v(points)],
            });
        }
    }

    let mut indices: Vec<u32> = Vec::with_capacity(su as usize * sv as usize * 6);
    for i in 0..su as usize {
        for j in 0..sv as usize {
            let a = (i * nv + j) as u32;
            let b = a + 1;
            let c = a + nv as u32;
            let d = c + 1;
            // CCW from +N face (where N is the local surface normal): treat
            // increasing-u as the "depth" axis and increasing-v as the
            // "width" axis. Two tris per quad.
            indices.push(a);
            indices.push(b);
            indices.push(d);
            indices.push(a);
            indices.push(d);
            indices.push(c);
        }
    }

    let mesh = Mesh {
        positions,
        normals: Vec::new(),
        uvs,
        indices,
        ..Default::default()
    };
    recompute_normals(&mesh)
}

/// Cubic Bernstein basis values at `t`: `(1-t)³, 3(1-t)²t, 3(1-t)t², t³`.
#[inline]
fn bernstein4(t: f32) -> [f32; 4] {
    let it = 1.0 - t;
    let it2 = it * it;
    let it3 = it2 * it;
    let t2 = t * t;
    let t3 = t2 * t;
    [it3, 3.0 * it2 * t, 3.0 * it * t2, t3]
}

/// Chord length along U (sum of distances along the centre row of control
/// points). Cheap proxy for the actual surface arc length; good enough for
/// tile-mode UV scaling.
fn chord_length_u(points: &[[f32; 3]]) -> f32 {
    let mut acc = 0.0_f32;
    for k in 0..3 {
        let a = points[k * 4 + 1];
        let b = points[(k + 1) * 4 + 1];
        let dx = a[0] - b[0];
        let dy = a[1] - b[1];
        let dz = a[2] - b[2];
        acc += (dx * dx + dy * dy + dz * dz).sqrt();
    }
    acc.max(1e-3)
}

/// Chord length along V (sum of distances along the centre column).
fn chord_length_v(points: &[[f32; 3]]) -> f32 {
    let mut acc = 0.0_f32;
    for k in 0..3 {
        let a = points[1 * 4 + k];
        let b = points[1 * 4 + k + 1];
        let dx = a[0] - b[0];
        let dy = a[1] - b[1];
        let dz = a[2] - b[2];
        acc += (dx * dx + dy * dy + dz * dz).sqrt();
    }
    acc.max(1e-3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_grid_4x4() -> Vec<[f32; 3]> {
        // 4×4 control net in the XZ plane at y=0 — every control point
        // sits on the patch corners, so the resulting surface is the
        // unit square. Useful as a degenerate "no-curvature" baseline.
        let mut points = Vec::with_capacity(16);
        for i in 0..4 {
            for j in 0..4 {
                let u = i as f32 / 3.0;
                let v = j as f32 / 3.0;
                points.push([u - 0.5, 0.0, v - 0.5]);
            }
        }
        points
    }

    #[test]
    fn empty_when_points_count_wrong() {
        let m = bezier_patch_mesh(&[[0.0; 3]; 15], 4, 4, UvMode::Fit);
        assert!(m.positions.is_empty());
    }

    #[test]
    fn corner_vertices_match_corner_control_points() {
        // For any bicubic Bézier, P(0,0)=P00, P(1,0)=P30, P(0,1)=P03,
        // P(1,1)=P33. This is the defining property of the basis at the
        // endpoints — tessellation density doesn't matter.
        let points = flat_grid_4x4();
        let m = bezier_patch_mesh(&points, 6, 6, UvMode::Fit);
        let nu = 7usize;
        let nv = 7usize;
        let p00 = m.positions[0];                    // u=0, v=0
        let p10 = m.positions[(nu - 1) * nv];        // u=1, v=0
        let p01 = m.positions[nv - 1];               // u=0, v=1
        let p11 = m.positions[(nu - 1) * nv + nv - 1]; // u=1, v=1
        approx_eq(p00, points[0]);
        approx_eq(p10, points[3 * 4]);
        approx_eq(p01, points[3]);
        approx_eq(p11, points[3 * 4 + 3]);
    }

    #[test]
    fn flat_grid_yields_planar_surface() {
        let points = flat_grid_4x4();
        let m = bezier_patch_mesh(&points, 4, 4, UvMode::Fit);
        for p in &m.positions {
            assert!(p[1].abs() < 1e-5, "expected y=0, got {}", p[1]);
        }
    }

    #[test]
    fn bulged_centre_lifts_middle() {
        // Lift just the four interior control points to y=1 — the centre
        // of the patch should rise off the plane while the corners stay
        // pinned at y=0.
        let mut points = flat_grid_4x4();
        for i in 1..3 {
            for j in 1..3 {
                points[i * 4 + j][1] = 1.0;
            }
        }
        let m = bezier_patch_mesh(&points, 8, 8, UvMode::Fit);
        let nu = 9usize;
        let nv = 9usize;
        let centre = m.positions[(nu / 2) * nv + nv / 2];
        // The centre lifts but never reaches y=1 (Bézier basis weights at
        // (0.5, 0.5) interior are 9/16 each, max patch height ≈ 0.5625).
        assert!(centre[1] > 0.4 && centre[1] < 0.65, "centre y unexpected: {}", centre[1]);
        // Corners stay at y=0.
        assert!(m.positions[0][1].abs() < 1e-5);
        assert!(m.positions[(nu - 1) * nv + nv - 1][1].abs() < 1e-5);
    }

    fn approx_eq(a: [f32; 3], b: [f32; 3]) {
        for k in 0..3 {
            assert!(
                (a[k] - b[k]).abs() < 1e-4,
                "axis {k}: {} vs {}",
                a[k],
                b[k],
            );
        }
    }
}
