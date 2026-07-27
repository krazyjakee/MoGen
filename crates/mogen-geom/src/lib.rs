pub mod cleanup;
pub mod conform;
#[cfg(any(feature = "csg", feature = "unstable-wasm-uu"))]
pub mod csg;
#[cfg(any(feature = "csg", feature = "unstable-wasm-uu"))]
pub mod csg_smooth;
pub mod deform;
pub mod isosurface;
pub mod primitives;
pub mod sdf;
pub mod subdivide;
pub mod surface_query;
pub mod xform;

pub use cleanup::{
    clean_csg_output, cull_coplanar_opposites, cull_degenerate, is_closed_manifold,
    is_degenerate_solid, mesh_volume, recompute_normals, weld_vertices,
};
pub use conform::{
    build_path_frames, conform_mesh, conform_patch, subdivide_along_axis, Axis, AxisMap,
    ConformParams, PatchParams, PathFrame,
};
#[cfg(any(feature = "csg", feature = "unstable-wasm-uu"))]
pub use csg::{
    difference, difference_many, hull_mesh, intersect, intersect_many, is_csg_manifold,
    try_union_many, union, union_many,
};
#[cfg(any(feature = "csg", feature = "unstable-wasm-uu"))]
pub use csg_smooth::union_smooth;
pub use deform::{bend, droop, jitter, noise, split_for_facets, taper, twist_y, wave};
pub use isosurface::blob_to_mesh;
pub use sdf::{blob_aabb, evaluate_field, smax, smin, BlobChild, SdfOp, SdfPrim};
pub use subdivide::loop_subdivide;
pub use surface_query::{SurfaceIndex, SurfacePoint};
pub use primitives::{
    box_faces_mesh, box_faces_mesh_authored, box_mesh, capsule_mesh, chamfered_box_mesh, coil_mesh, cone_mesh, curved_plane_mesh,
    cylinder_mesh, disc_mesh, ellipsoid_mesh, extrude_mesh, frustum_mesh, half_cylinder_mesh,
    heightfield_mesh,
    hemisphere_mesh, icosphere_mesh, inset_box_mesh, lathe_mesh, leaf_card_mesh, loft_mesh,
    bezier_patch_mesh, metaball_mesh,
    mesh_from_glb_bytes, plane_mesh, poly_mesh, prism_mesh, pyramid_mesh, quad_mesh, read_glb_bytes,
    rounded_box_mesh, sphere_mesh, spline_ribbon_mesh, spline_tube_mesh, superellipsoid_mesh,
    sweep_mesh, torus_arc_mesh, torus_mesh, tube_mesh, wedge_mesh, CoilHandedness, Contour,
    FaceUvXform, InsetFace, Section, SweepModulation, SweepProfile,
};
pub use xform::transform_mesh;

// CSG stubs — kept on the public surface even with both CSG features off so
// that downstream crates (mogen-dsl, mogen-export::merge) compile unchanged.
// Reaching a stub at runtime is a programmer error: builds either enable
// `csg` (desktop) or `unstable-wasm-uu` (wasm).
#[cfg(not(any(feature = "csg", feature = "unstable-wasm-uu")))]
mod csg_stub {
    use mogen_core::Mesh;

    fn unsupported() -> ! {
        panic!("CSG operations are not available: this build of mogen-geom was compiled without the `csg` feature");
    }

    pub fn union(_a: &Mesh, _b: &Mesh) -> Mesh {
        unsupported()
    }
    pub fn difference(_a: &Mesh, _b: &Mesh) -> Mesh {
        unsupported()
    }
    pub fn intersect(_a: &Mesh, _b: &Mesh) -> Mesh {
        unsupported()
    }
    pub fn union_many(_meshes: &[Mesh]) -> Mesh {
        unsupported()
    }
    pub fn try_union_many(_meshes: &[Mesh]) -> Option<Mesh> {
        // No CSG backend: there is nothing to merge into, so report "can't".
        None
    }
    pub fn is_csg_manifold(_mesh: &Mesh) -> bool {
        // Without a CSG backend nothing can be a boolean operand, so the merge
        // pass must treat every leaf as non-mergeable.
        false
    }
    pub fn difference_many(_a: &Mesh, _rest: &[Mesh]) -> Mesh {
        unsupported()
    }
    pub fn intersect_many(_meshes: &[Mesh]) -> Mesh {
        unsupported()
    }
    pub fn union_smooth(_meshes: &[Mesh], _k: f32) -> Mesh {
        unsupported()
    }
    pub fn hull_mesh(_points: &[[f32; 3]]) -> Mesh {
        unsupported()
    }
}
#[cfg(not(any(feature = "csg", feature = "unstable-wasm-uu")))]
pub use csg_stub::{
    difference, difference_many, hull_mesh, intersect, intersect_many, is_csg_manifold,
    try_union_many, union, union_many, union_smooth,
};
