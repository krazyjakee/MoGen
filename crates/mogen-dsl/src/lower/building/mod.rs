//! `building` node lowering.
//!
//! Top-level expander for `building "name" (...) { room_type … adjacency … }`.
//! The wrapper node stays editable; everything below it is stamped non-
//! editable because the entire subtree is a deterministic function of the
//! seed plus the declared attrs.
//!
//! Tranche 4 completes the originally specified scope: every layout style
//! (`grid`, `apartment-block`, `hotel-corridor`, `office-core`, `radial`,
//! `organic`, `maze`) and every roof shape (`flat`, `pitched`, `gabled`,
//! `hipped`, `mansard`, `shed`) is implemented, and `cellar_area=` lets
//! below-ground storeys use a smaller footprint than the above-ground
//! plate. See `docs/building.md` for the per-tranche schedule.

mod circulation;
mod config;
mod materials;
mod rng;
mod layout;
mod emit;

#[cfg(test)]
mod tests;

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{ColliderShape, NodeId, SceneGraph, Transform};

use crate::ast::Node;
use crate::lower::helpers::transform_from_attrs;
use crate::lower::node::apply_metadata;

use config::BuildingCfg;
use layout::{BuildingLayout, StoreyPlate};

pub(super) fn expand_building(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let cfg = config::read_cfg(node)?;

    let wrapper_name = node.name.clone().unwrap_or_else(|| node.kind.clone());
    let wrapper_transform = transform_from_attrs(node);
    let wrapper_id = match parent {
        None => graph.add_root(&wrapper_name, &node.kind, wrapper_transform),
        Some(p) => graph.add_child(p, &wrapper_name, &node.kind, wrapper_transform),
    };
    graph.set_source_span(wrapper_id, node.span);
    graph.nodes[wrapper_id.0 as usize].use_id = node.use_id;
    graph.nodes[wrapper_id.0 as usize].origin = node.origin.clone();
    apply_metadata(node, wrapper_id, graph)?;

    // Stamp default frame / slab / glass materials before any opening module
    // is expanded — the stdlib door / window / skylight bodies reference
    // them by name. Anything the user already declared on this origin wins.
    materials::ensure_opening_defaults(graph, node.origin.as_deref());

    let pre_expand_count = graph.nodes.len();

    let layout = layout::solve(&cfg)?;

    // Sorted storey indices so the lowest floor is emitted first; helps
    // anyone reading the tree top-down (basement → ground → upper).
    let bottom_storey = layout
        .storeys
        .iter()
        .map(|s| s.storey)
        .min()
        .unwrap_or(0);
    let top_storey = layout
        .storeys
        .iter()
        .map(|s| s.storey)
        .max()
        .unwrap_or(0);

    // `debug_render_floor` isolates one storey by filtering the floor list
    // down to it, but the rest of the building (vertical circulation, the
    // chosen floor's slab cutouts, the natural roof if the isolated storey
    // happens to be the top) emits unchanged so stairs and elevators stay
    // visible. Falls back to all storeys if the requested index doesn't
    // exist in the layout. When isolating, tag the wrapper as `floating` so
    // the connectivity validator (E1101) skips this subtree — stair flights
    // suspended between unrendered floor slabs would otherwise read as a
    // pile of disconnected step clusters. The flag is an explicit debug
    // affordance, so bypassing structural validation for it is fine.
    let isolating = matches!(cfg.debug_render_floor,
        Some(t) if layout.storeys.iter().any(|s| s.storey == t));
    let storeys_to_emit: Vec<&StoreyPlate> = if isolating {
        let target = cfg.debug_render_floor.unwrap();
        layout.storeys.iter().filter(|s| s.storey == target).collect()
    } else {
        layout.storeys.iter().collect()
    };
    if isolating {
        graph.nodes[wrapper_id.0 as usize]
            .tags
            .push("floating".into());
    }

    for storey_plate in &storeys_to_emit {
        emit_storey(
            node,
            &cfg,
            &layout,
            storey_plate,
            bottom_storey,
            top_storey,
            cfg.debug_hide_roof,
            wrapper_id,
            graph,
        )?;
    }

    // Vertical circulation (stair flights between adjacent storeys) lives in
    // its own subtree under the wrapper so it can span Y without juggling
    // per-storey local frames. Always emits — even when isolating a single
    // floor, the user wants to see stairs and the elevator shaft passing
    // through it.
    emit::circulation::emit_circulation(
        node,
        &cfg,
        &layout,
        bottom_storey,
        top_storey,
        wrapper_id,
        graph,
    )?;

    // Stamp the whole subtree non-editable so the inspector won't let users
    // hand-tweak generated walls (a rebuild would wipe the edits).
    for i in pre_expand_count..graph.nodes.len() {
        graph.nodes[i].editable = false;
    }

    tag_building_colliders(graph, pre_expand_count);

    Ok(wrapper_id)
}

