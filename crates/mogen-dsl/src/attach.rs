//! `attach` primitive resolution.
//!
//! An `attach (parent="...", socket="...", child="...", plug="...", offset=0, twist=0)`
//! node says: position `child` so its `plug` connector coincides with `parent`'s
//! `socket` connector, with `plug` pointing anti-parallel to `socket` (so the
//! parts meet instead of facing away from each other). Then reparent `child`
//! under `parent` so subsequent transforms propagate.
//!
//! Every primitive exposes a default set of connectors (e.g. `top`/`bottom` on
//! a cylinder, six faces on a box) derived from its dimensions — the LLM does
//! not have to declare them. User-declared connectors with the same name
//! override the defaults.
//!
//! This pass runs after the scene graph is built and before skin binding so
//! bind-pose world matrices reflect post-attach positions.
//!
//! The ordering is topological over parent→child chains: if A is attached to B,
//! which is attached to C, resolve C first, then B (now positioned relative to
//! C), then A. A cycle — or an attach targeting a descendant of its child —
//! is a hard error.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};
use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Span, Transform};

use crate::ast::{Node, Value};

#[derive(Debug)]
struct AttachSpec {
    parent: String,
    child: String,
    socket: String,
    plug: String,
    offset: f32,
    twist_deg: f32,
    #[allow(dead_code)]
    span: Span,
}

/// Resolve every `attach` declared at AST scope (scene root, group bodies).
///
/// Attaches inside an `array`/`mirror` subtree are NOT visited here — the
/// replicator handles them per-instance via [`resolve_attaches_in_scope`] so
/// each replicated copy resolves parent/child against its own subtree.
pub fn resolve_attaches(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    let specs = collect_attaches(ast)?;
    apply_specs(&specs, graph, None)
}

/// Resolve `attach` declarations inside a replicated subtree (one array or
/// mirror instance). Parent/child names are looked up within `scope_root`'s
/// descendants only, so sibling instances with identical node names don't
/// collide.
pub fn resolve_attaches_in_scope(
    children: &[Node],
    graph: &mut SceneGraph,
    scope_root: NodeId,
) -> Result<()> {
    let mut specs = Vec::new();
    for c in children {
        walk(c, &mut specs)?;
    }
    apply_specs(&specs, graph, Some(scope_root))
}

fn apply_specs(
    specs: &[AttachSpec],
    graph: &mut SceneGraph,
    scope: Option<NodeId>,
) -> Result<()> {
    if specs.is_empty() {
        return Ok(());
    }

    // Each child can be positioned by at most one attach.
    let mut seen_children: HashMap<&str, usize> = HashMap::new();
    for (i, s) in specs.iter().enumerate() {
        if let Some(prev) = seen_children.insert(s.child.as_str(), i) {
            bail!(
                "attach: child \"{}\" is attached twice (first at spec #{prev}, again at spec #{i})",
                s.child
            );
        }
    }
    let attached_children: HashSet<&str> = seen_children.keys().copied().collect();

    // Iterate until every spec is applied. A spec is applicable once its parent
    // is not itself waiting to be attached.
    let mut applied: HashSet<String> = HashSet::new();
    let mut pending: Vec<&AttachSpec> = specs.iter().collect();
    while !pending.is_empty() {
        let mut progress = false;
        let mut next: Vec<&AttachSpec> = Vec::new();
        for s in pending.drain(..) {
            let parent_waiting =
                attached_children.contains(s.parent.as_str()) && !applied.contains(&s.parent);
            if parent_waiting {
                next.push(s);
                continue;
            }
            apply_attach(s, graph, scope)?;
            applied.insert(s.child.clone());
            progress = true;
        }
        pending = next;
        if !progress && !pending.is_empty() {
            let stuck: Vec<&str> = pending.iter().map(|s| s.child.as_str()).collect();
            bail!("attach: cycle detected among {stuck:?}");
        }
    }

    Ok(())
}

fn collect_attaches(ast: &[Node]) -> Result<Vec<AttachSpec>> {
    let mut out = Vec::new();
    for n in ast {
        walk(n, &mut out)?;
    }
    Ok(out)
}

fn walk(n: &Node, out: &mut Vec<AttachSpec>) -> Result<()> {
    if n.kind == "attach" {
        out.push(build_spec(n)?);
        return Ok(());
    }
    // Stop at replicator boundaries: their attach children are resolved
    // per-instance by resolve_attaches_in_scope, not globally.
    if n.kind == "array" || n.kind == "mirror" {
        return Ok(());
    }
    for c in &n.children {
        walk(c, out)?;
    }
    Ok(())
}

