//! Helpers shared by both path and patch modes: target-surface
//! construction, mesh placement after deformation, axis-baking, and
//! sanity-extent reporting.

use anyhow::{anyhow, bail, Result};

use glam::{Mat4, Quat, Vec3};

use mogen_core::{Connector, NodeId, SceneGraph, Transform};
use mogen_geom::{transform_mesh, SurfaceIndex};

use crate::attach::reparent_pub;

pub(super) fn build_target_surface(
    graph: &SceneGraph,
    target_id: NodeId,
    target_name: &str,
) -> Result<SurfaceIndex> {
    let target_mesh = graph.nodes[target_id.0 as usize]
        .mesh
        .as_ref()
        .ok_or_else(|| {
            anyhow!(
                "conform: target \"{}\" has no geometry — conform requires a mesh-bearing target",
                target_name
            )
        })?
        .clone();
    let surface = SurfaceIndex::build(&target_mesh);
    if surface.is_empty() {
        bail!("conform: target \"{}\" mesh has no triangles", target_name);
    }
    Ok(surface)
}

pub(super) fn place_deformed_mesh(
    graph: &mut SceneGraph,
    child_id: NodeId,
    target_id: NodeId,
    deformed_target_local: mogen_core::Mesh,
    reparent: bool,
) {
    if reparent {
        // Place the deformed mesh "as if" it lives in target-local space:
        // reparent under target with identity local transform. Any user
        // pos=/rot= on the child is intentionally discarded.
        graph.nodes[child_id.0 as usize].mesh = Some(deformed_target_local);
        graph.nodes[child_id.0 as usize].geometry_identity = None;
        graph.nodes[child_id.0 as usize].transform = Transform::IDENTITY;
        reparent_pub(graph, child_id, target_id);
    } else {
        // Keep the child's parentage intact; move the deformed mesh from
        // target-local space back into child-local space.
        let world = graph.world_transforms();
        let to_child = world[child_id.0 as usize].inverse() * world[target_id.0 as usize];
        let final_mesh = transform_mesh(&deformed_target_local, to_child);
        graph.nodes[child_id.0 as usize].mesh = Some(final_mesh);
        graph.nodes[child_id.0 as usize].geometry_identity = None;
    }
}

pub(super) fn list_connector_names(cs: &[Connector]) -> String {
    if cs.is_empty() {
        return "<none>".to_string();
    }
    cs.iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Bake the user's local rotation and (non-unit) scale from `transform` into
/// `mesh`'s vertex positions and normals. Translation is intentionally
/// dropped — conform positions the result via the target's connector(s),
/// not the child's `pos=`. Returns the input cloned untouched when the
/// rotation/scale are both identity.
///
/// Without this step, a `decal "logo" (rot=[0, 0, 90])` (or any rotated
/// conform child) would silently lose its rotation: `place_deformed_mesh`
/// resets `node.transform` to identity after deformation, so the only place
/// the user's rotation can survive is baked into the geometry.
pub(super) fn bake_local_rs_into_mesh(
    transform: &mogen_core::Transform,
    mesh: &mogen_core::Mesh,
) -> mogen_core::Mesh {
    if transform.rotation == Quat::IDENTITY && transform.scale == Vec3::ONE {
        return mesh.clone();
    }
    let m = Mat4::from_scale_rotation_translation(transform.scale, transform.rotation, Vec3::ZERO);
    transform_mesh(mesh, m)
}

pub(super) fn aabb_extent(mesh: &mogen_core::Mesh, axis: usize) -> Option<f32> {
    if mesh.positions.is_empty() {
        return None;
    }
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for p in &mesh.positions {
        let v = p[axis];
        if v < lo {
            lo = v;
        }
        if v > hi {
            hi = v;
        }
    }
    Some(hi - lo)
}
