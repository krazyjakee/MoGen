use crate::ast::{Node, Value};
use anyhow::{anyhow, Result};
use glam::{Mat4, Vec3};
use mogen_core::{NodeId, Relationship, SceneGraph};
use std::collections::{HashMap, HashSet};

fn error(spec: &Node, message: impl std::fmt::Display) -> anyhow::Error {
    anyhow!(
        "E0150 at {}:{}..{}: {message}",
        spec.origin
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "source".into()),
        spec.span.start,
        spec.span.end
    )
}

pub(super) fn resolve(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    let mut all = Vec::new();
    fn walk<'a>(nodes: &'a [Node], replicated: bool, all: &mut Vec<&'a Node>) -> Result<()> {
        for node in nodes {
            if node.kind == "relate" && replicated {
                return Err(error(
                    node,
                    "relate inside a replicator is unsupported; instantiate a named module instead",
                ));
            }
            all.push(node);
            walk(
                &node.children,
                replicated || matches!(node.kind.as_str(), "array" | "mirror" | "grid" | "stack"),
                all,
            )?;
        }
        Ok(())
    }
    walk(ast, false, &mut all)?;
    let specs: Vec<_> = all.iter().copied().filter(|n| n.kind == "relate").collect();
    if specs.is_empty() {
        return Ok(());
    }
    let mut resolved = vec![];
    let mut writes: HashMap<NodeId, HashSet<String>> = HashMap::new();
    for spec in specs {
        let child = resolve_name(graph, spec, "child")?;
        let target = resolve_name(graph, spec, "target")?;
        if graph.is_ancestor(child, target) {
            return Err(error(spec, "target cannot be the child or its descendant"));
        }
        let mode = spec.attr_string("mode").unwrap_or("align");
        if !matches!(mode, "align" | "ground" | "endpoint") {
            return Err(error(spec, "mode must be align, ground or endpoint"));
        }
        let write = if mode == "endpoint" {
            let end = spec.attr_string("endpoint").unwrap_or("end");
            if !matches!(end, "start" | "end") {
                return Err(error(spec, "endpoint must be start or end"));
            }
            end
        } else {
            "rigid"
        };
        let set = writes.entry(child).or_default();
        if set.contains(write) || set.contains("rigid") || (write == "rigid" && !set.is_empty()) {
            return Err(error(
                spec,
                "conflicting relationships write the same child placement or endpoint",
            ));
        }
        set.insert(write.into());
        resolved.push((spec, child, target));
    }
    // An operation waits for every operation that can move its target or
    // parent frame. A stable scan preserves authored order between peers.
    let mut done = vec![false; resolved.len()];
    let mut order = vec![];
    while order.len() < resolved.len() {
        let before = order.len();
        for (i, (_, child, target)) in resolved.iter().enumerate() {
            if done[i] {
                continue;
            }
            let blocked = resolved.iter().enumerate().any(|(j, (_, driver, _))| {
                i != j
                    && !done[j]
                    && driver != child
                    && (graph.is_ancestor(*driver, *target) || graph.is_ancestor(*driver, *child))
            });
            if !blocked {
                done[i] = true;
                order.push(i);
            }
        }
        if before == order.len() {
            return Err(error(
                resolved[0].0,
                "cyclic relationship dependencies; remove a driver cycle",
            ));
        }
    }
    let mut next = graph.clone();
    let mut recipes: HashMap<NodeId, Node> = HashMap::new();
    for i in order {
        let (spec, child, target) = resolved[i];
        apply(spec, child, target, &all, &mut recipes, &mut next)?;
    }
    *graph = next;
    Ok(())
}