fn build_spec(n: &Node) -> Result<AttachSpec> {
    let parent = str_attr(n, "parent")
        .ok_or_else(|| anyhow!("attach requires parent=\"<node name>\""))?;
    let child = str_attr(n, "child")
        .ok_or_else(|| anyhow!("attach requires child=\"<node name>\""))?;
    let socket = str_attr(n, "socket").unwrap_or_else(|| "top".to_string());
    let plug = str_attr(n, "plug").unwrap_or_else(|| "bottom".to_string());
    let offset = n.attr_number("offset").unwrap_or(0.0);
    let twist_deg = n.attr_number("twist").unwrap_or(0.0);
    Ok(AttachSpec { parent, child, socket, plug, offset, twist_deg, span: n.span })
}

fn str_attr(n: &Node, key: &str) -> Option<String> {
    match n.attr(key)? {
        Value::String(s) | Value::Ident(s) => Some(s.clone()),
        _ => None,
    }
}

fn apply_attach(
    spec: &AttachSpec,
    graph: &mut SceneGraph,
    scope: Option<NodeId>,
) -> Result<()> {
    let find = |name: &str| match scope {
        Some(root) => graph.find_node_in_subtree(root, name),
        None => graph.find_node(name),
    };
    let parent_id = find(&spec.parent)
        .ok_or_else(|| anyhow!("attach: unknown parent node \"{}\"", spec.parent))?;
    let child_id = find(&spec.child)
        .ok_or_else(|| anyhow!("attach: unknown child node \"{}\"", spec.child))?;
    if parent_id == child_id {
        bail!("attach: \"{}\" cannot be attached to itself", spec.child);
    }
    if graph.is_ancestor(child_id, parent_id) {
        bail!(
            "attach: cycle — \"{}\" is already an ancestor of \"{}\"",
            spec.child,
            spec.parent
        );
    }

    let (socket_pos, socket_dir) = {
        let p = &graph.nodes[parent_id.0 as usize];
        let c = p.connectors.iter().find(|c| c.name == spec.socket).ok_or_else(|| {
            anyhow!(
                "attach: parent \"{}\" has no connector \"{}\" (available: {})",
                spec.parent,
                spec.socket,
                list_connector_names(&p.connectors)
            )
        })?;
        (c.pos, normalize_or(c.rotation * Vec3::Y, Vec3::Y))
    };

    let (plug_pos, plug_dir) = {
        let c = &graph.nodes[child_id.0 as usize];
        let conn = c.connectors.iter().find(|c| c.name == spec.plug).ok_or_else(|| {
            anyhow!(
                "attach: child \"{}\" has no connector \"{}\" (available: {})",
                spec.child,
                spec.plug,
                list_connector_names(&c.connectors)
            )
        })?;
        (conn.pos, normalize_or(conn.rotation * Vec3::Y, Vec3::Y))
    };

    let child_scale = graph.nodes[child_id.0 as usize].transform.scale;

    // Rotation: take plug_dir to -socket_dir, then apply twist around that axis.
    let target = normalize_or(-socket_dir, -Vec3::Y);
    let base = Quat::from_rotation_arc(normalize_or(plug_dir, Vec3::Y), target);
    let twist = Quat::from_axis_angle(target, spec.twist_deg.to_radians());
    let rotation = twist * base;

    // Target point in parent's local space.
    let target_local = socket_pos + socket_dir * spec.offset;

    // plug in child's new local space = rotation * (scale * plug_pos) + translation
    let scaled = Vec3::new(
        plug_pos.x * child_scale.x,
        plug_pos.y * child_scale.y,
        plug_pos.z * child_scale.z,
    );
    let translation = target_local - rotation * scaled;

    graph.nodes[child_id.0 as usize].transform =
        Transform::from_trs(translation, rotation, child_scale);

    reparent(graph, child_id, parent_id);
    Ok(())
}

fn list_connector_names(cs: &[mogen_core::Connector]) -> String {
    if cs.is_empty() {
        return "<none>".to_string();
    }
    cs.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
}

fn normalize_or(v: Vec3, fallback: Vec3) -> Vec3 {
    let n = v.normalize_or_zero();
    if n == Vec3::ZERO {
        fallback
    } else {
        n
    }
}

