//! Lower `skeleton { bone ... }` blocks into scene nodes + a `Skin` record,
//! and bind procedural meshes with `skin="<name>"` to those skeletons.
//!
//! Bones are ordinary scene nodes (kind="bone") parented under the skeleton
//! group (kind="skeleton"); the `Skin::joints` list captures their ids in
//! depth-first order so the same ordering is used for JOINTS_0 indices and the
//! inverse-bind matrix accessor.

use anyhow::{anyhow, bail, Result};
use glam::{Mat4, Vec3};

use mogen_core::{NodeId, SceneGraph, Skin, Transform};

use crate::ast::Node;

const DEFAULT_ENVELOPE: f32 = 0.75;
const MAX_INFLUENCES: usize = 4;

/// Lower a top-level or scene-level `skeleton` block. Returns the parent scene
/// node created for the skeleton group so later passes can attach children to
/// it if needed; the `Skin` itself is stored in `graph.skins`.
pub fn lower_skeleton(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("skeleton requires a name"))?;

    if graph.find_skin(&name).is_some() {
        bail!("duplicate skeleton \"{name}\"");
    }

    let transform = transform_from_attrs(node);
    let skel_id = match parent {
        None => graph.add_root(&name, "skeleton", transform),
        Some(p) => graph.add_child(p, &name, "skeleton", transform),
    };
    graph.set_source_span(skel_id, node.span);
    graph.nodes[skel_id.0 as usize].origin = node.origin.clone();

    let mut joints: Vec<NodeId> = Vec::new();
    let mut envelopes: Vec<f32> = Vec::new();
    for c in &node.children {
        if c.kind != "bone" {
            bail!(
                "skeleton \"{name}\" children must be `bone` nodes (got `{}`)",
                c.kind
            );
        }
        lower_bone(c, skel_id, graph, &mut joints, &mut envelopes)?;
    }

    if joints.is_empty() {
        bail!("skeleton \"{name}\" has no bones");
    }

    // Compute bind-pose inverse matrices from current world transforms. This
    // captures every bone's rest pose; any clip that later rotates a bone
    // will animate its current world transform relative to this matrix.
    let worlds = graph.world_transforms();
    let ibms: Vec<[[f32; 4]; 4]> = joints
        .iter()
        .map(|j| worlds[j.0 as usize].inverse().to_cols_array_2d())
        .collect();

    let skeleton_root = joints.first().copied();
    graph.add_skin(Skin {
        name,
        joints,
        inverse_bind_matrices: ibms,
        envelopes,
        skeleton_root,
        origin: node.origin.clone(),
    });

    Ok(skel_id)
}

fn lower_bone(
    node: &Node,
    parent: NodeId,
    graph: &mut SceneGraph,
    joints: &mut Vec<NodeId>,
    envelopes: &mut Vec<f32>,
) -> Result<()> {
    let name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("bone requires a name"))?;
    let transform = transform_from_attrs(node);
    let id = graph.add_child(parent, &name, "bone", transform);
    graph.set_source_span(id, node.span);
    graph.nodes[id.0 as usize].origin = node.origin.clone();
    joints.push(id);
    envelopes.push(node.attr_number("envelope").unwrap_or(DEFAULT_ENVELOPE));
    for c in &node.children {
        if c.kind != "bone" {
            bail!(
                "bone \"{name}\" children must be `bone` nodes (got `{}`)",
                c.kind
            );
        }
        lower_bone(c, id, graph, joints, envelopes)?;
    }
    Ok(())
}

