pub mod cleanup;
pub mod conform;
#[cfg(feature = "csg")]
pub mod csg;
#[cfg(feature = "csg")]
pub mod csg_smooth;
pub mod primitives;
pub mod surface_query;
pub mod xform;

pub use cleanup::{
    clean_csg_output, cull_coplanar_opposites, cull_degenerate, recompute_normals, weld_vertices,
};
pub use conform::{
    build_path_frames, conform_mesh, conform_patch, subdivide_along_axis, Axis, AxisMap,
    ConformParams, PatchParams, PathFrame,
};
#[cfg(feature = "csg")]
pub use csg::{difference, difference_many, intersect, intersect_many, union, union_many};
#[cfg(feature = "csg")]
pub use csg_smooth::union_smooth;
pub use surface_query::{SurfaceIndex, SurfacePoint};
pub use primitives::{
    box_mesh, capsule_mesh, cone_mesh, curved_plane_mesh, cylinder_mesh, disc_mesh, ellipsoid_mesh,
    frustum_mesh, half_cylinder_mesh, hemisphere_mesh, icosphere_mesh, lathe_mesh, leaf_card_mesh,
    mesh_from_glb_bytes, plane_mesh, prism_mesh, pyramid_mesh, quad_mesh, read_glb_bytes,
    rounded_box_mesh, sphere_mesh, spline_ribbon_mesh, spline_tube_mesh, superellipsoid_mesh,
    torus_arc_mesh, torus_mesh, tube_mesh, wedge_mesh,
};
pub use xform::transform_mesh;

// CSG stubs — kept on the public surface even with the `csg` feature off so
// that downstream crates (mogen-dsl, mogen-export::merge) compile unchanged.
// Callers in builds without `csg` MUST avoid invoking these; for the wasm
// crate that means rejecting `union`/`difference`/`intersect` AST nodes
// before lowering. Reaching a stub is a programmer error, hence the panic.
#[cfg(not(feature = "csg"))]
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
    pub fn difference_many(_a: &Mesh, _rest: &[Mesh]) -> Mesh {
        unsupported()
    }
    pub fn intersect_many(_meshes: &[Mesh]) -> Mesh {
        unsupported()
    }
    pub fn union_smooth(_meshes: &[Mesh], _k: f32) -> Mesh {
        unsupported()
    }
}
#[cfg(not(feature = "csg"))]
pub use csg_stub::{
    difference, difference_many, intersect, intersect_many, union, union_many, union_smooth,
};
