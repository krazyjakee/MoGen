//! Patch-mode conform: lay a flat / disc child onto a single anchor
//! connector on the target's surface.

use anyhow::{anyhow, bail, Result};

use mogen_core::{ConformBinding, NodeId, SceneGraph};
use mogen_geom::{conform_patch, subdivide_along_axis, Axis, PatchParams};

use super::kinds::{check_kind_allowed, default_up_for, ConformModeKind};
use super::place::{
    bake_local_rs_into_mesh, build_target_surface, list_connector_names, place_deformed_mesh,
};
use super::spec::{ConformMode, ConformSpec};

pub(super) fn apply_patch(
    spec: &ConformSpec,
    graph: &mut SceneGraph,
    target_id: NodeId,
    child_id: NodeId,
) -> Result<()> {
    let ConformMode::Patch { at, up } = &spec.mode else {
        unreachable!("apply_patch called with non-Patch mode");
    };

    let child_kind = graph.nodes[child_id.0 as usize].kind.clone();
    check_kind_allowed(&child_kind, &spec.child, ConformModeKind::Patch)?;

    // Imported meshes have no canonical up axis — author must pick one.
    let up_axis = match (child_kind.as_str(), *up) {
        ("mesh", None) => {
            bail!(
                "conform: imported mesh \"{}\" must specify up= (no canonical normal axis)",
                spec.child
            );
        }
        (_, Some(a)) => a,
        _ => default_up_for(&child_kind),
    };

    // Resolve the anchor connector first so a missing-connector error fires
    // before we build the surface index.
    let (anchor_pos, anchor_dir) = {
        let t = &graph.nodes[target_id.0 as usize];
        let c = t.connectors.iter().find(|c| c.name == *at).ok_or_else(|| {
            anyhow!(
                "conform: target \"{}\" has no connector \"{}\" (available: {})",
                spec.target,
                at,
                list_connector_names(&t.connectors)
            )
        })?;
        // Connectors store rotation as a quat that turns +Y into `dir`,
        // so the outward direction is `rotation * +Y`.
        let dir = c.rotation * glam::Vec3::Y;
        (c.pos, Some(dir))
    };

    let surface = build_target_surface(graph, target_id, &spec.target)?;

    let child_mesh = graph.nodes[child_id.0 as usize]
        .mesh
        .as_ref()
        .ok_or_else(|| anyhow!("conform: child \"{}\" has no mesh", spec.child))?
        .clone();
    // Bake the user's local rotation/scale into the mesh before deformation.
    // `place_deformed_mesh` will reset the node transform to identity, so any
    // `rot=`/`scale=` on the source decal/disc/etc. would otherwise be silently
    // discarded. The user's `pos=` is intentionally dropped — patch positioning
    // comes from `at=`, not from `pos=`. For a decal the meaningful rotation is
    // a tangent-plane spin (`rz=` / `rot=[0,0,deg]`); off-plane rotations bend
    // the artwork off the surface, which the conform kernel will faithfully
    // reproduce — surprising results there are author-driven, not silent.
    let child_mesh = bake_local_rs_into_mesh(&graph.nodes[child_id.0 as usize].transform, &child_mesh);
    // Auto-subdivide along the two planar axes so the patch can follow surface
    // curvature. A bare disc has only a centre + rim (≈31 verts); without
    // densification the triangles between rim vertices stay flat and the patch
    // doesn't wrap the target. Mirrors the path-mode subdivision in apply_path.
    let (planar_a, planar_b) = match up_axis {
        Axis::X => (Axis::Y, Axis::Z),
        Axis::Y => (Axis::X, Axis::Z),
        Axis::Z => (Axis::X, Axis::Y),
    };
    let target_segments = 16;
    let child_mesh = subdivide_along_axis(&child_mesh, planar_a, target_segments);
    let child_mesh = subdivide_along_axis(&child_mesh, planar_b, target_segments);

    let deformed_target_local = conform_patch(
        &child_mesh,
        &surface,
        &PatchParams {
            up: up_axis,
            anchor: anchor_pos,
            anchor_dir,
            roll_ref: None,
            lift: spec.lift,
        },
    );

    place_deformed_mesh(graph, child_id, target_id, deformed_target_local, spec.reparent);
    graph.nodes[child_id.0 as usize].conform_binding = Some(ConformBinding::Patch {
        target: target_id,
        at: at.clone(),
    });

    Ok(())
}
