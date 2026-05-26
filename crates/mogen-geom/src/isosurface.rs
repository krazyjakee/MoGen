//! Mesh extraction from a `blob` SDF field via fast surface nets.
//!
//! Pipeline: take an ordered slice of `BlobChild`s, compute a padded AABB
//! that encloses every additive child, sample the smooth-blended field on a
//! cubic-voxel grid sized to the requested `resolution`, then feed the
//! sample buffer to `fast_surface_nets::surface_nets`. The buffer's
//! voxel-space positions are mapped back to world space using the grid
//! transform and the result is wrapped in a `Mesh` with planar UVs (XZ
//! projection — adequate for the bbox-projected texturing the LLM-driven
//! material path already uses).
//!
//! Surface nets (vs. classic marching cubes) gives noticeably better
//! topology for organic shapes — quads-as-tris rather than tetrahedral
//! slivers — which means the `subdivide=N` Loop pass smooths cleanly
//! instead of fighting the staircased seams MC tends to leave.

use fast_surface_nets::ndshape::{RuntimeShape, Shape};
use fast_surface_nets::{surface_nets, SurfaceNetsBuffer};
use glam::Vec3;
use mogen_core::Mesh;

use crate::sdf::{blob_aabb, evaluate_field, BlobChild};

/// Maximum total voxel count enforced before allocation. At ~3 bytes/voxel
/// of working memory (sdf buffer + surface-nets internals) this caps RAM at
/// roughly 50 MB and CPU at well under a second on a modern desktop. The
/// DSL-side validator clamps `resolution` per axis so callers shouldn't hit
/// this — it's a defensive backstop against pathological AABBs.
const MAX_TOTAL_VOXELS: u64 = 16_000_000;

