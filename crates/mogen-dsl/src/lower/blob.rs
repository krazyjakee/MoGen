//! Lowering for the `blob` container — true SDF + surface-nets meshing.
//!
//! Distinct from both `metaball` (sphere-cluster, vertex-fillet) and
//! `union(smooth=k)` (mesh CSG + vertex pull): blob walks every child as an
//! implicit primitive, evaluates the smooth-blended field on a voxel grid,
//! and extracts the zero-isosurface with `fast-surface-nets`. This is what
//! makes smooth eye sockets / nostrils / blended chin masses on a skull
//! actually look organic instead of staircased.
//!
//! Children supported (`SdfPrim`): `sphere`, `ellipsoid`, `box`,
//! `rounded_box`, `capsule`, `cylinder`, `torus`. Each accepts the same
//! `pos` / `rot` / `scale` / size attrs the mesh primitives do, plus an
//! optional `op="subtract"` to carve a smooth cavity rather than add mass.

use anyhow::{anyhow, bail, Result};
use glam::Vec3;

use mogen_core::{NodeId, SceneGraph};
use mogen_geom::{blob_to_mesh, BlobChild, SdfOp, SdfPrim};

use crate::ast::{Node, Value};

use super::connector::{add_aabb_connectors_if_missing, add_connector};
use super::helpers::{apply_subdivide, inherit_material_from_ancestor, resolve_size3, transform_from_attrs};
use super::lod::scaled_count;

pub(super) fn lower_blob(
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
    graph.nodes[id.0 as usize].use_id = node.use_id;
    graph.nodes[id.0 as usize].origin = node.origin.clone();

    super::node::apply_metadata(node, id, graph)?;

    // Material inheritance: blob's own mat= wins; otherwise first additive
    // child's mat=; otherwise ancestor chain. Matches `lower_csg`.
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
                if let Some(mid) =
                    graph.find_material_scoped(&name, first_operand.origin.as_deref())
                {
                    graph.set_material(id, mid);
                }
            }
        }
    }
    inherit_material_from_ancestor(id, graph);

    // Collect SDF children.
    let mut children: Vec<BlobChild> = Vec::new();
    for c in &node.children {
        match c.kind.as_str() {
            "material" | "connector" => continue,
            _ => children.push(parse_blob_child(c)?),
        }
    }
    if children.is_empty() {
        bail!(
            "`blob` requires at least one implicit-field primitive child (sphere, ellipsoid, box, rounded_box, capsule, cylinder, torus)"
        );
    }
    // At least one Add child is required — a blob of only subtracts has
    // nothing to carve into and produces an empty mesh.
    if !children.iter().any(|c| matches!(c.op, SdfOp::Add)) {
        bail!(
            "`blob` requires at least one additive child; every child has `op=subtract`"
        );
    }

    let blend = node.attr_number("blend").unwrap_or(0.1).max(0.0);
    // Voxel grid resolution is dimensional (count per axis), so it scales
    // linearly with LOD just like primitive `segments=`. The hard upper cap
    // (256) stays — `scaled_count` only enforces the floor — so a global
    // `lod_scale=2.0` on a hero asset can't blow up the voxel budget.
    let raw_resolution = node.attr_number("resolution").map(|n| n as u32).unwrap_or(96);
    let resolution = scaled_count(raw_resolution, 16).min(256);

    let mut mesh = blob_to_mesh(&children, blend, resolution);
    if mesh.indices.is_empty() {
        bail!(
            "`blob` produced no surface — check that additive children overlap and `resolution` is large enough"
        );
    }
    mesh = apply_subdivide(node, mesh)?;

    graph.set_mesh(id, mesh);

    for c in &node.children {
        if c.kind == "connector" {
            add_connector(c, id, graph)?;
        }
    }
    add_aabb_connectors_if_missing(id, graph);
    Ok(id)
}

fn parse_blob_child(node: &Node) -> Result<BlobChild> {
    let op = match node.attr_string("op") {
        Some("subtract") | Some("sub") | Some("carve") => SdfOp::Subtract,
        Some("add") | None => SdfOp::Add,
        Some(other) => {
            bail!("blob child `op=` must be \"add\" or \"subtract\" (got: \"{other}\")")
        }
    };
    let prim = parse_sdf_prim(node)?;
    let xform = transform_from_attrs(node).to_mat4();
    Ok(BlobChild::new(prim, op, xform))
}

fn parse_sdf_prim(node: &Node) -> Result<SdfPrim> {
    match node.kind.as_str() {
        "sphere" => {
            let r = node.attr_number("radius").unwrap_or(0.5).max(0.0);
            Ok(SdfPrim::Sphere { radius: r })
        }
        "ellipsoid" => {
            let s = resolve_size3(node, Vec3::ONE);
            Ok(SdfPrim::Ellipsoid { half: s * 0.5 })
        }
        "box" => {
            let s = resolve_size3(node, Vec3::ONE);
            Ok(SdfPrim::Box { half: s * 0.5 })
        }
        "rounded_box" => {
            let s = resolve_size3(node, Vec3::ONE);
            let r = node.attr_number("radius").unwrap_or(0.1).max(0.0);
            Ok(SdfPrim::RoundedBox { half: s * 0.5, radius: r })
        }
        "capsule" => {
            let r = node.attr_number("radius").unwrap_or(0.3).max(0.0);
            let h = node.attr_number("height").unwrap_or(1.0).max(0.0);
            Ok(SdfPrim::Capsule { radius: r, height: h })
        }
        "cylinder" => {
            let r = node.attr_number("radius").unwrap_or(0.5).max(0.0);
            let h = node.attr_number("height").unwrap_or(1.0).max(0.0);
            Ok(SdfPrim::Cylinder { radius: r, height: h })
        }
        "torus" => {
            let major = node.attr_number("major").unwrap_or(0.5).max(0.0);
            let minor = node.attr_number("minor").unwrap_or(0.15).max(0.0);
            Ok(SdfPrim::Torus { major, minor })
        }
        other => Err(anyhow!(
            "blob child kind `{other}` is not implicit-field-supported (allowed: sphere, ellipsoid, box, rounded_box, capsule, cylinder, torus)"
        )),
    }
}

