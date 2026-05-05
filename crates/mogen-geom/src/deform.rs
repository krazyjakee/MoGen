//! Mesh deformation kernels for variety modifiers (`bend`, `twist`, `taper`,
//! `droop`, `noise`, `jitter`, `faceted`). Each kernel mutates `mesh.positions`
//! in place and leaves normals stale — callers must follow with
//! `recompute_normals` (and optionally `weld_vertices`) once all kernels have
//! run. The exception is `split_for_facets`, which rebuilds the mesh outright.
//!
//! All angles are in radians at this layer; the lowering pass converts user
//! degrees with `.to_radians()` before calling in.

use glam::Vec3;

use mogen_core::Mesh;

/// Bend the mesh around `bend_axis` (0=X, 1=Y, 2=Z) along the perpendicular
/// `length_axis`. The bend is arc-length-preserving: the point at the base of
/// the length axis stays put, and the rest of the mesh follows an arc whose
/// total subtended angle is `total_rad`. The third axis (perpendicular to
/// both) is the direction the column tilts toward; the bend's sign follows
/// `bend_axis × length_axis` (right-hand rule).
pub fn bend(mesh: &mut Mesh, bend_axis: usize, length_axis: usize, total_rad: f32) {
    if total_rad.abs() < 1e-6 || mesh.positions.is_empty() {
        return;
    }
    debug_assert!(bend_axis < 3 && length_axis < 3 && bend_axis != length_axis);
    let perp_axis = 3 - bend_axis - length_axis;

    let (lmin, lmax) = axis_range(&mesh.positions, length_axis);
    let length_extent = (lmax - lmin).max(1e-6);
    let r = length_extent / total_rad;

    for p in mesh.positions.iter_mut() {
        let h = p[length_axis] - lmin;
        let phi = total_rad * h / length_extent;
        let sin_p = phi.sin();
        let cos_p = phi.cos();
        // Slice centerline position in the (length, perp) plane:
        let center_along = r * sin_p;
        let center_perp = r * (1.0 - cos_p);
        // Local perp direction at this slice (perpendicular to the bent
        // tangent), in the (length, perp) plane:
        let local_perp_along = -sin_p;
        let local_perp_perp = cos_p;
        let perp_orig = p[perp_axis];
        let new_along = center_along + perp_orig * local_perp_along;
        let new_perp = center_perp + perp_orig * local_perp_perp;
        p[length_axis] = lmin + new_along;
        p[perp_axis] = new_perp;
        // p[bend_axis] is preserved by the rotation around bend_axis.
    }
}

/// Twist the mesh around the Y axis. Vertices rotate by an angle proportional
/// to their height (y - y_min) / length: 0 at the base, `total_rad` at the
/// top. Other axes follow the same pattern by permuting indices, but v1 only
/// exposes the Y form because that's the dominant case.
pub fn twist_y(mesh: &mut Mesh, total_rad: f32) {
    if total_rad.abs() < 1e-6 || mesh.positions.is_empty() {
        return;
    }
    let (lmin, lmax) = axis_range(&mesh.positions, 1);
    let length_extent = (lmax - lmin).max(1e-6);
    for p in mesh.positions.iter_mut() {
        let h = (p[1] - lmin) / length_extent;
        let phi = total_rad * h;
        let s = phi.sin();
        let c = phi.cos();
        let x = p[0];
        let z = p[2];
        p[0] = x * c - z * s;
        p[2] = x * s + z * c;
    }
}

/// Linear taper along Y. Scale of XZ varies from 1.0 at y_min to `ratio` at
/// y_max. `ratio < 1` shrinks toward the top (cone-like); `ratio > 1` flares.
pub fn taper(mesh: &mut Mesh, ratio: f32) {
    if (ratio - 1.0).abs() < 1e-6 || mesh.positions.is_empty() {
        return;
    }
    let r = ratio.max(0.0);
    let (lmin, lmax) = axis_range(&mesh.positions, 1);
    let length_extent = (lmax - lmin).max(1e-6);
    for p in mesh.positions.iter_mut() {
        let h = (p[1] - lmin) / length_extent;
        let s = 1.0 + (r - 1.0) * h;
        p[0] *= s;
        p[2] *= s;
    }
}

