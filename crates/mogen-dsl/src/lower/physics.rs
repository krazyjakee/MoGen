//! Lowering for physics: the `physics "<name>" (…)` declaration and the
//! per-node `phys="<name>"` binding.
//!
//! Mirrors [`super::material`]. [`collect_physics`] hoists every declaration
//! into `SceneGraph::physics` (Pass 1, alongside materials). [`bind_physics`]
//! resolves a geometry node's `phys=` reference into a [`PhysicsBody`] snapshot
//! and stamps it on the node. Mass and centre of gravity are filled later
//! by [`weigh_bodies`], after attach, conform, and skin binding
//! settle the geometry. Explicit weights are preserved on meshes and groups.

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
/// `weight=<mass>` override is recorded when a substance resolves; without an
/// explicit or inherited substance, no body is created. Other mass properties are
/// filled by [`weigh_bodies`].
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

/// Compute masses and local centres of gravity after geometry has settled.
/// Explicit weights, including zero, are retained. Meshless compounds derive
/// their centroid from own-mesh descendants; nested group weights do not
/// contribute to enclosing compounds. Skip this pass when there are no bodies.
pub(super) fn weigh_bodies(graph: &mut SceneGraph) {
    if !graph.nodes.iter().any(|node| node.physics.is_some()) {
        return;
    }
    let worlds = graph.world_transforms();
    // (a) Leaves — nodes with their own mesh. Weight = substance density × world
    // volume (the world-transform determinant folds in this node's + ancestors'
    // scale, so `scale=2` weighs 8×); centre of gravity = the mesh's volume
    // centroid, in local space. An explicit `weight=` keeps its overridden mass
    // but still gets a real centre of gravity.
    for id in 0..graph.nodes.len() {
        let node = &graph.nodes[id];
        let Some(body) = &node.physics else { continue };
        let Some(mesh) = &node.mesh else { continue };
        let mass = if body.mass.is_some() {
            body.mass
        } else {
            // Group as `density × (volume × det)` — the same association the
            // golden GLBs were baked with; regrouping shifts the last f32 ULP.
            let world_volume = mesh.solid_volume() * worlds[id].determinant().abs();
            Some(body.weight_per_m3 * world_volume)
        };
        let cog = mesh.solid_centroid();
        let b = graph.nodes[id].physics.as_mut().unwrap();
        b.mass = mass;
        b.center_of_gravity = cog;
    }
    // (b) Compound bodies — a node that carries a physics body but has no mesh
    // of its own (a `group phys=…`, possibly inherited) reports the *combined*
    // mass and mass-weighted centre of gravity of every mesh-bearing descendant,
    // expressed in its own local frame. An engine can then treat the whole
    // assembly as one rigid body. Only own-mesh descendants contribute, so a
    // compound group nested above another never double-counts the shared
    // leaves. Runs after (a) so leaf masses already exist.
    for id in 0..graph.nodes.len() {
        if graph.nodes[id].physics.is_none() || graph.nodes[id].mesh.is_some() {
            continue;
        }
        let mut total = 0.0f32;
        let mut weighted = glam::Vec3::ZERO;
        collect_subtree_mass(graph, NodeId(id as u32), &worlds, &mut total, &mut weighted);
        if total > 0.0 {
            let local_com = worlds[id].inverse().transform_point3(weighted / total);
            let b = graph.nodes[id].physics.as_mut().unwrap();
            b.mass.get_or_insert(total);
            b.center_of_gravity = Some([local_com.x, local_com.y, local_com.z]);
        }
    }
}

/// Accumulate the mass and mass-weighted *world-space* centre of gravity of
/// every mesh-bearing physics body strictly below `id`. Only own-mesh nodes
/// contribute, so a compound group above another compound group can't
/// double-count the leaves they share.
fn collect_subtree_mass(
    graph: &SceneGraph,
    id: NodeId,
    worlds: &[glam::Mat4],
    total: &mut f32,
    weighted: &mut glam::Vec3,
) {
    for &c in &graph.nodes[id.0 as usize].children {
        let node = &graph.nodes[c.0 as usize];
        if node.mesh.is_some() {
            if let Some(m) = node.physics.as_ref().and_then(|b| b.mass) {
                let cog = node
                    .physics
                    .as_ref()
                    .and_then(|b| b.center_of_gravity)
                    .unwrap_or([0.0, 0.0, 0.0]);
                let world_pt = worlds[c.0 as usize].transform_point3(glam::Vec3::from_array(cog));
                *total += m;
                *weighted += m * world_pt;
            }
        }
        collect_subtree_mass(graph, c, worlds, total, weighted);
    }
}