fn reparent(graph: &mut SceneGraph, child: NodeId, new_parent: NodeId) {
    let old_parent = graph.nodes[child.0 as usize].parent;
    match old_parent {
        Some(p) if p == new_parent => return,
        Some(p) => {
            graph.nodes[p.0 as usize].children.retain(|c| *c != child);
        }
        None => {
            graph.roots.retain(|c| *c != child);
        }
    }
    graph.nodes[child.0 as usize].parent = Some(new_parent);
    graph.nodes[new_parent.0 as usize].children.push(child);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;
    use crate::lower;

    fn build(src: &str) -> SceneGraph {
        let ast = parse(src).unwrap();
        lower(&ast).unwrap()
    }

    #[test]
    fn head_attaches_to_top_of_body() {
        let src = r#"
            scene {
              box "body" (size=[1, 2, 1])
              sphere "head" (radius=0.3)
            }
            attach (parent="body", child="head", socket="top", plug="bottom")
        "#;
        let g = build(src);
        let head = g.find_node("head").unwrap();
        let worlds = g.world_transforms();
        let head_center = worlds[head.0 as usize].transform_point3(Vec3::ZERO);
        // body.top = y=1; head.bottom = y=-0.3; after attach, head center at y=1.3.
        assert!((head_center.y - 1.3).abs() < 1e-4, "y = {}", head_center.y);
        // And head is reparented under body.
        assert_eq!(g.nodes[head.0 as usize].parent, Some(g.find_node("body").unwrap()));
    }

    #[test]
    fn transitive_attach_chain() {
        let src = r#"
            scene {
              box "a" (size=[1, 1, 1])
              box "b" (size=[1, 1, 1])
              box "c" (size=[1, 1, 1])
            }
            attach (parent="a", child="b", socket="top", plug="bottom")
            attach (parent="b", child="c", socket="top", plug="bottom")
        "#;
        let g = build(src);
        let c = g.find_node("c").unwrap();
        let world = g.world_transforms()[c.0 as usize];
        let center = world.transform_point3(Vec3::ZERO);
        // a: [0,0,0]; b: y=1 (on top of a); c: y=2 (on top of b).
        assert!((center.y - 2.0).abs() < 1e-4, "y = {}", center.y);
    }

    #[test]
    fn cycle_is_detected() {
        let src = r#"
            scene {
              box "a" (size=[1,1,1])
              box "b" (size=[1,1,1])
            }
            attach (parent="a", child="b")
            attach (parent="b", child="a")
        "#;
        let ast = parse(src).unwrap();
        let err = lower(&ast).unwrap_err();
        assert!(format!("{err}").contains("cycle"));
    }

    #[test]
    fn missing_connector_reports_available_names() {
        let src = r#"
            scene {
              sphere "head" (radius=0.3)
              box "body" (size=[1,1,1])
            }
            attach (parent="body", child="head", socket="neck", plug="bottom")
        "#;
        let ast = parse(src).unwrap();
        let err = lower(&ast).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no connector \"neck\""));
        assert!(msg.contains("top")); // lists available
    }

    #[test]
    fn user_connector_overrides_default() {
        let src = r#"
            scene {
              box "body" (size=[1,1,1]) {
                connector "top" (at=[0, 5, 0], dir=[0, 1, 0])
              }
              sphere "head" (radius=0.3)
            }
            attach (parent="body", child="head", socket="top", plug="bottom")
        "#;
        let g = build(src);
        let head = g.find_node("head").unwrap();
        let worlds = g.world_transforms();
        let y = worlds[head.0 as usize].transform_point3(Vec3::ZERO).y;
        // Overridden socket is at y=5, plug at y=-0.3 → head center at 5.3.
        assert!((y - 5.3).abs() < 1e-4, "y = {}", y);
    }

    #[test]
    fn array_of_module_with_attach_wires_every_instance() {
        // Regression for a bug where attach specs inside a module used by an
        // `array` only resolved for the first instance: name lookup hit the
        // first matching node in the whole graph, so arms_1..N were left with
        // their sub-parts flat-parented under the instance root.
        let src = r#"
            module "arm" () {
              group "root" {
                box "branch" (size=[0.4, 0.02, 0.02])
                cylinder "candle" (radius=0.05, height=0.3)
                attach (parent="branch", child="candle", socket="right", plug="bottom")
              }
            }

            scene {
              sphere "hub" (radius=0.1)
              array "arms" (count=4, around=y) {
                use "arm" ()
              }
            }
        "#;
        let g = build(src);
        // Every arms_i should have a root whose branch parents its candle.
        for i in 0..4 {
            let instance = g
                .find_node(&format!("arms_{i}"))
                .unwrap_or_else(|| panic!("missing arms_{i}"));
            let root = g.find_node_in_subtree(instance, "root").unwrap();
            let branch = g.find_node_in_subtree(root, "branch").unwrap();
            let candle = g.find_node_in_subtree(root, "candle").unwrap();
            assert_eq!(
                g.nodes[candle.0 as usize].parent,
                Some(branch),
                "arms_{i} candle should be parented under its own branch"
            );
        }
    }

    #[test]
    fn twist_rotates_around_socket_dir() {
        let src = r#"
            scene {
              box "body" (size=[1,1,1])
              box "hat" (size=[2, 0.2, 0.5])
            }
            attach (parent="body", child="hat", socket="top", plug="bottom", twist=90)
        "#;
        let g = build(src);
        let hat = g.find_node("hat").unwrap();
        let worlds = g.world_transforms();
        // After 90° twist around +Y, hat's local +X axis maps to -Z in parent space.
        let x_axis = worlds[hat.0 as usize].transform_vector3(Vec3::X);
        assert!(x_axis.z.abs() > 0.99, "x_axis = {x_axis:?}");
    }
}