/// Quadratic gravity-style sag along -Y. Vertex `y` drops by
/// `amount * length * h^2` where `h = (y - y_min) / length`. The base stays
/// fixed; the top sinks by `amount * length`.
pub fn droop(mesh: &mut Mesh, amount: f32) {
    if amount.abs() < 1e-6 || mesh.positions.is_empty() {
        return;
    }
    let (lmin, lmax) = axis_range(&mesh.positions, 1);
    let length_extent = (lmax - lmin).max(1e-6);
    for p in mesh.positions.iter_mut() {
        let h = (p[1] - lmin) / length_extent;
        p[1] -= amount * length_extent * h * h;
    }
}

/// Coherent value-noise displacement along the vertex normal. Vertices in the
/// same cell of a quantization grid share a random value, producing blobby
/// "rock"-style bumps. Frequency is derived from the mesh's smallest AABB
/// extent so a small mesh doesn't get over-detailed.
pub fn noise(mesh: &mut Mesh, amount: f32, seed: u32) {
    if amount.abs() < 1e-6 || mesh.positions.is_empty() {
        return;
    }
    debug_assert_eq!(mesh.positions.len(), mesh.normals.len());
    let extent = aabb_extent(&mesh.positions);
    let min_extent = extent.x.min(extent.y).min(extent.z).max(1e-4);
    // ~5 cells across the smallest dim → coarse blobs; clamp so dense meshes
    // don't get a per-vertex pattern from quantization aliasing.
    let frequency = 5.0 / min_extent;
    let displacement_scale = min_extent * 0.5;
    for (p, n) in mesh.positions.iter_mut().zip(mesh.normals.iter()) {
        let qx = (p[0] * frequency).round() as i32;
        let qy = (p[1] * frequency).round() as i32;
        let qz = (p[2] * frequency).round() as i32;
        let r = cell_noise(qx, qy, qz, seed);
        let d = amount.clamp(0.0, 1.0) * displacement_scale * r;
        p[0] += n[0] * d;
        p[1] += n[1] * d;
        p[2] += n[2] * d;
    }
}

/// Per-vertex random displacement along the vertex normal. Unlike `noise`,
/// nearby vertices get uncorrelated values, producing high-frequency "jagged"
/// detail. Magnitude is scaled by the smallest AABB extent.
pub fn jitter(mesh: &mut Mesh, amount: f32, seed: u32) {
    if amount.abs() < 1e-6 || mesh.positions.is_empty() {
        return;
    }
    debug_assert_eq!(mesh.positions.len(), mesh.normals.len());
    let extent = aabb_extent(&mesh.positions);
    let min_extent = extent.x.min(extent.y).min(extent.z).max(1e-4);
    let displacement_scale = min_extent * 0.25;
    let mut rng = seed.max(1);
    for (p, n) in mesh.positions.iter_mut().zip(mesh.normals.iter()) {
        let r = rand_pm(&mut rng);
        let d = amount.clamp(0.0, 1.0) * displacement_scale * r;
        p[0] += n[0] * d;
        p[1] += n[1] * d;
        p[2] += n[2] * d;
    }
}

