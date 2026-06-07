//! `terrain` node lowering.
//!
//! Top-level expander for `terrain "name" (...)`. Like `cave`/`building`, the
//! wrapper node stays editable while everything below it is stamped
//! non-editable: the whole subtree is a deterministic function of `seed=` plus
//! the declared attrs, so a rebuild would wipe any hand edits.
//!
//! The pipeline is config → field (source + retouch passes, see `field.rs`) →
//! emit (a grid of crack-free chunk meshes + optional water plane) → POIs
//! (peaks / flat spots / shoreline). The intermediate `HeightField` is built
//! once and shared by emit and POI so both read identical heights.

mod carve;
mod config;
mod emit;
mod field;
mod materials;
mod poi;

#[cfg(test)]
mod tests;

use anyhow::Result;

use mogen_core::{NodeId, SceneGraph};

use crate::ast::Node;
use crate::lower::procedural::{begin_procedural, finish_procedural};

pub(super) fn expand_terrain(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let cfg = config::read_cfg(node);

    // Binds the user's `mat=` (ground finish) to the wrapper so chunks inherit
    // it; the validator allows `mat=` on `terrain` via GEOMETRY_COMMON.
    let (wrapper_id, pre_expand_count) = begin_procedural(node, parent, graph)?;

    materials::ensure_defaults(graph, node.origin.as_deref());

    let mut field = field::build(&cfg);
    // Roads flatten the field before emit so the chunk seams/skirts stay valid;
    // holes are applied during emit (cells dropped + rim walled). Both read the
    // wrapper's child declarations.
    let roads = carve::read_roads(node);
    carve::carve_roads(&mut field, &cfg, &roads);
    let holes = carve::read_holes(node, &cfg, &field);
    emit::emit_chunks(node, &cfg, &field, &holes, wrapper_id, graph);
    poi::emit_pois(node, &cfg, &field, wrapper_id, graph);

    finish_procedural(graph, pre_expand_count);

    Ok(wrapper_id)
}
