//! `dungeon` node lowering.
//!
//! Top-level expander for `dungeon "name" (...)`. A spin-off of `cave`: instead
//! of an organic carved void it builds a tile-based dungeon crawler — grid-snapped
//! rectangular rooms joined by axis-aligned corridors, stacked into `levels`
//! floors that staircases connect. Like every procedural generator the wrapper
//! node stays editable while the whole subtree below it is stamped non-editable
//! (a deterministic function of `seed=` + attrs, so a rebuild wipes hand edits).
//!
//! The pipeline is config → generate (room placement + corridor spanning tree +
//! stair threading + exterior entrance) → emit (watertight box decks / walls /
//! stepped flights) → POIs (entrance, spawn, treasure rooms, stair heads/feet,
//! prop spots).

mod config;
mod emit;
mod generate;
mod materials;
mod poi;
mod rng;

#[cfg(test)]
mod tests;

use anyhow::Result;

use mogen_core::{NodeId, SceneGraph};

use crate::ast::Node;
use crate::lower::procedural::{begin_procedural, finish_procedural};

pub(super) fn expand_dungeon(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let cfg = config::read_cfg(node);

    // Binds the user's `mat=` to the wrapper so every box inherits it; the
    // validator allows `mat=` on `dungeon` via GEOMETRY_COMMON.
    let (wrapper_id, pre_expand_count) = begin_procedural(node, parent, graph)?;

    // Stamp default floor / stone materials before emitting so the geometry can
    // bind to them. Anything the user declared on this origin wins.
    materials::ensure_defaults(graph, node.origin.as_deref());

    let layout = generate::generate(&cfg);
    emit::emit(node, &cfg, &layout, wrapper_id, graph);
    poi::emit_pois(node, &cfg, &layout, wrapper_id, graph);

    finish_procedural(graph, pre_expand_count);

    Ok(wrapper_id)
}
