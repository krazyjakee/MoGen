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
//! `pos` / `rot` / `scale` set on the attached child are preserved as a local
//! offset composed on top of the alignment: `pos` shifts the anchor in the
//! parent's frame, `rot` rotates the (already-aligned) node around its anchor,
//! `scale` is the child's own scale (used when computing where the plug lands).
//! With all three at their defaults the child sits exactly where the alignment
//! puts it; non-default values let a Studio gizmo drag persist instead of
//! being silently discarded on the next build.
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

use mogen_core::{AttachBinding, NodeId, SceneGraph, Span, Transform};

use crate::ast::Node;

#[derive(Debug)]
struct AttachSpec {
    parent: String,
    child: String,
    socket: String,
    plug: String,
    offset: f32,
    twist_deg: f32,
    /// Module-use expansion frame this attach was authored in, or `None` for
    /// attaches written directly in the user's scene. Drives scoped name
    /// resolution: an attach with `use_id=Some(k)` only sees graph nodes
    /// stamped with the same id.
    use_id: Option<u32>,
    #[allow(dead_code)]
    span: Span,
}

/// Resolve every `attach` declared at AST scope (scene root, group bodies).
///
/// Each `attach` is stamped during module expansion with the `use_id` of the
/// expansion frame it lives in. We group specs by that id and resolve each
/// group against the matching scene nodes — so two imported objects that
/// both contain a node literally named `"base"` don't collide: each is
/// stamped with a different `use_id`, and the attach only sees nodes that
/// share its stamp.
///
/// Top-level attaches (authored directly in the user's scene, no `use_id`)
/// fall back to the global namespace, preserving the historical behavior.
///
/// Attaches inside an `array`/`mirror`/`grid` subtree are NOT visited here —
/// the replicator handles them per-instance via [`resolve_attaches_in_scope`]
/// so each replicated copy resolves parent/child against its own subtree.
pub fn resolve_attaches(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    let specs = collect_attaches(ast)?;

    // Partition by use_id so cycle detection / child-attached-twice checks run
    // per expansion frame. The same node name in two different frames refers
    // to two different graph nodes and must not collide in the dedup map.
    let mut by_use: HashMap<Option<u32>, Vec<AttachSpec>> = HashMap::new();
    for s in specs {
        by_use.entry(s.use_id).or_default().push(s);
    }
    for (_, group) in by_use {
        apply_specs(&group, graph, None)?;
    }
    Ok(())
}

/// Resolve `attach` declarations inside a replicated subtree (one array or
/// mirror instance). Parent/child names are looked up within `scope_root`'s
/// descendants only, so sibling instances with identical node names don't
/// collide.
///
/// Specs are also partitioned by `use_id` (mirroring [`resolve_attaches`]) so
/// two imported objects whose internals happen to share a child name — e.g.
/// `mouse.mog` and `keyboard.mog` both calling their cord `"cable"` — don't
/// trip the duplicate-child check or steal each other's connectors when both
/// are pulled into the same replicated body.
pub fn resolve_attaches_in_scope(
    children: &[Node],
    graph: &mut SceneGraph,
    scope_root: NodeId,
) -> Result<()> {
    let mut specs = Vec::new();
    for c in children {
        walk(c, &mut specs)?;
    }
    let mut by_use: HashMap<Option<u32>, Vec<AttachSpec>> = HashMap::new();
    for s in specs {
        by_use.entry(s.use_id).or_default().push(s);
    }
    for (_, group) in by_use {
        apply_specs(&group, graph, Some(scope_root))?;
    }
    Ok(())
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
    if n.kind == "array" || n.kind == "mirror" || n.kind == "grid" {
        return Ok(());
    }
    for c in &n.children {
        walk(c, out)?;
    }
    Ok(())
}