/// Bind every scene node whose `skin` attribute names an existing skin. For
/// each such node, compute per-vertex weights by nearest-bone + envelope
/// falloff and populate `Mesh::joints` / `Mesh::weights`. If the node also
/// carries `bind="<bone>"`, every vertex is rigidly weighted 1.0 to that
/// single joint instead — used for accessories that should follow one bone
/// without deforming (hats, backpacks, hand-held props).
pub fn bind_meshes(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    let bindings = collect_skin_bindings(ast);
    if bindings.is_empty() {
        return Ok(());
    }

    let worlds = graph.world_transforms();

    for (node_name, skin_name, bind_to) in bindings {
        let skin_id = graph
            .find_skin(&skin_name)
            .ok_or_else(|| anyhow!("mesh \"{node_name}\" refers to unknown skin \"{skin_name}\""))?;
        let node_id = graph
            .find_node(&node_name)
            .ok_or_else(|| anyhow!("skin binding: unknown scene node \"{node_name}\""))?;

        let mesh_world = worlds[node_id.0 as usize];
        let (joint_worlds, envelopes, bind_index) = {
            let skin = &graph.skins[skin_id.0 as usize];
            let joint_worlds: Vec<Mat4> = skin
                .joints
                .iter()
                .map(|j| worlds[j.0 as usize])
                .collect();
            let envelopes: Vec<f32> = if skin.envelopes.len() == skin.joints.len() {
                skin.envelopes.clone()
            } else {
                vec![DEFAULT_ENVELOPE; skin.joints.len()]
            };
            let bind_index = match &bind_to {
                Some(bone) => {
                    let idx = skin
                        .joints
                        .iter()
                        .position(|j| graph.nodes[j.0 as usize].name == *bone)
                        .ok_or_else(|| {
                            anyhow!(
                                "mesh \"{node_name}\" `bind=\"{bone}\"` does not name a bone in skin \"{skin_name}\""
                            )
                        })?;
                    Some(idx as u16)
                }
                None => None,
            };
            (joint_worlds, envelopes, bind_index)
        };

        let (joints, weights, baked_positions, baked_normals) = {
            let mesh = graph
                .nodes
                .get(node_id.0 as usize)
                .and_then(|n| n.mesh.as_ref())
                .ok_or_else(|| anyhow!("skin binding target \"{node_name}\" has no mesh"))?;
            // glTF 2.0: a node with `skin` has its local TRS ignored at render.
            // Bake the node's world transform into the POSITION/NORMAL buffers
            // so the skinned mesh lives in the same frame as the joints.
            let baked_positions: Vec<[f32; 3]> = mesh
                .positions
                .iter()
                .map(|p| {
                    let w = mesh_world.transform_point3(Vec3::from_array(*p));
                    [w.x, w.y, w.z]
                })
                .collect();
            let normal_mat = mesh_world.inverse().transpose();
            let baked_normals: Vec<[f32; 3]> = mesh
                .normals
                .iter()
                .map(|n| {
                    let v = Vec3::from_array(*n);
                    let rotated =
                        normal_mat.transform_vector3(v).normalize_or_zero();
                    [rotated.x, rotated.y, rotated.z]
                })
                .collect();
            let (j, w) = match bind_index {
                Some(idx) => rigid_skin_weights(baked_positions.len(), idx),
                None => compute_skin_weights(
                    &baked_positions,
                    Mat4::IDENTITY,
                    &joint_worlds,
                    &envelopes,
                ),
            };
            (j, w, baked_positions, baked_normals)
        };

        let node_mut = &mut graph.nodes[node_id.0 as usize];
        if let Some(m) = node_mut.mesh.as_mut() {
            m.positions = baked_positions;
            m.normals = baked_normals;
            m.joints = joints;
            m.weights = weights;
        }
        node_mut.skin = Some(skin_id);
        node_mut.transform = Transform::IDENTITY;
    }

    Ok(())
}

fn transform_from_attrs(node: &Node) -> Transform {
    let t = node.attr_vec3("pos").unwrap_or(Vec3::ZERO);
    let r = node.attr_rotation("rot").unwrap_or(glam::Quat::IDENTITY);
    let s = node.attr_scale("scale").unwrap_or(Vec3::ONE);
    Transform::from_trs(t, r, s)
}

/// Walk the post-expansion AST collecting `(node_name, skin_name, bind_to)`
/// triples from every node that carries a `skin="..."` attribute. `bind_to`
/// is the optional `bind="<bone>"` override that pins every vertex to a
/// single joint with weight 1.0. Only nodes with an explicit `name` can be
/// bound, since we resolve the scene target by name.
fn collect_skin_bindings(ast: &[Node]) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    for n in ast {
        walk_bindings(n, None, None, &mut out);
    }
    out
}

/// Whether `kind` produces an actual mesh node that can carry skin weights.
/// Group-like containers (`scene`, `group`, `solid`, `stack`, `grid`) only
/// pass `skin=` down to their descendants; they don't bind meshes themselves.
fn is_mesh_kind(kind: &str) -> bool {
    !matches!(
        kind,
        "scene"
            | "group"
            | "solid"
            | "stack"
            | "grid"
            | "array"
            | "mirror"
            | "module"
            | "use"
            | "skeleton"
            | "bone"
            | "material"
            | "attach"
            | "connector"
            | "joint"
            | "clip"
            | "track"
            | "spin"
            | "open_close"
            | "wave"
            | "flap"
            | "idle"
    )
}

