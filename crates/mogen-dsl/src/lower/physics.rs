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

/// Resolve a geometry node's `phys="<name>"` reference into a [`PhysicsBody`]
/// snapshot on the node. Copies the substance properties off the referenced
/// declaration and records any explicit per-node `weight=<mass>` override (a
/// flat mass in kg — distinct from the block's per-volume `weight=`). Leaves
/// `mass`/`center_of_gravity` for the auto-weigh pass unless overridden.
pub(super) fn bind_physics(node: &Node, id: NodeId, graph: &mut SceneGraph) -> Result<()> {
    let Some(name) = node.attr_string("phys") else {
        return Ok(());
    };
    let pid = graph
        .find_physics_scoped(name, node.origin.as_deref())
        .ok_or_else(|| anyhow!("unknown physics material: {name}"))?;
    let pm = &graph.physics[pid.0 as usize];
    let mut body = PhysicsBody {
        material: pm.name.clone(),
        weight_per_m3: pm.weight_per_m3,
        friction: pm.friction,
        bounce: pm.bounce,
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