/// Walk every node the building emitter just produced and tag mesh-bearing
/// nodes with [`ColliderShape::Trimesh`] so a game engine importer can drop
/// the .glb into a level and have physics work without hand-authoring shapes.
///
/// We skip:
/// - Slot wrappers and everything underneath them: doors / windows / skylights
///   are placeholders the importer will replace with its own prefabs, and
///   those prefabs ship their own colliders.
/// - Nodes that already carry a collider (defensive — buildings don't set
///   one anywhere else today, but the field is public).
///
/// AABB would be too coarse for buildings — interior walls, stair flights and
/// roof slopes are not box-aligned. Trimesh re-uses the node's own mesh as
/// the collision shape, which is exactly the geometry the wall builder
/// already cleaned up for export.
fn tag_building_colliders(graph: &mut SceneGraph, start: usize) {
    let mut in_slot_subtree = vec![false; graph.nodes.len()];
    for i in start..graph.nodes.len() {
        let parent_in_slot = graph.nodes[i]
            .parent
            .map(|p| in_slot_subtree[p.0 as usize])
            .unwrap_or(false);
        let self_is_slot = graph.nodes[i].slot.is_some();
        if parent_in_slot || self_is_slot {
            in_slot_subtree[i] = true;
            continue;
        }
        if graph.nodes[i].collider.is_some() {
            continue;
        }
        // POI markers are gameplay anchors, not collision geometry — even the
        // optional `debug_show_poi` spheres stay collider-free, like caves.
        if graph.nodes[i].kind == "poi" {
            continue;
        }
        if graph.nodes[i].mesh.is_some() {
            graph.nodes[i].collider = Some(ColliderShape::Trimesh);
        }
    }
}

fn emit_storey(
    node: &Node,
    cfg: &BuildingCfg,
    layout: &BuildingLayout,
    storey_plate: &StoreyPlate,
    bottom_storey: i32,
    top_storey: i32,
    force_no_ceiling: bool,
    wrapper_id: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let storey = storey_plate.storey;
    let y = storey_world_y(cfg, storey);
    let floor_group = graph.add_child(
        wrapper_id,
        format!("floor_{}", storey_label(storey)),
        "group",
        Transform::from_trs(Vec3::new(0.0, y, 0.0), Quat::IDENTITY, Vec3::ONE),
    );
    graph.nodes[floor_group.0 as usize].origin = node.origin.clone();
    graph.nodes[floor_group.0 as usize].tags.extend([
        "building".into(),
        format!("storey={}", storey),
    ]);

    let is_top_topology = storey == top_storey;
    let is_top = is_top_topology && !force_no_ceiling;
    let is_bottom = storey == bottom_storey;
    let ctx = emit::StoreyCtx {
        storey,
        is_bottom,
        is_top,
        bottom_storey,
        top_storey,
    };
    emit::emit_floor(
        node,
        cfg,
        &layout.circulation,
        &storey_plate.plate,
        ctx,
        &layout.entrance_support,
        floor_group,
        graph,
    )?;
    Ok(())
}

/// World-space Y of the top surface of storey `s`'s floor slab.
fn storey_world_y(cfg: &BuildingCfg, storey: i32) -> f32 {
    let step = cfg.ceiling_height + cfg.ceiling_thickness;
    storey as f32 * step
}

/// Human-readable storey suffix used for node names. Basements get a `b`
/// prefix so `floor_b1` reads naturally even though the index is `-1`.
fn storey_label(s: i32) -> String {
    if s >= 0 {
        s.to_string()
    } else {
        format!("b{}", -s)
    }
}
