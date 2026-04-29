use anyhow::{bail, Result};

use mogen_core::{Mesh, NodeId, SceneGraph, UvMode};
use mogen_geom::{
    clean_csg_output, difference_many, intersect_many, transform_mesh, union_many, union_smooth,
};

use crate::ast::{Node, Value};

use super::connector::{add_aabb_connectors_if_missing, add_connector};
use super::helpers::{inherit_material_from_ancestor, transform_from_attrs};
use super::node::apply_metadata;
use super::primitive::primitive_mesh;

/// Lower a CSG node. Each child is evaluated into a single mesh (in the CSG
/// node's local space, with the child's own transform baked in); the boolean
/// op is applied left-to-right; the result is cleaned (weld/cull/normals) and
/// attached to the CSG node. CSG children do not become separate scene nodes.
pub(super) fn lower_csg(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let transform = transform_from_attrs(node);
    let name = node.name.clone().unwrap_or_else(|| node.kind.clone());

    let id = match parent {
        None => graph.add_root(&name, &node.kind, transform),
        Some(p) => graph.add_child(p, &name, &node.kind, transform),
    };
    graph.set_source_span(id, node.span);

    // Metadata + material exactly as for other kinds.
    apply_metadata(node, id, graph)?;

    // If the CSG node itself declares no material, inherit from the first
    // operand's `mat=`. Operands don't become separate scene nodes, so their
    // material would otherwise be silently dropped — and "hollow out this
    // brick dome" naturally should stay brick.
    if graph.nodes[id.0 as usize].material.is_none() {
        if let Some(first_operand) = node
            .children
            .iter()
            .find(|c| !matches!(c.kind.as_str(), "material" | "connector"))
        {
            let mat_name = match first_operand.attr("mat") {
                Some(Value::String(s)) | Some(Value::Ident(s)) => Some(s.clone()),
                _ => None,
            };
            if let Some(name) = mat_name {
                if let Some(mid) = graph.find_material(&name) {
                    graph.set_material(id, mid);
                }
            }
        }
    }
    // Fall back to lexical inheritance from the CSG node's parent chain when
    // neither the CSG node nor any operand declared a material. Runs before
    // uv_mode is read so operand UVs use the inherited material's convention.
    inherit_material_from_ancestor(id, graph);

    // Evaluate operand meshes. Connectors are allowed on the CSG node itself
    // (captured below) but silently skipped inside operand bodies. All
    // operands inherit the CSG node's `uv_mode` so the combined result is
    // texturally consistent — operand-level mat= refs on a CSG operand are
    // already discarded for the binding (only the first one influences the
    // result's material via the inheritance step above).
    let csg_uv_mode = graph.nodes[id.0 as usize]
        .material
        .and_then(|mid| graph.materials.get(mid.0 as usize))
        .map(|m| m.uv_mode)
        .unwrap_or_default();
    let mut operand_meshes: Vec<Mesh> = Vec::new();
    for c in &node.children {
        match c.kind.as_str() {
            "material" | "connector" => continue,
            _ => operand_meshes.push(eval_mesh(c, /*bake_transform=*/ true, csg_uv_mode)?),
        }
    }

    let combined = match node.kind.as_str() {
        "union" => {
            if operand_meshes.is_empty() {
                bail!("`union` requires at least one operand");
            }
            match node.attr_number("smooth") {
                Some(k) if k > 0.0 => union_smooth(&operand_meshes, k),
                _ => union_many(&operand_meshes),
            }
        }
        "difference" => {
            if operand_meshes.is_empty() {
                bail!("`difference` requires at least one operand");
            }
            let (first, rest) = operand_meshes.split_first().unwrap();
            difference_many(first, rest)
        }
        "intersect" => {
            if operand_meshes.len() < 2 {
                bail!("`intersect` requires at least two operands");
            }
            intersect_many(&operand_meshes)
        }
        _ => unreachable!(),
    };

    graph.set_mesh(id, clean_csg_output(&combined));

    // Attach connectors declared directly on the CSG node.
    for c in &node.children {
        if c.kind == "connector" {
            add_connector(c, id, graph)?;
        }
    }
    // Default face connectors from the combined mesh AABB.
    add_aabb_connectors_if_missing(id, graph);
    Ok(id)
}

/// Evaluate a DSL node into a single triangle mesh in its parent's space.
/// If `bake_transform` is true, the node's own pos/rot/scale is baked into the
/// vertices — used when folding operands into a CSG result. `uv_mode` is
/// inherited from the eventual material binding (CSG node's own material) so
/// every operand contributes UVs in the same convention.
fn eval_mesh(node: &Node, bake_transform: bool, uv_mode: UvMode) -> Result<Mesh> {
    let local = if let Some(mesh_res) = primitive_mesh(node, uv_mode) {
        mesh_res?
    } else {
        match node.kind.as_str() {
        "union" | "difference" | "intersect" => {
            let mut operands: Vec<Mesh> = Vec::new();
            for c in &node.children {
                match c.kind.as_str() {
                    "material" | "connector" => continue,
                    _ => operands.push(eval_mesh(c, true, uv_mode)?),
                }
            }
            match node.kind.as_str() {
                "union" => {
                    if operands.is_empty() {
                        bail!("`union` requires at least one operand");
                    }
                    match node.attr_number("smooth") {
                        Some(k) if k > 0.0 => union_smooth(&operands, k),
                        _ => union_many(&operands),
                    }
                }
                "difference" => {
                    if operands.is_empty() {
                        bail!("`difference` requires at least one operand");
                    }
                    let (first, rest) = operands.split_first().unwrap();
                    difference_many(first, rest)
                }
                "intersect" => {
                    if operands.len() < 2 {
                        bail!("`intersect` requires at least two operands");
                    }
                    intersect_many(&operands)
                }
                _ => unreachable!(),
            }
        }
        other => bail!("`{other}` is not allowed as a CSG operand"),
        }
    };

    if !bake_transform {
        return Ok(local);
    }
    let t = transform_from_attrs(node);
    if t.is_identity() {
        Ok(local)
    } else {
        Ok(transform_mesh(&local, t.to_mat4()))
    }
}