/// Rebuild the mesh so each triangle has its own three vertices with the
/// face's flat normal — produces a faceted, low-poly look. UVs are preserved
/// per-corner. Joints/weights are dropped; faceted shading on a skinned mesh
/// breaks vertex sharing in ways the skinning pipeline can't track, so callers
/// gate this on non-skinned meshes (the deform lowering pass does this).
pub fn split_for_facets(mesh: &Mesh) -> Mesh {
    let n_tris = mesh.indices.len() / 3;
    let mut positions = Vec::with_capacity(n_tris * 3);
    let mut normals = Vec::with_capacity(n_tris * 3);
    let has_uvs = mesh.has_uvs();
    let mut uvs = if has_uvs { Vec::with_capacity(n_tris * 3) } else { Vec::new() };
    let mut indices = Vec::with_capacity(n_tris * 3);
    for tri in mesh.indices.chunks_exact(3) {
        let a = tri[0] as usize;
        let b = tri[1] as usize;
        let c = tri[2] as usize;
        let pa = Vec3::from_array(mesh.positions[a]);
        let pb = Vec3::from_array(mesh.positions[b]);
        let pc = Vec3::from_array(mesh.positions[c]);
        let face_n = (pb - pa).cross(pc - pa).normalize_or_zero();
        let n_arr = [face_n.x, face_n.y, face_n.z];
        positions.push(mesh.positions[a]);
        positions.push(mesh.positions[b]);
        positions.push(mesh.positions[c]);
        normals.push(n_arr);
        normals.push(n_arr);
        normals.push(n_arr);
        if has_uvs {
            uvs.push(mesh.uvs[a]);
            uvs.push(mesh.uvs[b]);
            uvs.push(mesh.uvs[c]);
        }
        let base = (positions.len() - 3) as u32;
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
    Mesh {
        positions,
        normals,
        uvs,
        indices,
        ..Default::default()
    }
}

fn axis_range(positions: &[[f32; 3]], axis: usize) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for p in positions {
        let v = p[axis];
        if v < lo {
            lo = v;
        }
        if v > hi {
            hi = v;
        }
    }
    (lo, hi)
}

fn aabb_extent(positions: &[[f32; 3]]) -> Vec3 {
    let (xmin, xmax) = axis_range(positions, 0);
    let (ymin, ymax) = axis_range(positions, 1);
    let (zmin, zmax) = axis_range(positions, 2);
    Vec3::new(xmax - xmin, ymax - ymin, zmax - zmin)
}

