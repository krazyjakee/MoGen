use anyhow::{anyhow, bail, Result};
use glam::Vec3;

use mogen_core::{AlphaMode, Connector, Material, NodeId, SceneGraph, TextureRef, UvMode};

use crate::ast::{Node, Value};

use crate::skin_lower::lower_skeleton;

use super::branch::expand_branch;
use super::connector::{add_aabb_connectors_if_missing, add_connector, default_connectors};
use super::csg::lower_csg;
use super::deform::apply_deform;
use super::helpers::{
    anchor_for, apply_anchor_to_mesh, inherit_material_from_ancestor, transform_from_attrs,
};
use super::layout::{apply_relative_placement, expand_grid, expand_replicator, expand_stack};
use super::light::lower_light;
use super::lod::{LodMultiplierGuard, LodOriginScaleGuard};
use super::primitive::primitive_mesh;

pub(super) fn lower_into(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    // Switch the active LOD multiplier to whatever the imported file
    // (identified by `node.origin`) declared, if any. Geometry declared in
    // the user's own file has `origin == None` and falls through to the
    // top-level `LOD_SCALE`. The guard restores the previous scale on drop
    // so children with a different origin still get their own override.
    let _lod = LodOriginScaleGuard::for_origin(node.origin.as_deref());
    // Stack a per-node `lod=N` multiplier on top of the origin scale so a
    // `lod=2.0` group can boost detail in a hero subtree (and `lod=0.5`
    // can drop background parts) without touching the file-global
    // `lod_scale`. The multiplier guard compounds with whatever the
    // origin guard set up; both restore previous values on drop.
    let _lod_mul = LodMultiplierGuard::for_node(node);
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
    if node.kind == "light" {
        return lower_light(node, parent, graph);
    }

    // Validate the collider attribute up-front so a bad value fails fast even
    // for kinds (group, solid, geometry primitives) where the post-pass would
    // otherwise just leave `collider = None`.
    if let Some(s) = node.attr_string("collider") {
        if s != "aabb" {
            bail!(
                "collider value must be \"aabb\" (got: \"{s}\")"
            );
        }
    }

    let transform = transform_from_attrs(node);
    let name = node.name.clone().unwrap_or_else(|| node.kind.clone());

    let id = match parent {
        None => graph.add_root(&name, &node.kind, transform),
        Some(p) => graph.add_child(p, &name, &node.kind, transform),
    };
    graph.set_source_span(id, node.span);
    graph.nodes[id.0 as usize].use_id = node.use_id;
    graph.nodes[id.0 as usize].origin = node.origin.clone();

    // Record the request now (independent of subtree contents); the post-pass
    // in `lower_with_source` walks the resolved subtree and assigns the AABB.
    if matches!(node.attr_string("collider"), Some("aabb")) {
        super::COLLIDER_REQUESTS.with(|r| r.borrow_mut().push(id));
    }

    // Shadow opt-out: `cast_shadow=0` excludes the node's mesh from the
    // realtime shadow pre-pass and stamps `extras.cast_shadow=false` on the
    // exported glTF node. Default is true so existing scenes are unchanged.
    if let Some(v) = node.attr_number("cast_shadow") {
        graph.nodes[id.0 as usize].cast_shadow = v != 0.0;
    }

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

    // Material lookup. Scoped by `node.origin` so geometry imported from
    // another `.mog` file binds to that file's materials before falling back
    // to bare-name lookup.
    if let Some(Value::String(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material_scoped(mat_name, node.origin.as_deref())
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    } else if let Some(Value::Ident(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material_scoped(mat_name, node.origin.as_deref())
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    }
    // Decals own a synthesized transparent material instead of binding to a
    // user-declared one. Done before ancestor inheritance so a parent's
    // material can't leak in and override the alpha/double-sided/Fit-UV
    // settings the decal pipeline depends on.
    if node.kind == "decal" {
        let mid = graph.add_material(synthesize_decal_material(node, &name));
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
    if let Some(mesh_res) = primitive_mesh(node, uv_mode) {
        let mut mesh = mesh_res?;
        // Deformation runs before anchor shift so the anchor reflects the
        // post-deform AABB — the user's `anchor=bottom` still lines up flush
        // with the base of a bent or melted shape rather than its parametric
        // bounding box.
        if node.kind != "mesh" {
            apply_deform(&mut mesh, node);
        }
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
            // `conform` is resolved by `resolve_conforms`, which walks the
            // expanded AST recursively and finds conform nodes regardless of
            // nesting. Skipping it here matters when an imported scene-as-
            // module body (e.g. `sports_bag.mog`) carrying conform directives
            // is expanded inside a `group` wrapper.
            "material" | "attach" | "conform" => continue,
            // Animation and clip-track decls are processed by their own pass
            // (see lower_animations). They get here when an imported scene-as-
            // module body — which can carry animations alongside geometry — is
            // expanded inside a `group` or another wrapper. Skipping them
            // keeps the geometry pass focused on geometry.
            "joint" | "clip" | "track"
            | "spin" | "open_close" | "wave" | "flap" | "idle" => continue,
            // A skeleton nested inside a `group` (e.g. the user wrapped a
            // `use "humanoid_full" ()` in `group "humanoid" { ... }`) lowers
            // here with the group as parent. Scene-level skeletons are
            // special-cased earlier in `lower` so they land at the root.
            "skeleton" => {
                lower_skeleton(c, Some(id), graph)?;
            }
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
    if matches!(
        node.kind.as_str(),
        "group" | "solid" | "extrude" | "sweep" | "loft",
    ) {
        // Primitives whose geometry derives from arbitrary author-supplied
        // 2D contours (extrude / sweep / loft) don't have a closed-form
        // top/bottom/side connector layout the way `cylinder` or `box`
        // does, so we mirror the group fallback: synthesize the six face
        // connectors from the subtree AABB.
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

/// Build the per-decal `Material` that the lowered scene binds to a `decal`
/// node. Auto-named `__decal_<decal_name>` so a user's `mat=` can't reach it
/// (decals own their material outright); transparency, image mapping, and
/// double-sided rendering are forced regardless of any inherited settings on
/// a parent material.
fn synthesize_decal_material(node: &Node, decal_name: &str) -> Material {
    let mut mat = Material::new(format!("__decal_{decal_name}"));
    mat.alpha_mode = AlphaMode::Blend;
    mat.uv_mode = UvMode::Fit;
    mat.double_sided = true;
    mat.roughness = node.attr_number("roughness").unwrap_or(0.6);
    if let Some(t) = node.attr_vec3("tint") {
        mat.base_color = [t.x, t.y, t.z, 1.0];
    } else {
        mat.base_color = [1.0, 1.0, 1.0, 1.0];
    }
    if let Some(path) = node.attr_string("image") {
        mat.base_color_texture = Some(TextureRef::new(path.to_string()));
    }
    mat.origin = node.origin.clone();
    mat
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
            .find_material_scoped(mat_name, node.origin.as_deref())
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    } else if let Some(Value::Ident(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material_scoped(mat_name, node.origin.as_deref())
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    }
    Ok(())
}
