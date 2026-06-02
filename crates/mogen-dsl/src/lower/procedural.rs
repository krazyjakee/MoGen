//! Shared scaffolding for the procedural generators (`branch`, `building`,
//! `cave`).
//!
//! Every generator follows the same shape: create one editable wrapper node
//! carrying the source node's transform / span / metadata, emit a deterministic
//! subtree beneath it from `seed=` + attrs, then stamp that whole subtree
//! non-editable so the inspector won't let a user hand-tweak generated geometry
//! (a rebuild from the seed would wipe the edits). `begin_procedural` and
//! `finish_procedural` factor that boilerplate out so each generator only writes
//! its own generation logic — and any future generator gets the contract free.

use anyhow::Result;

use mogen_core::{NodeId, SceneGraph};

use crate::ast::Node;

use super::helpers::transform_from_attrs;
use super::node::apply_metadata;

/// Create the editable wrapper node for a procedural generator and capture the
/// node count just before generation begins. The wrapper carries the source
/// node's transform, span, `use_id`, origin, and declared metadata (role / tags
/// / `mat=`). Returns `(wrapper_id, pre_expand_count)`; pass `pre_expand_count`
/// to [`finish_procedural`] once the subtree is emitted.
pub(super) fn begin_procedural(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<(NodeId, usize)> {
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
    Ok((wrapper_id, pre_expand_count))
}

/// Stamp every node emitted since `pre_expand_count` (the generator's whole
/// subtree) non-editable. The wrapper itself stays editable so the user can
/// still tweak the generator's own attrs.
pub(super) fn finish_procedural(graph: &mut SceneGraph, pre_expand_count: usize) {
    for i in pre_expand_count..graph.nodes.len() {
        graph.nodes[i].editable = false;
    }
}
