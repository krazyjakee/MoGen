use std::f32::consts::TAU;

use anyhow::{anyhow, bail, Result};
use glam::{Quat, Vec3};

use mogen_core::{subtree_local_aabb, Aabb, NodeId, SceneGraph, Transform};

use crate::ast::{Node, Value};
use crate::attach::resolve_attaches_in_scope;
use crate::conform::resolve_conforms_in_scope;

use super::connector::{add_aabb_connectors_if_missing, add_connector};
use super::helpers::{axis_vec3, string_or_ident, transform_from_attrs};
use super::node::{apply_metadata, lower_into};

/// Stack-layout container: lay children out along one axis using each child's
/// own AABB as its "slot" size. Removes all the half-extent arithmetic that
/// LLMs reliably get wrong when tiling boxes next to each other.
///
/// Layout model:
///   * `axis` — `x` | `y` | `z` (default `y`). Primary stacking direction.
///   * `gap` — constant spacing inserted between consecutive children.
///   * `align` — `center` | `start` | `end` (default `center`). Alignment on
///     the two perpendicular axes.
///   * `pack` — `start` | `center` | `end` (default `start`). Where the whole
///     stack sits along the axis: `start` keeps the first child at origin;
///     `center` centres the stack around the origin; `end` puts the last
///     child's far face at origin.
///
/// Each child keeps its own declared `pos` as an additive offset inside its
/// slot — so the LLM can still nudge a slotted panel by `y=0.01` without
/// rewriting the stack.
pub(super) fn expand_stack(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let axis_idx = stack_axis_index(node);
    let gap = node.attr_number("gap").unwrap_or(0.0);
    let align = string_or_ident(node.attr("align")).unwrap_or("center").to_string();
    let pack = string_or_ident(node.attr("pack")).unwrap_or("start").to_string();

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

    let mut child_ids: Vec<NodeId> = Vec::new();
    for c in &node.children {
        match c.kind.as_str() {
            "material" | "attach" | "conform" => continue,
            "connector" => { add_connector(c, wrapper_id, graph)?; }
            _ => {
                let id = lower_into(c, Some(wrapper_id), graph)?;
                child_ids.push(id);
            }
        }
    }

    // Measure each child in the wrapper's local frame (AABB of its subtree
    // transformed by its own TRS). Children without geometry contribute no
    // extent but still receive any perpendicular alignment shift of 0.
    let extents: Vec<Option<Aabb>> = child_ids
        .iter()
        .map(|id| {
            let subtree = subtree_local_aabb(graph, *id)?;
            let m = graph.nodes[id.0 as usize].transform.to_mat4();
            Some(subtree.transformed(m))
        })
        .collect();

    let axis_pick = |v: Vec3, i: usize| match i { 0 => v.x, 1 => v.y, _ => v.z };
    let set_axis = |v: &mut Vec3, i: usize, val: f32| match i {
        0 => v.x = val,
        1 => v.y = val,
        _ => v.z = val,
    };

    let mut offsets: Vec<Vec3> = vec![Vec3::ZERO; child_ids.len()];
    let mut cursor: f32 = 0.0;
    let mut first = true;
    for (i, ext) in extents.iter().enumerate() {
        let Some(aabb) = ext else { continue };
        let ax_min = axis_pick(aabb.min, axis_idx);
        let ax_max = axis_pick(aabb.max, axis_idx);
        if first { first = false; } else { cursor += gap; }
        let mut o = Vec3::ZERO;
        set_axis(&mut o, axis_idx, cursor - ax_min);
        for other in 0..3 {
            if other == axis_idx { continue; }
            let o_min = axis_pick(aabb.min, other);
            let o_max = axis_pick(aabb.max, other);
            let shift = match align.as_str() {
                "start" => -o_min,
                "end" => -o_max,
                _ => -(o_min + o_max) * 0.5, // center
            };
            set_axis(&mut o, other, shift);
        }
        offsets[i] = o;
        cursor += ax_max - ax_min;
    }
    let total_extent = cursor;
    let pack_shift = match pack.as_str() {
        "center" => -total_extent * 0.5,
        "end" => -total_extent,
        _ => 0.0,
    };
    for (i, id) in child_ids.iter().enumerate() {
        let mut o = offsets[i];
        let cur = axis_pick(o, axis_idx);
        set_axis(&mut o, axis_idx, cur + pack_shift);
        let t = &mut graph.nodes[id.0 as usize].transform;
        t.translation += o;
    }

    add_aabb_connectors_if_missing(wrapper_id, graph);
    Ok(wrapper_id)
}