fn resolve_name(graph: &SceneGraph, spec: &Node, key: &str) -> Result<NodeId> {
    let name = spec
        .attr_string(key)
        .ok_or_else(|| error(spec, format!("{key} requires a node name")))?;
    let ids: Vec<_> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.name == name && graph.use_id_visible(spec.use_id, n.use_id))
        .map(|(i, _)| NodeId(i as u32))
        .collect();
    if ids.len() != 1 {
        return Err(error(
            spec,
            format!(
                "{key} {name:?} resolves to {} nodes; choose a unique name in this module scope",
                ids.len()
            ),
        ));
    }
    Ok(ids[0])
}

fn number(spec: &Node, key: &str, default: f32) -> Result<f32> {
    let n = spec.attr_number(key).unwrap_or(default);
    if !n.is_finite() || n < 0.0 {
        return Err(error(spec, format!("{key} must be finite and nonnegative")));
    }
    Ok(n)
}

fn apply(
    spec: &Node,
    child: NodeId,
    target: NodeId,
    all: &[&Node],
    recipes: &mut HashMap<NodeId, Node>,
    graph: &mut SceneGraph,
) -> Result<()> {
    let mode = spec.attr_string("mode").unwrap_or("align");
    let socket_name = spec.attr_string("socket").unwrap_or("top");
    let socket = graph
        .get(target)
        .connectors
        .iter()
        .find(|c| c.name == socket_name)
        .cloned()
        .ok_or_else(|| error(spec, format!("target has no connector {socket_name:?}")))?;
    let insertion = number(spec, "insertion", 0.0)?;
    let clearance = number(spec, "clearance", 0.0)?;
    let tolerance = number(spec, "tolerance", 0.0001)?;
    if insertion > 0.0 && clearance > 0.0 {
        return Err(error(spec, "choose insertion or clearance, not both"));
    }
    if tolerance == 0.0 {
        return Err(error(spec, "tolerance must be greater than zero"));
    }
    let offset = spec.attr_vec3("offset").unwrap_or(Vec3::ZERO);
    if !offset.is_finite() {
        return Err(error(spec, "offset must be finite"));
    }
    let worlds = graph.world_transforms();
    let tw = worlds[target.0 as usize];
    let cw = worlds[child.0 as usize];
    if !tw.inverse().is_finite() || !cw.inverse().is_finite() {
        return Err(error(
            spec,
            "relationship requires finite invertible parent transforms",
        ));
    }
    let normal_local = socket.rotation * Vec3::Y;
    let normal = tw
        .inverse()
        .transpose()
        .transform_vector3(normal_local)
        .normalize_or_zero();
    let target_local = socket.pos + offset;
    let destination = tw.transform_point3(target_local) + normal * (clearance - insertion);
    let anchor;
    let endpoint;
    if mode == "endpoint" {
        let original = graph.get(child);
        if !matches!(
            original.kind.as_str(),
            "sweep" | "spline_tube" | "spline_ribbon"
        ) || original.conform_binding.is_some()
            || original.skin.is_some()
            || !original.children.is_empty()
        {
            return Err(error(
                spec,
                "endpoint supports an unskinned, unconformed leaf sweep/spline path only",
            ));
        }
        if !recipes.contains_key(&child) {
            let candidates: Vec<_> = all
                .iter()
                .filter(|n| {
                    n.kind == original.kind
                        && n.use_id == original.use_id
                        && n.origin == original.origin
                        && Some(n.span) == original.source_span
                })
                .collect();
            if candidates.len() != 1 {
                return Err(error(
                    spec,
                    "cannot identify one authored path; edit an explicit named path",
                ));
            }
            recipes.insert(child, (**candidates[0]).clone());
        }
        let recipe = recipes.get_mut(&child).unwrap();
        if recipe.attrs.iter().any(|(k, _)| {
            k == "anchor"
                || k == "subdivide"
                || k.starts_with("bend_")
                || k.starts_with("twist_")
                || matches!(k.as_str(), "taper" | "droop" | "noise" | "jitter" | "wave")
        }) || recipe.attr_number("closed").unwrap_or(0.0) != 0.0
        {
            return Err(error(spec,"endpoint retessellation does not support anchor, subdivision, closed paths or deformation modifiers"));
        }
        let key = if recipe.kind == "sweep" {
            "path"
        } else {
            "points"
        };
        let mut points = recipe
            .attr_list_vec3(key)
            .ok_or_else(|| error(spec, "endpoint requires an explicit authored path"))?;
        if points.len() < 2 {
            return Err(error(spec, "path needs at least two controls"));
        }
        let end = spec.attr_string("endpoint").unwrap_or("end");
        let index = if end == "start" { 0 } else { points.len() - 1 };
        anchor = cw.inverse().transform_point3(destination);
        points[index] = anchor.to_array();
        for (k, v) in &mut recipe.attrs {
            if k == key {
                *v = Value::ListVec3(points.clone());
            }
        }
        let uv = original
            .material
            .and_then(|m| graph.materials.get(m.0 as usize))
            .map(|m| m.uv_mode)
            .unwrap_or_default();
        let mesh = super::primitive::primitive_mesh(recipe, uv)
            .ok_or_else(|| error(spec, "unsupported path primitive"))??
            .mesh;
        graph.set_mesh(child, mesh);
        graph.nodes[child.0 as usize].geometry_identity = None;
        graph.nodes[child.0 as usize].path_frame = None; // previous frame described the authored, unconstrained path
        graph.nodes[child.0 as usize]
            .connectors
            .retain(|c| c.source_span.is_some());
        super::connector::add_aabb_connectors_if_missing(child, graph);
        endpoint = Some(end.into());
    } else {
        let point = if mode == "align" {
            let plug = spec.attr_string("plug").unwrap_or("bottom");
            let c = graph
                .get(child)
                .connectors
                .iter()
                .find(|c| c.name == plug)
                .ok_or_else(|| error(spec, format!("child has no connector {plug:?}")))?;
            cw.transform_point3(c.pos)
        } else {
            let mut minimum: Option<Vec3> = None;
            for (i, node) in graph.nodes.iter().enumerate() {
                if !graph.is_ancestor(child, NodeId(i as u32)) {
                    continue;
                }
                if let Some(mesh) = &node.mesh {
                    for p in &mesh.positions {
                        let p = worlds[i].transform_point3(Vec3::from_array(*p));
                        if minimum.is_none_or(|old| p.dot(normal) < old.dot(normal)) {
                            minimum = Some(p);
                        }
                    }
                }
            }
            minimum.ok_or_else(|| error(spec, "grounded child has no surface vertices"))?
        };
        anchor = cw.inverse().transform_point3(point);
        let delta = if mode == "ground" {
            normal * (destination - point).dot(normal)
        } else {
            destination - point
        };
        let parent = graph
            .get(child)
            .parent
            .map(|p| worlds[p.0 as usize])
            .unwrap_or(Mat4::IDENTITY);
        if !parent.inverse().is_finite() {
            return Err(error(spec, "child parent transform is singular"));
        }
        graph.nodes[child.0 as usize].transform.translation +=
            parent.inverse().transform_vector3(delta);
        endpoint = None;
    }
    let mut ancestor = graph.get(child).parent;
    while let Some(id) = ancestor {
        if matches!(graph.get(id).kind.as_str(), "group" | "scene" | "solid") {
            graph.nodes[id.0 as usize]
                .connectors
                .retain(|c| c.source_span.is_some());
            super::connector::add_aabb_connectors_if_missing(id, graph);
        }
        ancestor = graph.get(id).parent;
    }
    graph.relationships.push(Relationship {
        mode: mode.into(),
        child,
        target,
        socket: socket_name.into(),
        endpoint,
        child_anchor: anchor.to_array(),
        target_anchor: target_local.to_array(),
        target_normal: normal_local.to_array(),
        insertion,
        clearance,
        tolerance,
        source_span: spec.span,
    });
    Ok(())
}
