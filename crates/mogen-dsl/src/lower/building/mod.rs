//! `building` node lowering.
//!
//! Top-level expander for `building "name" (...) { room_type … adjacency … }`.
//! The wrapper node stays editable; everything below it is stamped non-
//! editable because the entire subtree is a deterministic function of the
//! seed plus the declared attrs.
//!
//! Tranche 2 supports multi-storey buildings (any combination of
//! `floors_above ≥ 1`, `floors_below ≥ 0`), staircases and elevators
//! that line up across every floor, and top-floor skylights. Style is
//! still limited to `grid` and `apartment-block`, roof to `flat`; the
//! remaining styles + roof shapes arrive in Tranches 3-4. See
//! `docs/building.md` for the full plan.

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

    // `debug_render_floor` isolates one storey: filter the list down to it
    // and pretend it's both the bottom (no floor cutouts) and not the top
    // (no ceiling slab / skylights) so the result reads as a clean
    // floorplate. Falls back to all storeys if the requested index doesn't
    // exist in the layout.
    let storeys_to_emit: Vec<&StoreyPlate> = match cfg.debug_render_floor {
        Some(target) if layout.storeys.iter().any(|s| s.storey == target) => layout
            .storeys
            .iter()
            .filter(|s| s.storey == target)
            .collect(),
        _ => layout.storeys.iter().collect(),
    };
    let isolated_floor = cfg.debug_render_floor.is_some() && storeys_to_emit.len() == 1;

    for storey_plate in &storeys_to_emit {
        let force_no_ceiling = cfg.debug_hide_roof || isolated_floor;
        emit_storey(
            node,
            &cfg,
            &layout,
            storey_plate,
            bottom_storey,
            top_storey,
            isolated_floor,
            force_no_ceiling,
            wrapper_id,
            graph,
        )?;
    }

    // Vertical circulation (stair flights between adjacent storeys) lives in
    // its own subtree under the wrapper so it can span Y without juggling
    // per-storey local frames. Skipped when isolating a single floor —
    // dangling flights between unrendered storeys would just be visual noise.
    if !isolated_floor {
        emit::circulation::emit_circulation(
            node,
            &cfg,
            &layout,
            bottom_storey,
            top_storey,
            wrapper_id,
            graph,
        )?;
    }

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
    isolated_floor: bool,
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

    // Isolated debug floor: pretend it's the bottom (no stair cutouts in
    // the floor slab) regardless of where it actually sits in the stack.
    let is_top_topology = storey == top_storey;
    let is_top = is_top_topology && !force_no_ceiling;
    let is_bottom = storey == bottom_storey || isolated_floor;
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
