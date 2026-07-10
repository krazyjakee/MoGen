//! Lowering for physics: the `physics "<name>" (…)` declaration and the
//! per-node `phys="<name>"` binding.
//!
//! Mirrors [`super::material`]. [`collect_physics`] hoists every declaration
//! into `SceneGraph::physics` (Pass 1, alongside materials). [`bind_physics`]
//! resolves a geometry node's `phys=` reference into a [`PhysicsBody`] snapshot
//! and stamps it on the node. The heavy values — `mass` and `center_of_gravity`
//! — are filled later, once the final mesh exists, by the auto-weigh pass in
//! [`super::super::lower`] (they need the post-attach/conform geometry).

use anyhow::{anyhow, Result};

use mogen_core::{NodeId, PhysicsBody, PhysicsMaterial, SceneGraph};

use crate::ast::Node;

/// Hoist every `physics` declaration (top-level or nested inside a wrapping
/// group / module body) into `graph.physics`. Deduped by `(name, origin)` so
/// repeated subtree walks are idempotent — identical to how materials collect.
pub(super) fn collect_physics(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    for n in ast {
        collect_physics_recursive(n, graph)?;
    }
    Ok(())
}

fn collect_physics_recursive(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    if node.kind == "physics" {
        register_physics(node, graph)?;
    }
    for c in &node.children {
        collect_physics_recursive(c, graph)?;
    }
    Ok(())
}

fn register_physics(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    let name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("physics requires a name, e.g. `physics \"oak\" (...)`"))?;
    if graph
        .physics
        .iter()
        .any(|p| p.name == name && p.origin == node.origin)
    {
        return Ok(());
    }
    let mut phys = PhysicsMaterial::new(&name);
    // In a `physics` block, `weight=` is a weight *per cubic metre* (the
    // `700kg/m3` density form); the unit suffix already normalised it to kg/m³.
    if let Some(w) = node.attr_number("weight") {
        phys.weight_per_m3 = w;
    }
    if let Some(f) = node.attr_number("friction") {
        phys.friction = f;
    }
    if let Some(b) = node.attr_number("bounce") {
        phys.bounce = b;
    }
    phys.origin = node.origin.clone();
    graph.add_physics(phys);
    Ok(())
}

/// A resolved substance: the four intrinsic fields copied onto a [`PhysicsBody`]
/// (everything except the geometry-derived `mass`/`center_of_gravity`).
type Substance = (String, f32, f32, f32);

/// Resolve a geometry node's physics substance into a [`PhysicsBody`] snapshot.
///
/// An explicit `phys="<name>"` wins; otherwise the node **inherits** the nearest
/// ancestor's substance, exactly as `mat=` inherits down the hierarchy (see
/// [`super::helpers::inherit_material_from_ancestor`]). So `phys=` on a `group`
/// flows to every child mesh, which then weighs itself. A flat per-node
/// `weight=<mass>` override is recorded regardless; `mass`/`center_of_gravity`
/// are otherwise filled by the auto-weigh pass.
pub(super) fn bind_physics(node: &Node, id: NodeId, graph: &mut SceneGraph) -> Result<()> {
    let substance = if let Some(name) = node.attr_string("phys") {
        let pid = graph
            .find_physics_scoped(name, node.origin.as_deref())
            .ok_or_else(|| anyhow!("unknown physics material: {name}"))?;
        let pm = &graph.physics[pid.0 as usize];
        (pm.name.clone(), pm.weight_per_m3, pm.friction, pm.bounce)
    } else {
        // No own `phys=` — inherit from the nearest ancestor that has a body.
        // The ancestor's own mass/COG never carry down; each node weighs itself.
        match inherited_substance(id, graph) {
            Some(s) => s,
            None => return Ok(()),
        }
    };
    let (material, weight_per_m3, friction, bounce) = substance;
    let mut body = PhysicsBody {
        material,
        weight_per_m3,
        friction,
        bounce,
        mass: None,
        center_of_gravity: None,
    };
    // A per-node `weight=` is a flat mass override (kg): the object weighs
    // exactly this regardless of its volume (a hollow prop, a magic anvil).
    if let Some(w) = node.attr_number("weight") {
        body.mass = Some(w);
    }
    graph.nodes[id.0 as usize].physics = Some(body);
    Ok(())
}

/// Walk the parent chain and return the nearest ancestor's substance, if any.
/// Only the intrinsic fields are copied — never the ancestor's computed
/// `mass`/`center_of_gravity`.
fn inherited_substance(id: NodeId, graph: &SceneGraph) -> Option<Substance> {
    let mut cur = graph.nodes[id.0 as usize].parent;
    while let Some(p) = cur {
        if let Some(b) = &graph.nodes[p.0 as usize].physics {
            return Some((b.material.clone(), b.weight_per_m3, b.friction, b.bounce));
        }
        cur = graph.nodes[p.0 as usize].parent;
    }
    None
}
