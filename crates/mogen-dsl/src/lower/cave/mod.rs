//! `cave` node lowering.
//!
//! Top-level expander for `cave "name" (...) { feature … }`. Like `building`,
//! the wrapper node stays editable while everything below it is stamped
//! non-editable: the whole subtree is a deterministic function of `seed=` plus
//! the declared attrs, so a rebuild would wipe any hand edits.
//!
//! The pipeline is config → layout (chamber placement + slope-capped passages
//! + entrances) → emit (one surface-nets rock shell) → decorate (stalagmites,
//! stalactites, rock piles, pools, lakes). See `docs/caves.md` for the full
//! attribute surface.

mod config;
mod decorate;
mod emit;
mod generate;
mod materials;
mod poi;
mod rng;

#[cfg(test)]
mod tests;

use anyhow::Result;

use mogen_core::{ColliderShape, NodeId, SceneGraph};

use crate::ast::Node;
use crate::lower::cave::config::ColliderMode;
use crate::lower::helpers::transform_from_attrs;
use crate::lower::node::apply_metadata;

pub(super) fn expand_cave(
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
    // Binds the user's `mat=` (rock finish) to the wrapper so the rock mesh
    // inherits it; the validator allows `mat=` on `cave` via GEOMETRY_COMMON.
    apply_metadata(node, wrapper_id, graph)?;

    // Stamp default rock / water materials before emitting so the rock mesh
    // and water decorations can bind to them. Anything the user declared on
    // this origin wins.
    materials::ensure_defaults(graph, node.origin.as_deref());

    let pre_expand_count = graph.nodes.len();

    let layout = generate::generate(&cfg);
    emit::emit_rock(node, &cfg, &layout, wrapper_id, graph)?;
    let column_bases = decorate::emit_decorations(node, &cfg, &layout, wrapper_id, graph);
    poi::emit_points_of_interest(node, &cfg, &layout, &column_bases, wrapper_id, graph);

    // Stamp the whole generated subtree non-editable (a rebuild regenerates it
    // from the seed, so hand edits wouldn't survive).
    for i in pre_expand_count..graph.nodes.len() {
        graph.nodes[i].editable = false;
    }

    tag_cave_colliders(graph, pre_expand_count, cfg.colliders, cfg.water_collider);

    Ok(wrapper_id)
}

/// Tag mesh-bearing cave nodes with a collider so a game engine importer gets
/// working physics. Rock surfaces use a trimesh (their cavities and slopes are
/// not box-aligned). `mode` selects which rock surfaces collide; water surfaces
/// are decorative and only collide when `water_collider` is set (so a player
/// can wade in by default).
fn tag_cave_colliders(
    graph: &mut SceneGraph,
    start: usize,
    mode: ColliderMode,
    water_collider: bool,
) {
    for i in start..graph.nodes.len() {
        if graph.nodes[i].collider.is_some() || graph.nodes[i].mesh.is_none() {
            continue;
        }
        // POI debug markers (`debug_show_poi`) are a visual aid only — never
        // give them collision.
        if graph.nodes[i].tags.iter().any(|t| t == "poi") {
            continue;
        }
        let role = graph.nodes[i].role.as_deref();
        let is_water = matches!(role, Some("pool") | Some("lake"));
        if is_water {
            // Water collision is its own opt-in, independent of `mode`.
            if water_collider {
                graph.nodes[i].collider = Some(ColliderShape::Trimesh);
            }
            continue;
        }
        let is_shell = role == Some("cave_rock");
        let wants_collider = match mode {
            ColliderMode::None => false,
            ColliderMode::Shell => is_shell,
            ColliderMode::All => true,
        };
        if wants_collider {
            graph.nodes[i].collider = Some(ColliderShape::Trimesh);
        }
    }
}