fn walk_bindings(
    node: &Node,
    inherited_skin: Option<&str>,
    inherited_bind: Option<&str>,
    out: &mut Vec<(String, String, Option<String>)>,
) {
    // Bones and skeletons can't be skin targets; don't descend into them.
    if node.kind == "skeleton" || node.kind == "bone" {
        return;
    }
    let own_skin = string_attr(node, "skin");
    let own_bind = string_attr(node, "bind");
    let effective_skin: Option<String> = own_skin
        .as_deref()
        .or(inherited_skin)
        .map(|s| s.to_string());
    // `bind` propagates from a group to its mesh descendants the same way
    // `skin` does, so wrapping a sub-tree in `group (skin="rig", bind="neck")`
    // pins every mesh inside it rigidly to that bone — used by the head /
    // face cluster, which should track the neck without envelope blending
    // pulling cheek vertices into the shoulder bones.
    let effective_bind: Option<String> = own_bind
        .as_deref()
        .or(inherited_bind)
        .map(|s| s.to_string());

    // A node is a binding target only if it actually has a mesh. Groups that
    // carry `skin=` propagate the binding to their mesh descendants instead.
    if let (Some(skin_ref), Some(name)) = (&effective_skin, &node.name) {
        if is_mesh_kind(&node.kind) {
            out.push((name.clone(), skin_ref.clone(), effective_bind.clone()));
        }
    }
    // CSG operands (`union`/`difference`/`intersect` children) are fused into
    // the parent node's mesh at lowering — they don't survive as separate
    // scene nodes, so a propagated `skin=` would dangle. Stop the walk here.
    if matches!(node.kind.as_str(), "union" | "difference" | "intersect") {
        return;
    }
    for c in &node.children {
        walk_bindings(
            c,
            effective_skin.as_deref(),
            effective_bind.as_deref(),
            out,
        );
    }
}

fn string_attr(node: &Node, key: &str) -> Option<String> {
    match node.attr(key)? {
        crate::ast::Value::String(s) | crate::ast::Value::Ident(s) => Some(s.clone()),
        _ => None,
    }
}

/// Pin every vertex rigidly to a single joint — used by `bind="<bone>"`.
/// Skips envelope falloff so accessories follow exactly one bone with no
/// stretching across neighbours.
fn rigid_skin_weights(count: usize, joint: u16) -> (Vec<[u16; 4]>, Vec<[f32; 4]>) {
    (
        vec![[joint, 0, 0, 0]; count],
        vec![[1.0, 0.0, 0.0, 0.0]; count],
    )
}

/// Assign each vertex up to `MAX_INFLUENCES` bones using a linear envelope
/// falloff: `w = max(0, 1 - d/envelope)`. Weights are clamped to the top
/// influences, normalized, and padded with zero slots when under-influenced.
/// If every bone is outside its envelope, the closest bone receives weight 1.0
/// so no vertex is ever left unbound.
fn compute_skin_weights(
    positions: &[[f32; 3]],
    mesh_world: Mat4,
    joint_worlds: &[Mat4],
    envelopes: &[f32],
) -> (Vec<[u16; 4]>, Vec<[f32; 4]>) {
    let mut out_joints = Vec::with_capacity(positions.len());
    let mut out_weights = Vec::with_capacity(positions.len());
    for p in positions {
        let v_world = mesh_world.transform_point3(Vec3::from_array(*p));
        // (joint_index, weight, distance)
        let mut scored: Vec<(u16, f32, f32)> = Vec::with_capacity(joint_worlds.len());
        for (i, (jw, env)) in joint_worlds.iter().zip(envelopes.iter()).enumerate() {
            let bone_pos = jw.transform_point3(Vec3::ZERO);
            let d = (v_world - bone_pos).length();
            let env = env.max(1e-4);
            let w = (1.0 - d / env).max(0.0);
            scored.push((i as u16, w, d));
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let sum_top: f32 = scored.iter().take(MAX_INFLUENCES).map(|s| s.1).sum();

        let (mut js, mut ws) = ([0u16; 4], [0.0f32; 4]);
        if sum_top <= 1e-6 {
            // Fallback: stick everything to the nearest bone.
            let nearest = scored
                .iter()
                .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
                .copied()
                .unwrap_or((0, 0.0, 0.0));
            js[0] = nearest.0;
            ws[0] = 1.0;
        } else {
            for (slot, (ji, wi, _)) in scored.iter().take(MAX_INFLUENCES).enumerate() {
                js[slot] = *ji;
                ws[slot] = wi / sum_top;
            }
        }
        out_joints.push(js);
        out_weights.push(ws);
    }
    (out_joints, out_weights)
}
