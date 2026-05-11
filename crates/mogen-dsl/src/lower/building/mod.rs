//! `building` node lowering.
//!
//! Top-level expander for `building "name" (...) { room_type … adjacency … }`.
//! The wrapper node stays editable; everything below it is stamped non-
//! editable because the entire subtree is a deterministic function of the
//! seed plus the declared attrs.
//!
//! Tranche 1 supports `style="grid"` and `style="apartment-block"`,
//! single-floor only, flat roof, no circulation, no skylights. See
//! `docs/building.md` for the full plan.

mod config;
mod rng;
mod layout;
mod emit;

#[cfg(test)]
mod tests;

use anyhow::Result;

use mogen_core::{NodeId, SceneGraph, Transform};

use crate::ast::Node;
use crate::lower::helpers::transform_from_attrs;
use crate::lower::node::apply_metadata;

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

    let floorplate = layout::solve(&cfg)?;

    let floor_group = graph.add_child(
        wrapper_id,
        "floor_0".to_string(),
        "group",
        Transform::IDENTITY,
    );
    graph.nodes[floor_group.0 as usize].origin = node.origin.clone();
    graph.nodes[floor_group.0 as usize]
        .tags
        .extend(["building".into(), "floor_0".into()]);

    emit::emit_floor(node, &cfg, &floorplate, floor_group, graph)?;

    // Stamp the whole subtree non-editable so the inspector won't let users
    // hand-tweak generated walls (a rebuild would wipe the edits). Matches
    // the branch-wrapper convention.
    for i in pre_expand_count..graph.nodes.len() {
        graph.nodes[i].editable = false;
    }

    Ok(wrapper_id)
}