fn stack_axis_index(node: &Node) -> usize {
    match string_or_ident(node.attr("axis")) {
        Some("x") | Some("X") => 0,
        Some("z") | Some("Z") => 2,
        _ => 1, // default y
    }
}

/// N-dimensional grid replicator. Creates `count.x * count.y * count.z`
/// copies of the body, each offset by `step * [i, j, k]`. When `center=1`
/// the grid is centred on the wrapper's origin (useful for checkerboards
/// and floor tilings where the whole pattern should sit around `[0,0,0]`).
pub(super) fn expand_grid(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let count = match node.attr("count") {
        Some(Value::Vec3(v)) => [v[0].max(1.0) as u32, v[1].max(1.0) as u32, v[2].max(1.0) as u32],
        Some(Value::Number(n)) => [n.max(1.0) as u32, 1, 1],
        Some(Value::List(v)) if v.len() == 3 => {
            [v[0].max(1.0) as u32, v[1].max(1.0) as u32, v[2].max(1.0) as u32]
        }
        Some(Value::List(v)) if v.len() == 2 => {
            [v[0].max(1.0) as u32, 1, v[1].max(1.0) as u32]
        }
        _ => [1u32, 1, 1],
    };
    let step = match node.attr("step") {
        Some(Value::Vec3(v)) => Vec3::from_array(*v),
        Some(Value::Number(n)) => Vec3::splat(*n),
        Some(Value::List(v)) if v.len() == 3 => Vec3::new(v[0], v[1], v[2]),
        Some(Value::List(v)) if v.len() == 2 => Vec3::new(v[0], 0.0, v[1]),
        _ => Vec3::ZERO,
    };
    let center = node.attr_number("center").unwrap_or(0.0) != 0.0;

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

    let center_offset = if center {
        Vec3::new(
            (count[0] as f32 - 1.0) * step.x * 0.5,
            (count[1] as f32 - 1.0) * step.y * 0.5,
            (count[2] as f32 - 1.0) * step.z * 0.5,
        )
    } else {
        Vec3::ZERO
    };

    for k in 0..count[2] {
        for j in 0..count[1] {
            for i in 0..count[0] {
                let offset = Vec3::new(
                    i as f32 * step.x,
                    j as f32 * step.y,
                    k as f32 * step.z,
                ) - center_offset;
                let instance_name = format!("{wrapper_name}_{i}_{j}_{k}");
                let iid = graph.add_child(
                    wrapper_id,
                    instance_name,
                    "group",
                    Transform::from_translation(offset),
                );
                for c in &node.children {
                    match c.kind.as_str() {
                        "material" | "attach" | "conform" => continue,
                        "connector" => { add_connector(c, iid, graph)?; }
                        _ => { lower_into(c, Some(iid), graph)?; }
                    }
                }
                resolve_attaches_in_scope(&node.children, graph, iid)?;
                resolve_conforms_in_scope(&node.children, graph, iid)?;
                add_aabb_connectors_if_missing(iid, graph);
            }
        }
    }
    // Every scene node created inside the grid body is a replica — multiple
    // scene nodes can point at the same AST span, so they can't be rewritten
    // independently. Clear the editable flag for everything produced here.
    for i in pre_expand_count..graph.nodes.len() {
        graph.nodes[i].editable = false;
    }
    add_aabb_connectors_if_missing(wrapper_id, graph);
    Ok(wrapper_id)
}

