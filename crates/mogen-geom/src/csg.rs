//! Boolean CSG over triangle meshes — backed by Google's Manifold library
//! via the `manifold-csg` Rust bindings (zmerlynn/manifold-csg). The
//! `manifold-csg` crate is shared between the desktop build (CMake-built
//! Manifold linked natively) and the wasm build (same Manifold sources
//! cross-compiled via `wasm-cxx-shim` for `wasm32-unknown-unknown`); see
//! the workspace README for the LLVM 20+ host requirement on the wasm leg.
//!
//! Manifold guarantees watertight output for any pair of valid manifold
//! inputs (including curved-vs-curved cases that the previous BSP/csg.js
//! implementation dropped fragments on). The public API here is unchanged
//! from the BSP era — `union`/`difference`/`intersect` and the `_many`
//! variants — so callers in `mogen-dsl` and `mogen-export` need no changes.
//!
//! ## Input requirements
//!
//! Manifold requires each input to be topologically manifold: every edge
//! shared by exactly two triangles using the same vertex indices. Our
//! primitives often emit per-face vertex copies for sharp edges (a cube
//! has 24 vertices, 4 per face), so we run [`weld_vertices`] on every
//! operand before handing it to Manifold. This is the same epsilon-weld
//! used by `clean_csg_output`.
//!
//! ## Vertex properties
//!
//! When both operands carry UVs *and* those UVs are continuous across
//! coincident-position vertex copies, we send `[x, y, z, u, v]` per vertex
//! (`n_props = 5`) and Manifold interpolates the UVs linearly across boolean
//! intersection cuts. Most of our primitives emit per-face vertex copies with
//! face-specific UVs (a cube corner has three different UVs, a cylinder seam
//! has two), so the welding step would average those into garbage that the
//! boolean then smears across newly-cut triangles. When we detect that
//! pattern on either operand we drop UVs for the boolean and let the cleanup
//! pass synthesise triplanar UVs on the result.
//!
//! Per-vertex normals are not passed through — Manifold's boolean produces
//! new vertices on intersection curves whose normals would need to come
//! from the analytic surface, not interpolation. `clean_csg_output` calls
//! `recompute_normals` to rebuild face-averaged normals after the boolean.
//!
//! ## Errors
//!
//! `Manifold::from_mesh_f32` returns a `Result`. A non-manifold input
//! (e.g. an open primitive, or a mesh whose welding failed to close
//! coincident-vertex pairs) becomes an `Err`. We treat that as a bug in
//! the caller's primitive and panic with the offending status — silent
//! fallback would mask geometry corruption.

use std::collections::HashMap;

use manifold_csg::Manifold;

use mogen_core::Mesh;

use crate::cleanup::{recompute_normals, weld_vertices};

/// Same epsilon as `clean_csg_output` so a CSG operand that already went
/// through the cleanup pass doesn't get re-welded with a different threshold.
const WELD_EPS: f32 = 1e-4;

/// True when the mesh has two or more vertices at (within `eps`) the same
/// position carrying meaningfully different UVs — the per-face vertex copies
/// of a cube, the wrap seam of a cylinder, the rim where a side wall meets a
/// cap. Welding such verts averages their UVs, which is fine for normals
/// (smoothing) but garbage for textures: the boolean then linearly
/// interpolates that garbage across newly-cut triangles. When this is true,
/// callers fall back to triplanar UV synthesis on the result.
fn has_uv_seams(mesh: &Mesh, eps: f32) -> bool {
    if !mesh.has_uvs() {
        return false;
    }
    let scale = 1.0 / eps.max(1e-9);
    let uv_eps_sq = 1e-6_f32; // ~1e-3 in either component
    let mut buckets: HashMap<[i64; 3], u32> = HashMap::new();
    for (i, p) in mesh.positions.iter().enumerate() {
        let key = [
            (p[0] * scale).round() as i64,
            (p[1] * scale).round() as i64,
            (p[2] * scale).round() as i64,
        ];
        match buckets.get(&key) {
            None => {
                buckets.insert(key, i as u32);
            }
            Some(&first) => {
                let p1 = mesh.positions[first as usize];
                let dx = p[0] - p1[0];
                let dy = p[1] - p1[1];
                let dz = p[2] - p1[2];
                if dx * dx + dy * dy + dz * dz > eps * eps {
                    continue;
                }
                let uv0 = mesh.uvs[first as usize];
                let uv1 = mesh.uvs[i];
                let du = uv0[0] - uv1[0];
                let dv = uv0[1] - uv1[1];
                if du * du + dv * dv > uv_eps_sq {
                    return true;
                }
            }
        }
    }
    false
}

