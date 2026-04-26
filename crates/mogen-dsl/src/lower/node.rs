use anyhow::{anyhow, bail, Result};
use glam::Vec3;

use mogen_core::{Connector, NodeId, SceneGraph};

use crate::ast::{Node, Value};

use super::branch::expand_branch;
use super::connector::{add_aabb_connectors_if_missing, add_connector, default_connectors};
use super::csg::lower_csg;
use super::helpers::{
    anchor_for, apply_anchor_to_mesh, inherit_material_from_ancestor, transform_from_attrs,
};
use super::layout::{apply_relative_placement, expand_grid, expand_replicator, expand_stack};
use super::primitive::primitive_mesh;

pub(super) fn lower_into(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    if node.kind == "mirror" || node.kind == "array" {
        return expand_replicator(node, parent, graph);
    }
    if matches!(node.kind.as_str(), "union" | "difference" | "intersect") {
        return lower_csg(node, parent, graph);
    }
    if node.kind == "stack" {
        return expand_stack(node, parent, graph);
    }
    if node.kind == "grid" {
        return expand_grid(node, parent, graph);
    }
    if node.kind == "branch" {
        return expand_branch(node, parent, graph);
    }

    let transform = transform_from_attrs(node);
    let name = node.name.clone().unwrap_or_else(|| node.kind.clone());

    let id = match parent {
        None => graph.add_root(&name, &node.kind, transform),
        Some(p) => graph.add_child(p, &name, &node.kind, transform),
    };
    graph.set_source_span(id, node.span);

    // Metadata: role, tags (comma-separated string).
    if let Some(Value::String(role)) = node.attr("role") {
        graph.nodes[id.0 as usize].role = Some(role.clone());
    } else if let Some(Value::Ident(role)) = node.attr("role") {
        graph.nodes[id.0 as usize].role = Some(role.clone());
    }
    if let Some(Value::String(tags)) = node.attr("tags") {
        graph.nodes[id.0 as usize].tags =
            tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    }

    // Material lookup.
    if let Some(Value::String(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material(mat_name)
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    } else if let Some(Value::Ident(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material(mat_name)
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    }
    // Inherit from nearest ancestor when this node has no own `mat=`. Runs
    // before uv_mode is read so primitive UVs reflect the inherited material.
    inherit_material_from_ancestor(id, graph);

    let anchor = anchor_for(node);
    let mut anchor_shift = Vec3::ZERO;
    let uv_mode = graph.nodes[id.0 as usize]
        .material
        .and_then(|mid| graph.materials.get(mid.0 as usize))
        .map(|m| m.uv_mode)
        .unwrap_or_default();
    if let Some(mut mesh) = primitive_mesh(node, uv_mode) {
        anchor_shift = apply_anchor_to_mesh(&mut mesh, anchor.as_deref());
        graph.set_mesh(id, mesh);
    } else {
        match node.kind.as_str() {
            "group" | "scene" => {}
            "solid" => {
                // Export-time merge + optional coplanar cleanup read these tags.
                // See mogen-export::merge::merge_solid_groups.
                let n = &mut graph.nodes[id.0 as usize];
                if !n.tags.iter().any(|t| t == "solid") {
                    n.tags.push("solid".into());
                }
                let cleanup = node.attr("cleanup").and_then(|v| match v {
                    Value::String(s) | Value::Ident(s) => Some(s.as_str()),
                    _ => None,
                });
                if matches!(cleanup, Some("coplanar")) {
                    graph.nodes[id.0 as usize].tags.push("cleanup=coplanar".into());
                }
            }
            "material" => bail!("`material` must be a top-level or scene-level declaration"),
            other => bail!("unknown node kind: {}", other),
        }
    }

    // Expose canonical connectors (top/bottom/etc.) for primitives, derived
    // from the declared size/radius/height. User-declared `connector` children
    // further down replace these by name. Default connectors live in the
    // primitive's natural frame, so they move with the anchor shift to stay
    // flush with their face.
    for (name, at, dir) in default_connectors(node) {
        graph.nodes[id.0 as usize].connectors.push(Connector::from_at_dir(
            name.to_string(),
            at + anchor_shift,
            dir,
            String::new(),
            None,
        ));
    }

    for c in &node.children {
        match c.kind.as_str() {
            "material" | "attach" => continue,
            "connector" => {
                add_connector(c, id, graph)?;
            }
            _ => {
                lower_into(c, Some(id), graph)?;
            }
        }
    }

    // Groups pick up six face connectors (top/bottom/left/right/front/back)
    // synthesized from the subtree AABB. User-declared connectors with the
    // same name already took precedence via `add_connector`'s replace-by-name,
    // so we only push names that aren't present.
    if node.kind == "group" || node.kind == "solid" {
        add_aabb_connectors_if_missing(id, graph);
    }

    // Relative placement: translate this node so its face lines up flush with
    // a prior sibling's face (plus optional `gap`). Must run after children
    // are lowered so the self-AABB includes nested geometry.
    if let Some(parent_id) = parent {
        apply_relative_placement(node, id, parent_id, graph)?;
    }

    Ok(id)
}

pub(super) fn apply_metadata(node: &Node, id: NodeId, graph: &mut SceneGraph) -> Result<()> {
    if let Some(Value::String(role)) = node.attr("role") {
        graph.nodes[id.0 as usize].role = Some(role.clone());
    } else if let Some(Value::Ident(role)) = node.attr("role") {
        graph.nodes[id.0 as usize].role = Some(role.clone());
    }
    if let Some(Value::String(tags)) = node.attr("tags") {
        graph.nodes[id.0 as usize].tags =
            tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    }
    if let Some(Value::String(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material(mat_name)
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    } else if let Some(Value::Ident(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material(mat_name)
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    }
    Ok(())
}
