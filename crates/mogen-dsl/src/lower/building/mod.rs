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
mod rng;
mod layout;
mod emit;

#[cfg(test)]
mod tests;

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Transform};

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

    Ok(wrapper_id)
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
    emit::emit_floor(node, cfg, &layout.circulation, &storey_plate.plate, ctx, floor_group, graph)?;
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