/// Convert a `Mesh` into the `(vert_props, n_props, tri_indices)` triple
/// Manifold expects. UVs are interleaved when present and continuous across
/// coincident-position copies, so they survive the boolean. When the mesh has
/// UV seams (per-face vertex copies, cylinder wrap seams, cap-to-wall rims),
/// welding would average distinct face UVs into garbage; we fall back to
/// position-only and let `clean_csg_output` synthesise triplanar UVs.
fn to_manifold_input(mesh: &Mesh) -> (Vec<f32>, usize, Vec<u32>) {
    let send_uvs = mesh.has_uvs() && !has_uv_seams(mesh, WELD_EPS);
    let welded = weld_vertices(mesh, WELD_EPS);
    let n_props = if send_uvs { 5 } else { 3 };
    let mut vert_props = Vec::with_capacity(welded.positions.len() * n_props);
    for (i, p) in welded.positions.iter().enumerate() {
        vert_props.extend_from_slice(p);
        if send_uvs {
            vert_props.extend_from_slice(&welded.uvs[i]);
        }
    }
    (vert_props, n_props, welded.indices)
}

fn try_build_manifold(mesh: &Mesh) -> Option<(Manifold, usize)> {
    let (vert_props, n_props, tri_indices) = to_manifold_input(mesh);
    Manifold::from_mesh_f32(&vert_props, n_props, &tri_indices)
        .ok()
        .map(|m| (m, n_props))
}

fn build_manifold(mesh: &Mesh) -> (Manifold, usize) {
    try_build_manifold(mesh)
        .expect("CSG operand is not a manifold mesh; check that the primitive is closed")
}

/// Ground-truth test for whether `mesh` can be used as a CSG operand: it runs
/// the exact same import Manifold's boolean ops perform, so a `true` here
/// guarantees [`union`]/[`difference`]/[`intersect`] won't trip on this mesh.
///
/// The cheap [`crate::is_closed_manifold`] edge-incidence check
/// under-approximates this — it accepts meshes with inconsistent winding or
/// non-manifold vertex fans that Manifold then rejects. The merge pass gates
/// candidate leaves on *this* predicate so it never feeds the boolean a mesh
/// that would panic.
pub fn is_csg_manifold(mesh: &Mesh) -> bool {
    try_build_manifold(mesh).is_some()
}

/// Convert a Manifold result back to `Mesh`. UVs are recovered when the
/// Manifold carries them (`props >= 5`); otherwise the output `uvs` is empty
/// and the caller's `clean_csg_output` will assign triplanar coordinates.
/// Normals are recomputed from face geometry so the returned mesh is
/// internally consistent — callers like `union_smooth` rely on per-vertex
/// normals without needing a separate cleanup pass first.
fn from_manifold_output(m: &Manifold) -> Mesh {
    let (flat, props, tri_indices) = m.to_mesh_f32();
    let n_verts = flat.len() / props;
    let mut positions = Vec::with_capacity(n_verts);
    let mut uvs = Vec::with_capacity(if props >= 5 { n_verts } else { 0 });
    for i in 0..n_verts {
        let base = i * props;
        positions.push([flat[base], flat[base + 1], flat[base + 2]]);
        if props >= 5 {
            uvs.push([flat[base + 3], flat[base + 4]]);
        }
    }
    let raw = Mesh {
        positions,
        normals: vec![[0.0; 3]; n_verts],
        uvs,
        indices: tri_indices,
        ..Default::default()
    };
    recompute_normals(&raw)
}

fn boolean(a: &Mesh, b: &Mesh, op: Op) -> Mesh {
    let (ma, props_a) = build_manifold(a);
    let (mb, props_b) = build_manifold(b);
    boolean_inner(ma, props_a, mb, props_b, op)
}

/// Like [`boolean`] but returns `None` instead of panicking when either operand
/// is not a valid manifold. The merge pass uses this so a stray non-manifold
/// leaf degrades to "don't merge" rather than aborting the whole export.
fn try_boolean(a: &Mesh, b: &Mesh, op: Op) -> Option<Mesh> {
    let (ma, props_a) = try_build_manifold(a)?;
    let (mb, props_b) = try_build_manifold(b)?;
    Some(boolean_inner(ma, props_a, mb, props_b, op))
}