/// Linear-congruential RNG returning a [-1, 1] float. Same generator the
/// `branch` primitive uses (`crates/mogen-dsl/src/lower/branch.rs`); duplicated
/// here so this crate stays self-contained.
fn rand_pm(state: &mut u32) -> f32 {
    *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
    let bits = (*state >> 8) & 0x00FF_FFFF;
    (bits as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
}

/// Hash three integer cell coords with a seed into a [-1, 1] float. Cheap
/// integer mixer (xorshift-flavoured) — not cryptographic, but stable across
/// runs which is what determinism needs.
fn cell_noise(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    let mut h: u32 = seed.wrapping_add(0x9E3779B9);
    h = h.wrapping_add(x as u32).wrapping_mul(0x85EBCA6B);
    h ^= h >> 13;
    h = h.wrapping_add(y as u32).wrapping_mul(0xC2B2AE35);
    h ^= h >> 16;
    h = h.wrapping_add(z as u32).wrapping_mul(0x27D4EB2F);
    h ^= h >> 15;
    let bits = (h >> 8) & 0x00FF_FFFF;
    (bits as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{box_mesh, cylinder_mesh};
    use mogen_core::UvMode;

    fn aabb(positions: &[[f32; 3]]) -> (Vec3, Vec3) {
        let (xmin, xmax) = axis_range(positions, 0);
        let (ymin, ymax) = axis_range(positions, 1);
        let (zmin, zmax) = axis_range(positions, 2);
        (Vec3::new(xmin, ymin, zmin), Vec3::new(xmax, ymax, zmax))
    }

    #[test]
    fn bend_zero_is_noop() {
        let mut a = cylinder_mesh(0.1, 1.0, 12, UvMode::Fit);
        let before = a.positions.clone();
        bend(&mut a, 0, 1, 0.0);
        assert_eq!(a.positions, before);
    }

    #[test]
    fn bend_preserves_base_vertices() {
        // Bend the base of a column should leave y_min vertices untouched.
        let mut m = cylinder_mesh(0.1, 1.0, 12, UvMode::Fit);
        let (mn_before, _) = aabb(&m.positions);
        let base_y = mn_before.y;
        let base_indices: Vec<usize> = m
            .positions
            .iter()
            .enumerate()
            .filter(|(_, p)| (p[1] - base_y).abs() < 1e-5)
            .map(|(i, _)| i)
            .collect();
        let base_before: Vec<[f32; 3]> = base_indices.iter().map(|&i| m.positions[i]).collect();
        bend(&mut m, 0, 1, std::f32::consts::FRAC_PI_4);
        for (i, &orig) in base_indices.iter().zip(base_before.iter()) {
            let now = m.positions[*i];
            for k in 0..3 {
                assert!(
                    (now[k] - orig[k]).abs() < 1e-4,
                    "base vertex {i} moved on axis {k}: {} → {}",
                    orig[k],
                    now[k]
                );
            }
        }
    }

    #[test]
    fn twist_zero_is_noop() {
        let mut a = cylinder_mesh(0.1, 1.0, 12, UvMode::Fit);
        let before = a.positions.clone();
        twist_y(&mut a, 0.0);
        assert_eq!(a.positions, before);
    }

    #[test]
    fn taper_one_is_noop() {
        let mut a = cylinder_mesh(0.5, 1.0, 12, UvMode::Fit);
        let before = a.positions.clone();
        taper(&mut a, 1.0);
        assert_eq!(a.positions, before);
    }

    #[test]
    fn taper_half_shrinks_top() {
        let mut a = cylinder_mesh(0.5, 1.0, 12, UvMode::Fit);
        taper(&mut a, 0.5);
        // The top ring (y ≈ 0.5) should have radius ~0.25; the bottom ~0.5.
        let mut top_r = 0.0_f32;
        let mut bot_r = 0.0_f32;
        for p in &a.positions {
            let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
            if p[1] > 0.4 {
                top_r = top_r.max(r);
            } else if p[1] < -0.4 {
                bot_r = bot_r.max(r);
            }
        }
        assert!((top_r - 0.25).abs() < 1e-3, "expected top radius ~0.25, got {top_r}");
        assert!((bot_r - 0.5).abs() < 1e-3, "expected bottom radius ~0.5, got {bot_r}");
    }

    #[test]
    fn droop_pulls_top_down() {
        let mut a = cylinder_mesh(0.1, 1.0, 12, UvMode::Fit);
        let (_, mx_before) = aabb(&a.positions);
        droop(&mut a, 0.4);
        let (_, mx_after) = aabb(&a.positions);
        assert!(
            mx_after.y < mx_before.y - 0.3,
            "expected top y to drop by ~0.4, before={} after={}",
            mx_before.y,
            mx_after.y
        );
    }

    #[test]
    fn noise_deterministic_for_same_seed() {
        let m = box_mesh([1.0, 1.0, 1.0], UvMode::Fit);
        let mut a = m.clone();
        let mut b = m.clone();
        noise(&mut a, 0.3, 7);
        noise(&mut b, 0.3, 7);
        assert_eq!(a.positions, b.positions);
    }

    #[test]
    fn noise_differs_across_seeds() {
        let m = box_mesh([1.0, 1.0, 1.0], UvMode::Fit);
        let mut a = m.clone();
        let mut b = m.clone();
        noise(&mut a, 0.3, 1);
        noise(&mut b, 0.3, 2);
        assert_ne!(a.positions, b.positions);
    }

    #[test]
    fn jitter_zero_is_noop() {
        let mut a = box_mesh([1.0, 1.0, 1.0], UvMode::Fit);
        let before = a.positions.clone();
        jitter(&mut a, 0.0, 5);
        assert_eq!(a.positions, before);
    }

    #[test]
    fn split_for_facets_triples_vertex_count() {
        let m = box_mesh([1.0, 1.0, 1.0], UvMode::Fit);
        let n_tris = m.indices.len() / 3;
        let f = split_for_facets(&m);
        assert_eq!(f.positions.len(), n_tris * 3);
        assert_eq!(f.normals.len(), n_tris * 3);
        assert_eq!(f.indices.len(), n_tris * 3);
    }
}