/// Mesh the implicit field defined by `children` using surface nets at the
/// requested grid `resolution` (voxels along the longest world-space axis).
/// `blend` is the smooth-min radius used by `evaluate_field`.
///
/// Returns an empty mesh if `children` is empty, the AABB collapses, or
/// surface nets finds no zero-crossing inside the grid.
pub fn blob_to_mesh(children: &[BlobChild], blend: f32, resolution: u32) -> Mesh {
    if children.is_empty() {
        return Mesh::default();
    }
    let blend = blend.max(0.0);

    // First pass: AABB of additive children with a `3*blend` pad so smooth
    // bulges have room. Subtractive children are evaluated inside this same
    // grid (they carve, they can't grow the silhouette).
    let initial_pad = 3.0 * blend;
    let (lo0, hi0) = match blob_aabb(children, initial_pad) {
        Some(b) => b,
        None => return Mesh::default(),
    };
    let extent0 = hi0 - lo0;
    let max_axis = extent0.x.max(extent0.y).max(extent0.z);
    if max_axis <= 0.0 {
        return Mesh::default();
    }

    // Second pass: bump pad so we always have ≥2 voxels of clearance between
    // the surface and the grid boundary (surface-nets normals at the very
    // edge of the field are unreliable). Recompute the AABB with the larger
    // pad so the voxel grid actually contains that clearance.
    let res = resolution.max(8);
    let voxel0 = max_axis / (res as f32 - 1.0);
    let pad = initial_pad.max(2.0 * voxel0);
    let (lo, hi) = blob_aabb(children, pad).expect("AABB stable after pad");
    let extent = hi - lo;
    let max_axis = extent.x.max(extent.y).max(extent.z);
    let voxel = max_axis / (res as f32 - 1.0);

    let dim = |e: f32| -> u32 { ((e / voxel).ceil() as u32 + 1).max(2) };
    let nx = dim(extent.x);
    let ny = dim(extent.y);
    let nz = dim(extent.z);

    let total = nx as u64 * ny as u64 * nz as u64;
    if total > MAX_TOTAL_VOXELS {
        // Defensive backstop: rather than allocating multi-GB of voxels,
        // bail with an empty mesh. The DSL validator already caps
        // `resolution`, so reaching this means the AABB itself was
        // pathological — silently producing nothing is preferable to OOM.
        return Mesh::default();
    }

    let shape = RuntimeShape::<u32, 3>::new([nx, ny, nz]);
    let total_usize = total as usize;
    let mut sdf = vec![0.0_f32; total_usize];

    for i in 0..total_usize as u32 {
        let [ix, iy, iz] = shape.delinearize(i);
        let p = Vec3::new(
            lo.x + ix as f32 * voxel,
            lo.y + iy as f32 * voxel,
            lo.z + iz as f32 * voxel,
        );
        sdf[i as usize] = evaluate_field(children, p, blend);
    }

    let mut buf = SurfaceNetsBuffer::default();
    surface_nets(&sdf, &shape, [0, 0, 0], [nx - 1, ny - 1, nz - 1], &mut buf);

    if buf.indices.is_empty() {
        return Mesh::default();
    }

    let positions: Vec<[f32; 3]> = buf
        .positions
        .iter()
        .map(|p| {
            [
                lo.x + p[0] * voxel,
                lo.y + p[1] * voxel,
                lo.z + p[2] * voxel,
            ]
        })
        .collect();

    // Planar UVs from world-space XZ projection. Blob outputs are typically
    // textured with a bbox-projected material (the LLM material pipeline
    // doesn't try to unwrap a marching/surface-nets result), so an XZ planar
    // mapping is the same convention as `plane` and the lower face of every
    // bbox-projected primitive — consistent enough that mat="bone" on a
    // skull lands the right way up.
    let inv_w = 1.0 / extent.x.max(1e-6);
    let inv_d = 1.0 / extent.z.max(1e-6);
    let uvs: Vec<[f32; 2]> = positions
        .iter()
        .map(|p| [(p[0] - lo.x) * inv_w, (p[2] - lo.z) * inv_d])
        .collect();

    Mesh {
        positions,
        normals: buf.normals,
        uvs,
        indices: buf.indices,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdf::{BlobChild, SdfOp, SdfPrim};
    use glam::Mat4;

    #[test]
    fn empty_children_returns_empty_mesh() {
        let m = blob_to_mesh(&[], 0.1, 32);
        assert!(m.positions.is_empty());
    }

    #[test]
    fn single_sphere_produces_roughly_spherical_mesh() {
        let c = BlobChild::new(
            SdfPrim::Sphere { radius: 0.5 },
            SdfOp::Add,
            Mat4::IDENTITY,
        );
        let m = blob_to_mesh(&[c], 0.0, 48);
        assert!(!m.positions.is_empty(), "expected mesh output");
        let max_r = m
            .positions
            .iter()
            .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
            .fold(0.0_f32, f32::max);
        let min_r = m
            .positions
            .iter()
            .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
            .fold(f32::INFINITY, f32::min);
        assert!((max_r - 0.5).abs() < 0.05, "max radius off: {max_r}");
        assert!((min_r - 0.5).abs() < 0.05, "min radius off: {min_r}");
    }

    #[test]
    fn two_overlapping_spheres_smooth_into_one_blob() {
        let a = BlobChild::new(
            SdfPrim::Sphere { radius: 0.5 },
            SdfOp::Add,
            Mat4::from_translation(Vec3::new(-0.3, 0.0, 0.0)),
        );
        let b = BlobChild::new(
            SdfPrim::Sphere { radius: 0.5 },
            SdfOp::Add,
            Mat4::from_translation(Vec3::new(0.3, 0.0, 0.0)),
        );
        let m = blob_to_mesh(&[a, b], 0.15, 48);
        assert!(!m.positions.is_empty());
        let max_x = m.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
        let min_x = m.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
        assert!(max_x > 0.7, "expected blob to extend past one sphere, got max_x={max_x}");
        assert!(min_x < -0.7, "expected blob to extend past one sphere, got min_x={min_x}");
    }

    #[test]
    fn subtract_carves_a_cavity() {
        let outer = BlobChild::new(
            SdfPrim::Sphere { radius: 0.8 },
            SdfOp::Add,
            Mat4::IDENTITY,
        );
        let hole = BlobChild::new(
            SdfPrim::Sphere { radius: 0.3 },
            SdfOp::Subtract,
            Mat4::from_translation(Vec3::new(0.6, 0.0, 0.0)),
        );
        // Without the carver, no vertex should sit inside the cavity
        // (sphere centred at (0.6, 0, 0) with r=0.3 → exclusion zone is the
        // ball within 0.3 of (0.6, 0, 0)).
        let m = blob_to_mesh(&[outer, hole], 0.0, 64);
        assert!(!m.positions.is_empty());
        let inside_cavity = m.positions.iter().filter(|p| {
            let d = ((p[0] - 0.6).powi(2) + p[1].powi(2) + p[2].powi(2)).sqrt();
            d < 0.2  // well inside the cavity carver
        }).count();
        // Surface-nets places the cavity rim somewhere outside r=0.2 from
        // the carver centre, so no vertex should be deep inside it.
        assert_eq!(inside_cavity, 0, "found vertices inside carved cavity");
    }
}
