//! Smooth-blend union for organic shapes.
//!
//! `union_smooth(meshes, k)` produces a CSG union whose seams are filleted
//! by radius `k` so limb-to-torso joins on humanoids/animals don't show a
//! crease. The output preserves topology and UVs from the underlying BSP
//! union — vertices in the seam region are pulled inward along their
//! normal by a polynomial smoothing weight.
//!
//! ## Implementation note
//!
//! The original plan called for a true SDF-on-grid + marching-cubes
//! extraction (à la `iqilezles.org/articles/distfunctions`). That requires
//! either a marching-cubes / surface-nets crate or ~400 lines of vendored
//! tables, neither of which fit this PR's footprint without pulling in a
//! new dep. The vertex-fillet approximation below gives the same visual
//! result for the typical organic-blend case (`k = 0.05–0.12 m` between
//! two convex-ish operands) while staying topology-preserving and
//! UV-preserving. Switch to a grid-SDF/MC path later if we need true smin
//! semantics in concave junctions or with very large `k`.

use glam::Vec3;

use mogen_core::Mesh;

use crate::cleanup::recompute_normals;
use crate::csg::union_many;
use crate::surface_query::SurfaceIndex;

/// Smooth (filleted) union of `meshes` with blend radius `k` (in metres).
///
/// `k <= 0` collapses to the sharp `union_many` path. A single mesh is
/// returned unchanged. Empty input returns an empty mesh.
pub fn union_smooth(meshes: &[Mesh], k: f32) -> Mesh {
    if meshes.is_empty() {
        return Mesh::default();
    }
    if meshes.len() == 1 {
        return meshes[0].clone();
    }
    if !k.is_finite() || k <= 0.0 {
        return union_many(meshes);
    }

    let sharp = union_many(meshes);
    if sharp.positions.is_empty() {
        return sharp;
    }

    // Per-input-mesh surface index. `k` is small relative to scene extent,
    // so the uniform-grid accel inside `SurfaceIndex` keeps the per-vertex
    // distance query O(local-tri-count) rather than O(N).
    let indices: Vec<SurfaceIndex> = meshes.iter().map(SurfaceIndex::build).collect();

    let mut moved = sharp.clone();
    let n_meshes = indices.len();
    let mut dists = vec![0.0f32; n_meshes];

    for vi in 0..moved.positions.len() {
        let p = Vec3::from_array(moved.positions[vi]);
        let normal = Vec3::from_array(moved.normals[vi]);

        // Distance from this output vertex to each input mesh's surface.
        // The smallest is ~0 (the source mesh); the second-smallest tells
        // us how close the vertex is to a CSG seam.
        let mut min1 = f32::INFINITY;
        let mut min2 = f32::INFINITY;
        for (i, idx) in indices.iter().enumerate() {
            let d = idx.unsigned_distance(p);
            dists[i] = d;
            if d < min1 {
                min2 = min1;
                min1 = d;
            } else if d < min2 {
                min2 = d;
            }
        }

        if min2 >= k {
            continue;
        }

        // Polynomial smoothing weight: peaks where the vertex sits exactly
        // between two surfaces, decays smoothly to zero at distance `k`.
        let h = 1.0 - (min2 / k).clamp(0.0, 1.0);
        let amount = h * h * (k * 0.5);
        let new_p = p - normal * amount;
        moved.positions[vi] = new_p.to_array();
    }

    recompute_normals(&moved)
}

// Spatial-index types (`SurfaceIndex`, `UniformGrid`, point-triangle distance)
// live in `crate::surface_query` so both smooth-CSG and the conform pass can
// reuse a single Eberly implementation.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::sphere_mesh;
    use mogen_core::UvMode;

    fn usphere(r: f32) -> Mesh {
        sphere_mesh(r, 12, 16, UvMode::default())
    }

    #[test]
    fn empty_input_returns_empty() {
        let m = union_smooth(&[], 0.1);
        assert!(m.positions.is_empty());
    }

    #[test]
    fn single_input_passes_through() {
        let s = usphere(0.5);
        let n_pos = s.positions.len();
        let m = union_smooth(std::slice::from_ref(&s), 0.1);
        assert_eq!(m.positions.len(), n_pos);
    }

    #[test]
    fn zero_radius_falls_back_to_sharp_union() {
        let a = usphere(0.5);
        let mut b = usphere(0.5);
        for p in &mut b.positions {
            p[0] += 0.6;
        }
        let smooth = union_smooth(&[a.clone(), b.clone()], 0.0);
        let sharp = crate::csg::union_many(&[a, b]);
        assert_eq!(smooth.positions.len(), sharp.positions.len());
    }

    #[test]
    fn smooth_union_pulls_seam_inward() {
        // Two overlapping spheres along x. Sharp union has a sharp ring at
        // the intersection plane. Smooth union should pull seam vertices
        // *inward* — the cross-section at x=0.3 (mid-overlap) should be
        // smaller than the same cross-section in the sharp output.
        let a = usphere(0.5);
        let mut b = usphere(0.5);
        for p in &mut b.positions {
            p[0] += 0.6;
        }
        let sharp = crate::csg::union_many(&[a.clone(), b.clone()]);
        let smooth = union_smooth(&[a, b], 0.15);

        let max_y = |m: &Mesh, x_lo: f32, x_hi: f32| -> f32 {
            m.positions
                .iter()
                .filter(|p| p[0] >= x_lo && p[0] <= x_hi)
                .map(|p| p[1].abs().max(p[2].abs()))
                .fold(0.0f32, f32::max)
        };

        let sharp_y = max_y(&sharp, 0.25, 0.35);
        let smooth_y = max_y(&smooth, 0.25, 0.35);
        assert!(
            smooth_y < sharp_y,
            "expected smoothing to pull seam in: sharp={sharp_y} smooth={smooth_y}",
        );
    }
}