/// Resolve `above`/`below`/`left_of`/`right_of`/`in_front_of`/`behind`
/// against a prior sibling: translate `self` so the matching face is flush
/// with the target's opposite face (plus optional `gap`). At most one of
/// these attrs may be set on a node.
///
/// The lookup is scoped to earlier siblings under `parent_id` so instances
/// in a replicator (`array`, `mirror`, `grid`) don't collide with identically
/// named nodes elsewhere in the graph.
pub(super) fn apply_relative_placement(
    node: &Node,
    id: NodeId,
    parent_id: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    // (attr, axis_idx, sign): sign=+1 means target's max face → self's min face.
    const REL: &[(&str, usize, f32)] = &[
        ("above", 1, 1.0),
        ("below", 1, -1.0),
        ("right_of", 0, 1.0),
        ("left_of", 0, -1.0),
        ("behind", 2, 1.0),
        ("in_front_of", 2, -1.0),
    ];
    let mut chosen: Option<(&str, usize, f32, String)> = None;
    for (attr, axis, sign) in REL {
        if let Some(target) = string_or_ident(node.attr(attr)) {
            if chosen.is_some() {
                bail!(
                    "node `{}` sets more than one of above/below/left_of/right_of/in_front_of/behind — pick one",
                    node.name.as_deref().unwrap_or(&node.kind)
                );
            }
            chosen = Some((*attr, *axis, *sign, target.to_string()));
        }
    }
    let Some((_attr, axis_idx, sign, target_name)) = chosen else {
        return Ok(());
    };

    // Explicit `pos`/`x`/`y`/`z`/`from`+`to` along the placement axis wins
    // over the snap. Without this, `slab "x" (behind="y", pos=[0,0,0.75])`
    // silently has its `pos.z` overwritten with the flush-behind shift,
    // which surprised users who expected `pos` to be authoritative.
    if pos_axis_explicit(node, axis_idx) {
        return Ok(());
    }

    // Only search prior siblings under `parent_id`, skipping self. Using
    // `iter().find()` on the parent's children list preserves declaration
    // order implicitly.
    let target_id = {
        let parent = &graph.nodes[parent_id.0 as usize];
        parent.children.iter().copied()
            .find(|c| *c != id && graph.nodes[c.0 as usize].name == target_name)
            .ok_or_else(|| anyhow!(
                "relative-placement target `{target_name}` not found among prior siblings of `{}`",
                node.name.as_deref().unwrap_or(&node.kind)
            ))?
    };

    let gap = node.attr_number("gap").unwrap_or(0.0);

    let Some(target_local) = subtree_local_aabb(graph, target_id) else {
        // Target has no geometry — nothing to snap to.
        return Ok(());
    };
    let target_xform = graph.nodes[target_id.0 as usize].transform.to_mat4();
    let target_box = target_local.transformed(target_xform);

    let Some(self_local) = subtree_local_aabb(graph, id) else {
        return Ok(());
    };
    let self_xform = graph.nodes[id.0 as usize].transform.to_mat4();
    let self_box = self_local.transformed(self_xform);

    let pick = |v: Vec3, i: usize| match i { 0 => v.x, 1 => v.y, _ => v.z };
    let (t_min, t_max) = (pick(target_box.min, axis_idx), pick(target_box.max, axis_idx));
    let (s_min, s_max) = (pick(self_box.min, axis_idx), pick(self_box.max, axis_idx));

    // +1 means "self sits at higher coord than target" → target_max → self_min.
    let shift = if sign > 0.0 {
        t_max + gap - s_min
    } else {
        t_min - gap - s_max
    };

    let node_mut = &mut graph.nodes[id.0 as usize];
    match axis_idx {
        0 => node_mut.transform.translation.x += shift,
        1 => node_mut.transform.translation.y += shift,
        _ => node_mut.transform.translation.z += shift,
    }
    // Flag so the viewport gizmo can refuse to edit this node directly: its
    // translation is recomputed from the target's AABB on every compile, so
    // a plain `pos=` writeback would get overwritten.
    node_mut.relative_placed = true;
    Ok(())
}

