//! Boolean CSG over triangle meshes — backed by Google's Manifold library
//! via the `manifold3d` Rust bindings.
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
//! When both operands carry UVs we send `[x, y, z, u, v]` per vertex
//! (`n_props = 5`); Manifold interpolates the UVs linearly across boolean
//! intersection cuts. Otherwise we send positions only and let the cleanup
//! pass synthesise triplanar UVs.
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

use manifold3d::Manifold;

use mogen_core::Mesh;

use crate::cleanup::{recompute_normals, weld_vertices};

/// Same epsilon as `clean_csg_output` so a CSG operand that already went
/// through the cleanup pass doesn't get re-welded with a different threshold.
const WELD_EPS: f32 = 1e-4;

/// Convert a `Mesh` into the `(vert_props, n_props, tri_indices)` triple
/// Manifold expects. UVs are interleaved when present so they survive the
/// boolean; positions are emitted as the first three components either way.
fn to_manifold_input(mesh: &Mesh) -> (Vec<f32>, usize, Vec<u32>) {
    let welded = weld_vertices(mesh, WELD_EPS);
    let has_uvs = welded.has_uvs();
    let n_props = if has_uvs { 5 } else { 3 };
    let mut vert_props = Vec::with_capacity(welded.positions.len() * n_props);
    for (i, p) in welded.positions.iter().enumerate() {
        vert_props.extend_from_slice(p);
        if has_uvs {
            vert_props.extend_from_slice(&welded.uvs[i]);
        }
    }
    (vert_props, n_props, welded.indices)
}

fn build_manifold(mesh: &Mesh) -> (Manifold, usize) {
    let (vert_props, n_props, tri_indices) = to_manifold_input(mesh);
    let manifold = Manifold::from_mesh_f32(&vert_props, n_props, &tri_indices)
        .expect("CSG operand is not a manifold mesh; check that the primitive is closed");
    (manifold, n_props)
}

/// Convert a Manifold result back to `Mesh`. UVs are recovered when
/// `n_props == 5`; otherwise the output `uvs` is empty and the caller's
/// `clean_csg_output` will assign triplanar coordinates. Normals are
/// recomputed from face geometry so the returned mesh is internally
/// consistent — callers like `union_smooth` rely on per-vertex normals
/// without needing a separate cleanup pass first.
fn from_manifold_output(m: &Manifold, n_props: usize) -> Mesh {
    let (flat, props, tri_indices) = m.to_mesh_f32();
    debug_assert_eq!(props, n_props);
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
    // If only one side carries UVs, fall back to position-only output —
    // mixing a UV'd operand with a non-UV'd one would interpolate against
    // garbage. The cleanup pass will synthesise triplanar UVs.
    let out_props = if props_a == props_b { props_a } else { 3 };
    let result = match op {
        Op::Union => ma.union(&mb),
        Op::Difference => ma.difference(&mb),
        Op::Intersect => ma.intersection(&mb),
    };
    if result.is_empty() {
        return Mesh::default();
    }
    if out_props != props_a {
        // The result still has whatever n_props the operands shared inside
        // Manifold; if we requested mixed handling we re-emit without UVs by
        // stripping them after extraction.
        let mut m = from_manifold_output(&result, props_a);
        m.uvs.clear();
        return m;
    }
    from_manifold_output(&result, out_props)
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

pub fn intersect_many(meshes: &[Mesh]) -> Mesh {
    let mut it = meshes.iter();
    let Some(first) = it.next() else {
        return Mesh::default();
    };
    it.fold(first.clone(), |acc, m| intersect(&acc, m))
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
    fn union_preserves_uvs_when_both_operands_have_them() {
        let a = ubox([1.0, 1.0, 1.0]);
        let mut b = ubox([1.0, 1.0, 1.0]);
        for p in &mut b.positions {
            p[0] += 0.5;
        }
        assert!(a.has_uvs() && b.has_uvs(), "box primitives emit UVs");
        let u = union(&a, &b);
        assert!(u.has_uvs(), "union output should carry UVs through Manifold");
        assert_eq!(u.positions.len(), u.uvs.len());
    }

    #[test]
    fn difference_preserves_uvs_when_both_operands_have_them() {
        let a = ubox([2.0, 2.0, 2.0]);
        let b = ucyl(0.4, 3.0, 16);
        assert!(a.has_uvs() && b.has_uvs());
        let r = difference(&a, &b);
        assert!(r.has_uvs());
        assert_eq!(r.positions.len(), r.uvs.len());
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