fn build_spec(n: &Node) -> Result<AttachSpec> {
    let parent = n
        .attr_string("parent")
        .map(String::from)
        .ok_or_else(|| anyhow!("attach requires parent=\"<node name>\""))?;
    let child = n
        .attr_string("child")
        .map(String::from)
        .ok_or_else(|| anyhow!("attach requires child=\"<node name>\""))?;
    let socket = n.attr_string("socket").unwrap_or("top").to_string();
    let plug = n.attr_string("plug").unwrap_or("bottom").to_string();
    let offset = n.attr_number("offset").unwrap_or(0.0);
    let twist_deg = n.attr_number("twist").unwrap_or(0.0);
    Ok(AttachSpec {
        parent,
        child,
        socket,
        plug,
        offset,
        twist_deg,
        use_id: n.use_id,
        span: n.span,
    })
}

fn apply_attach(
    spec: &AttachSpec,
    graph: &mut SceneGraph,
    scope: Option<NodeId>,
) -> Result<()> {
    // Lookup precedence:
    //   1. Explicit `scope` (used by replicator per-instance pass) — limit to
    //      the instance's subtree so sibling replicas don't steal names from
    //      each other. Within that subtree, still filter by frame visibility
    //      so two imported objects with same-named internals (e.g. mouse and
    //      keyboard both having a `cable`) each see only their own frame.
    //   2. Frame-visible — `spec.use_id` itself plus any descendant frame in
    //      `graph.use_parents`. Lets an outer module's attach reach into a
    //      nested `use`'s nodes (e.g. `humanoid_full` attaching to a `torso`
    //      brought in by `use "humanoid_torso"`) while still keeping
    //      sibling-instance frames isolated from each other.
    let find = |name: &str| -> Option<NodeId> {
        if let Some(root) = scope {
            let mut stack = vec![root];
            while let Some(id) = stack.pop() {
                let n = &graph.nodes[id.0 as usize];
                if n.name == name && graph.use_id_visible(spec.use_id, n.use_id) {
                    return Some(id);
                }
                stack.extend(n.children.iter().copied());
            }
            return None;
        }
        graph
            .nodes
            .iter()
            .position(|n| n.name == name && graph.use_id_visible(spec.use_id, n.use_id))
            .map(|i| NodeId(i as u32))
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

    let user_xform = graph.nodes[child_id.0 as usize].transform;
    let child_scale = user_xform.scale;

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

    // Compose the user's pos/rot on top of the attach result, in the new
    // parent's local frame: pos shifts the anchor; rot rotates the
    // (already-aligned) node around its anchor. Lets a gizmo drag on an
    // attached node persist instead of being silently discarded.
    let final_translation = translation + user_xform.translation;
    let final_rotation = user_xform.rotation * rotation;

    graph.nodes[child_id.0 as usize].transform =
        Transform::from_trs(final_translation, final_rotation, child_scale);
    graph.nodes[child_id.0 as usize].attach_binding = Some(AttachBinding {
        parent: parent_id,
        socket: spec.socket.clone(),
        anchor: translation.to_array(),
        rotation: rotation.to_array(),
    });

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

/// Re-export of `reparent` for the conform pass, which mirrors attach's
/// "child becomes a graph child of the target" behaviour.
pub(crate) fn reparent_pub(graph: &mut SceneGraph, child: NodeId, new_parent: NodeId) {
    reparent(graph, child, new_parent)
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
    fn user_pos_offsets_attached_child() {
        let src = r#"
            scene {
              box "body" (size=[1, 2, 1])
              sphere "head" (radius=0.3, pos=[0, 0.05, 0])
            }
            attach (parent="body", child="head", socket="top", plug="bottom")
        "#;
        let g = build(src);
        let head = g.find_node("head").unwrap();
        let worlds = g.world_transforms();
        let y = worlds[head.0 as usize].transform_point3(Vec3::ZERO).y;
        // Without the offset attach lands head center at y=1.3; pos=[0,0.05,0]
        // shifts it 0.05 along the parent's local Y.
        assert!((y - 1.35).abs() < 1e-4, "y = {}", y);
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
    fn outer_module_can_use_same_inner_module_multiple_times() {
        // Regression: an outer module that calls the same inner module N
        // times must keep each instance's internal attach specs in their
        // own frame. Before the fix, every nested `use` inherited the
        // outermost frame, so all five "tip" attaches collapsed into one
        // bucket and the second one tripped E0701 ("attached twice").
        let src = r#"
            module "pen" () {
              group "root" {
                cylinder "body" (radius=0.04, height=1.0)
                cone "tip" (radius=0.04, height=0.15)
                attach (parent="body", child="tip", socket="bottom", plug="bottom")
              }
            }

            module "pot" () {
              group "p1" { use "pen" () }
              group "p2" { use "pen" () }
              group "p3" { use "pen" () }
            }

            scene { use "pot" () }
        "#;
        let g = build(src);
        // Each pen instance must have its own body parenting its own tip.
        // The pen module uses `group "root"` to wrap things; that root sits
        // under p1/p2/p3 inside the synthesised pot module body.
        let mut tips_with_correct_parent = 0;
        for n in &g.nodes {
            if n.name == "tip" {
                let parent = n.parent.expect("tip should be reparented under body");
                let parent_name = &g.nodes[parent.0 as usize].name;
                assert_eq!(parent_name, "body", "tip parent should be 'body'");
                tips_with_correct_parent += 1;
            }
        }
        assert_eq!(tips_with_correct_parent, 3, "expected 3 tips, one per pen");
    }

    #[test]
    fn replicator_isolates_same_named_children_across_frames() {
        // Regression: when two modules brought into a replicator body each
        // declared an attach for a child with the same name, the per-instance
        // resolver lumped them into one bucket and tripped E0701
        // ("attached twice"). Each attach lives in its own `use` frame, so
        // they should be partitioned by `use_id` and resolve independently.
        let src = r#"
            module "mouse" () {
              group "mouse_root" {
                box "body" (size=[0.06, 0.03, 0.10])
                cylinder "cable" (radius=0.003, height=0.2)
                attach (parent="body", child="cable", socket="back", plug="bottom")
              }
            }
            module "keyboard" () {
              group "kbd_root" {
                box "case" (size=[0.4, 0.02, 0.15])
                cylinder "cable" (radius=0.003, height=0.2)
                attach (parent="case", child="cable", socket="back", plug="bottom")
              }
            }
            scene {
              array "desks" (count=2, around=y) {
                use "mouse" ()
                use "keyboard" ()
              }
            }
        "#;
        let g = build(src);
        // Each replica should own one mouse "cable" parented under its own
        // "body" and one keyboard "cable" parented under its own "case".
        let mut under_body = 0;
        let mut under_case = 0;
        for n in &g.nodes {
            if n.name != "cable" { continue; }
            let parent = n.parent.expect("cable should be reparented");
            match g.nodes[parent.0 as usize].name.as_str() {
                "body" => under_body += 1,
                "case" => under_case += 1,
                other => panic!("cable parented under unexpected node: {other}"),
            }
        }
        assert_eq!(under_body, 2, "expected one mouse cable per replica");
        assert_eq!(under_case, 2, "expected one keyboard cable per replica");
    }

    #[test]
    fn outer_module_attach_can_target_inner_module_node() {
        // The inverse case: an attach declared in an outer module references
        // a node brought in by a nested `use`. Before the fix this worked
        // (because nested uses inherited the outer frame); we keep that
        // behaviour with the descendant-frame lookup.
        let src = r#"
            module "head" () {
              sphere "head" (radius=0.3)
            }
            module "humanoid" () {
              box "torso" (size=[1, 2, 1])
              use "head" ()
              attach (parent="torso", child="head", socket="top", plug="bottom")
            }
            scene { use "humanoid" () }
        "#;
        let g = build(src);
        let head = g.find_node("head").unwrap();
        let torso = g.find_node("torso").unwrap();
        assert_eq!(
            g.nodes[head.0 as usize].parent,
            Some(torso),
            "head should be reparented under torso via the outer module's attach"
        );
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