fn boolean_inner(ma: Manifold, props_a: usize, mb: Manifold, props_b: usize, op: Op) -> Mesh {
    // Only trust the result's UVs when *both* operands carried them. If the
    // operands disagree (one had UVs, one didn't) Manifold still emits a 5-prop
    // result, but the UV channel of the non-UV side is garbage; strip it and
    // let `clean_csg_output` synthesise triplanar coordinates instead.
    let keep_uvs = props_a == props_b && props_a >= 5;
    let result = match op {
        Op::Union => ma.union(&mb),
        Op::Difference => ma.difference(&mb),
        Op::Intersect => ma.intersection(&mb),
    };
    if result.is_empty() {
        return Mesh::default();
    }
    let mut m = from_manifold_output(&result);
    if !keep_uvs {
        m.uvs.clear();
    }
    m
}

#[derive(Clone, Copy)]
enum Op {
    Union,
    Difference,
    Intersect,
}

pub fn union(a: &Mesh, b: &Mesh) -> Mesh {
    boolean(a, b, Op::Union)
}

pub fn difference(a: &Mesh, b: &Mesh) -> Mesh {
    boolean(a, b, Op::Difference)
}

pub fn intersect(a: &Mesh, b: &Mesh) -> Mesh {
    boolean(a, b, Op::Intersect)
}

/// Left-fold `a` through each subsequent mesh with `difference` — i.e. subtract
/// every element of `rest` from `a`.
pub fn difference_many(a: &Mesh, rest: &[Mesh]) -> Mesh {
    rest.iter().fold(a.clone(), |acc, m| difference(&acc, m))
}

pub fn union_many(meshes: &[Mesh]) -> Mesh {
    let mut it = meshes.iter();
    let Some(first) = it.next() else {
        return Mesh::default();
    };
    it.fold(first.clone(), |acc, m| union(&acc, m))
}

/// Fallible variant of [`union_many`]: returns `None` the moment any operand
/// (or fold intermediate) fails to import as a manifold, instead of panicking.
/// Used by the export-time mesh-merge optimisation, where a non-manifold input
/// should fall back to keeping meshes separate, never crash the build.
pub fn try_union_many(meshes: &[Mesh]) -> Option<Mesh> {
    let mut it = meshes.iter();
    let first = it.next()?;
    // Validate the first operand explicitly. The fold only runs try_boolean on
    // it when there are two or more meshes; a single-element non-manifold slice
    // would otherwise bypass all validation and return Some instead of None.
    try_build_manifold(first)?;
    let mut acc = first.clone();
    for m in it {
        acc = try_boolean(&acc, m, Op::Union)?;
    }
    Some(acc)
}

pub fn intersect_many(meshes: &[Mesh]) -> Mesh {
    let mut it = meshes.iter();
    let Some(first) = it.next() else {
        return Mesh::default();
    };
    it.fold(first.clone(), |acc, m| intersect(&acc, m))
}