/// Did the author explicitly place this node along `axis_idx`?
/// Mirrors the components consumed by `resolve_pos`: the `pos` vec3, the
/// scalar `x`/`y`/`z` shortcuts, and the corner-form `from`+`to` midpoint.
/// Only a non-zero contribution counts as "set" — `pos=[0,0,0]` is the
/// same as no `pos` at all and lets the snap fire.
fn pos_axis_explicit(node: &Node, axis_idx: usize) -> bool {
    if let (Some(a), Some(b)) = (node.attr_vec3("from"), node.attr_vec3("to")) {
        let mid = (a + b) * 0.5;
        let v = match axis_idx { 0 => mid.x, 1 => mid.y, _ => mid.z };
        if v != 0.0 {
            return true;
        }
    }
    let shortcut = match axis_idx { 0 => "x", 1 => "y", _ => "z" };
    if let Some(n) = node.attr_number(shortcut) {
        if n != 0.0 {
            return true;
        }
    }
    if let Some(p) = node.attr_vec3("pos") {
        let v = match axis_idx { 0 => p.x, 1 => p.y, _ => p.z };
        if v != 0.0 {
            return true;
        }
    }
    false
}

pub(super) fn expand_replicator(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let wrapper_name = node.name.clone().unwrap_or_else(|| node.kind.clone());
    let wrapper_transform = transform_from_attrs(node);
    let wrapper_id = match parent {
        None => graph.add_root(&wrapper_name, &node.kind, wrapper_transform),
        Some(p) => graph.add_child(p, &wrapper_name, &node.kind, wrapper_transform),
    };
    graph.set_source_span(wrapper_id, node.span);
    graph.nodes[wrapper_id.0 as usize].use_id = node.use_id;
    graph.nodes[wrapper_id.0 as usize].origin = node.origin.clone();
    let pre_expand_count = graph.nodes.len();

    let instance_transforms: Vec<Transform> = match node.kind.as_str() {
        "mirror" => {
            let axis = node
                .attr("axis")
                .and_then(axis_vec3)
                .unwrap_or(Vec3::X);
            let s = Vec3::ONE - 2.0 * axis.normalize_or_zero().abs();
            // Clamp to [-1, 1]: +axis component becomes -1.
            let mirror_scale = Vec3::new(
                if axis.x.abs() > 0.5 { -1.0 } else { 1.0 },
                if axis.y.abs() > 0.5 { -1.0 } else { 1.0 },
                if axis.z.abs() > 0.5 { -1.0 } else { 1.0 },
            );
            let _ = s; // Reserved for arbitrary-axis mirror in future.
            vec![Transform::IDENTITY, Transform::from_trs(Vec3::ZERO, Quat::IDENTITY, mirror_scale)]
        }
        "array" => {
            let count = node.attr_number("count").unwrap_or(1.0).max(1.0) as u32;
            let axis = node.attr("around").and_then(axis_vec3).unwrap_or(Vec3::Y);
            let axis = axis.normalize_or(Vec3::Y);
            let start = node.attr_number("start_angle").unwrap_or(0.0).to_radians();
            (0..count)
                .map(|i| {
                    let a = start + TAU * (i as f32) / (count as f32);
                    Transform::from_trs(Vec3::ZERO, Quat::from_axis_angle(axis, a), Vec3::ONE)
                })
                .collect()
        }
        _ => unreachable!(),
    };

    for (i, t) in instance_transforms.iter().enumerate() {
        let instance_name = format!("{wrapper_name}_{i}");
        let iid = graph.add_child(wrapper_id, instance_name, "group", *t);
        for c in &node.children {
            match c.kind.as_str() {
                "material" | "attach" | "conform" => continue,
                "connector" => add_connector(c, iid, graph)?,
                _ => { lower_into(c, Some(iid), graph)?; }
            }
        }
        // Resolve attach + conform specs declared inside the replicator body,
        // scoped to this instance's subtree. Without this, every copy would
        // resolve parent/child against the first instance's nodes (name collision).
        resolve_attaches_in_scope(&node.children, graph, iid)?;
        resolve_conforms_in_scope(&node.children, graph, iid)?;
        add_aabb_connectors_if_missing(iid, graph);
    }
    // Each replicator instance is a synthetic copy — multiple scene nodes can
    // map to the same AST span, so their transforms can't be rewritten back
    // one-at-a-time. The wrapper (array/mirror) itself stays editable so the
    // user can tweak it.
    for i in pre_expand_count..graph.nodes.len() {
        graph.nodes[i].editable = false;
    }
    add_aabb_connectors_if_missing(wrapper_id, graph);

    Ok(wrapper_id)
}