/// Convex hull of a 3D point cloud, returned as a clean, watertight,
/// triplanar-UV'd `Mesh`. Backs the `hull` primitive — the lossless sink for
/// arbitrary convex solids (e.g. a sheared/sloped 8-corner block) that no
/// parametric primitive captures. Fewer than 4 points, or a fully coplanar
/// set, yields a degenerate (empty) hull; callers validate the point count
/// before reaching here. UVs come from `clean_csg_output`'s per-face planar
/// pass, since a hull has no natural parameterisation.
pub fn hull_mesh(points: &[[f32; 3]]) -> Mesh {
    let pts: Vec<[f64; 3]> = points
        .iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect();
    let raw = from_manifold_output(&Manifold::hull_pts(&pts));
    crate::cleanup::clean_csg_output(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{box_mesh, cylinder_mesh};
    use glam::Vec3;
    use mogen_core::UvMode;

    fn ubox(s: [f32; 3]) -> Mesh {
        box_mesh(s, UvMode::default())
    }

    /// A tetrahedron with one face wound backwards. Every undirected edge is
    /// still shared by exactly two triangles (so `is_closed_manifold` passes),
    /// but the reversed face produces a duplicated directed halfedge, which
    /// Manifold rejects. Models the `extrude`/`spline_tube` meshes that crashed
    /// the merge pass.
    fn edge_closed_but_inconsistent() -> Mesh {
        Mesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            // Real meshes always carry normals; weld_vertices indexes them.
            normals: vec![[0.0, 0.0, 0.0]; 4],
            // Consistent winding is [0,1,2, 0,3,1, 0,2,3, 1,3,2]; the last face
            // is flipped to [1,2,3] to break orientation.
            indices: vec![0, 1, 2, 0, 3, 1, 0, 2, 3, 1, 2, 3],
            ..Default::default()
        }
    }

    #[test]
    fn is_csg_manifold_rejects_edge_closed_but_inconsistent_winding() {
        let bad = edge_closed_but_inconsistent();
        assert!(
            crate::is_closed_manifold(&bad),
            "cheap edge check should still pass — that is why it under-approximates"
        );
        assert!(
            !is_csg_manifold(&bad),
            "Manifold must reject the inconsistent winding"
        );
        // The fallible union must decline rather than panic.
        assert!(try_union_many(&[ubox([1.0, 1.0, 1.0]), bad]).is_none());
    }

    #[test]
    fn is_csg_manifold_accepts_closed_primitive() {
        assert!(is_csg_manifold(&ubox([1.0, 1.0, 1.0])));
    }

    #[test]
    fn hull_of_cube_corners_is_a_closed_manifold_box() {
        // The 8 corners of a unit cube; their convex hull is that cube.
        let corners: Vec<[f32; 3]> = [-0.5f32, 0.5]
            .iter()
            .flat_map(|&x| {
                [-0.5f32, 0.5].iter().flat_map(move |&y| {
                    [-0.5f32, 0.5].iter().map(move |&z| [x, y, z])
                })
            })
            .collect();
        let m = hull_mesh(&corners);
        assert!(!m.positions.is_empty(), "hull must produce geometry");
        assert!(
            is_csg_manifold(&m),
            "a convex hull must be a watertight, consistently-wound manifold"
        );
        // Hull is a clean solid usable as a CSG operand (the migration's
        // arbitrary-block sink relies on this).
        assert!(crate::is_closed_manifold(&m));
        // Extra interior points must not change the convex hull.
        let mut padded = corners.clone();
        padded.push([0.0, 0.0, 0.0]);
        padded.push([0.1, -0.2, 0.3]);
        let m2 = hull_mesh(&padded);
        let bb = |mesh: &Mesh| {
            let mut lo = [f32::MAX; 3];
            let mut hi = [f32::MIN; 3];
            for p in &mesh.positions {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            (lo, hi)
        };
        assert_eq!(bb(&m).0, bb(&m2).0);
        assert_eq!(bb(&m).1, bb(&m2).1);
    }

    #[test]
    fn try_union_many_two_valid_manifolds_returns_some() {
        let a = ubox([1.0, 1.0, 1.0]);
        let mut b = ubox([1.0, 1.0, 1.0]);
        for p in &mut b.positions {
            p[0] += 2.0;
        }
        let result = try_union_many(&[a, b]);
        assert!(result.is_some(), "two valid manifolds must union to Some");
        assert!(!result.unwrap().positions.is_empty());
    }

    #[test]
    fn try_union_many_single_non_manifold_returns_none() {
        let bad = edge_closed_but_inconsistent();
        assert!(
            try_union_many(&[bad]).is_none(),
            "single non-manifold operand must return None, not Some"
        );
    }

    fn ucyl(r: f32, h: f32, seg: u32) -> Mesh {
        cylinder_mesh(r, h, seg, UvMode::default())
    }

    fn bounds(m: &Mesh) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for p in &m.positions {
            min = min.min(Vec3::from_array(*p));
            max = max.max(Vec3::from_array(*p));
        }
        (min, max)
    }

    #[test]
    fn union_of_disjoint_boxes_has_both() {
        let a = ubox([1.0, 1.0, 1.0]);
        let mut b = ubox([1.0, 1.0, 1.0]);
        for p in &mut b.positions {
            p[0] += 3.0;
        }
        let u = union(&a, &b);
        let (min, max) = bounds(&u);
        assert!(min.x <= -0.5 + 1e-4);
        assert!(max.x >= 3.5 - 1e-4);
        assert!(!u.indices.is_empty());
    }

    #[test]
    fn difference_cuts_hole_in_wall() {
        let wall = ubox([4.0, 3.0, 0.2]);
        // Cylinder pierces all the way through the wall's thickness.
        let hole = ucyl(0.4, 1.0, 16);
        let result = difference(&wall, &hole);
        assert!(!result.indices.is_empty());
        let (min, max) = bounds(&result);
        // Wall's x/y envelope is preserved; only material near the cylinder is removed.
        assert!(min.x <= -1.99);
        assert!(max.x >= 1.99);
        assert!(min.y <= -1.49);
        assert!(max.y >= 1.49);
    }

    #[test]
    fn box_minus_small_box() {
        let a = box_mesh([2.0, 2.0, 2.0], mogen_core::UvMode::default());
        let b = ubox([1.0, 1.0, 1.0]);
        let r = difference(&a, &b);
        let (min, max) = bounds(&r);
        assert!(min.x <= -0.99);
        assert!(max.x >= 0.99);
    }

    #[test]
    fn box_minus_contained_cylinder() {
        let a = box_mesh([2.0, 2.0, 2.0], mogen_core::UvMode::default());
        let b = ucyl(0.3, 1.0, 16);
        let r = difference(&a, &b);
        let (min, max) = bounds(&r);
        assert!(min.x <= -0.99);
        assert!(max.x >= 0.99);
    }

    #[test]
    fn intersect_of_disjoint_is_empty() {
        let a = ubox([1.0, 1.0, 1.0]);
        let mut b = ubox([1.0, 1.0, 1.0]);
        for p in &mut b.positions {
            p[0] += 3.0;
        }
        let i = intersect(&a, &b);
        assert!(i.indices.is_empty());
    }

    #[test]
    fn union_drops_seam_uvs_so_triplanar_can_take_over() {
        // Box/cylinder primitives carry per-face UV seams that would average
        // into garbage if welded. The boolean must drop them so the cleanup
        // pass can synthesise triplanar UVs on the result.
        let a = ubox([1.0, 1.0, 1.0]);
        let mut b = ubox([1.0, 1.0, 1.0]);
        for p in &mut b.positions {
            p[0] += 0.5;
        }
        assert!(a.has_uvs() && b.has_uvs(), "box primitives emit UVs");
        let u = union(&a, &b);
        assert!(
            !u.has_uvs(),
            "boxes have UV seams — boolean output must defer to triplanar"
        );
    }

    #[test]
    fn difference_drops_seam_uvs_so_triplanar_can_take_over() {
        let a = ubox([2.0, 2.0, 2.0]);
        let b = ucyl(0.4, 3.0, 16);
        assert!(a.has_uvs() && b.has_uvs());
        let r = difference(&a, &b);
        assert!(
            !r.has_uvs(),
            "seamed inputs must yield no-UV output for triplanar fallback"
        );
    }

    #[test]
    fn seamless_uv_inputs_survive_boolean() {
        // Build operands whose coincident-position vertices share UVs (no
        // seams) so the boolean keeps UVs end-to-end.
        let a = ubox([1.0, 1.0, 1.0]);
        let mut a = a;
        // Replace per-face UVs with a single uv-per-position to simulate a
        // seamless mesh (e.g., a sphere-equivalent with no wrap seam).
        for uv in &mut a.uvs {
            *uv = [0.5, 0.5];
        }
        let mut b = ubox([1.0, 1.0, 1.0]);
        for p in &mut b.positions {
            p[0] += 0.5;
        }
        for uv in &mut b.uvs {
            *uv = [0.5, 0.5];
        }
        let u = union(&a, &b);
        assert!(u.has_uvs(), "seamless UVs survive the boolean");
        assert_eq!(u.positions.len(), u.uvs.len());
    }

    #[test]
    fn union_drops_uvs_when_one_operand_lacks_them() {
        let a = ubox([1.0, 1.0, 1.0]);
        let mut b = ubox([1.0, 1.0, 1.0]);
        for p in &mut b.positions {
            p[0] += 0.5;
        }
        // Simulate an operand that came through the legacy no-UV path.
        b.uvs.clear();
        assert!(!b.has_uvs());
        let u = union(&a, &b);
        assert!(
            !u.has_uvs(),
            "mixed-UV inputs should fail closed to no-UV output",
        );
    }
}
