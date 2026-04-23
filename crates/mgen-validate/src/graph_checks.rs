//! Post-lowering validation of the `SceneGraph`. Focuses on invariants that
//! can only be checked after module expansion + skin binding — per-vertex
//! weights summing to 1.0, joint indices being in range, and skeleton roots
//! being ancestors of every joint. Also runs a geometric connectivity check
//! that catches free-positioned parts which drift apart (floating head,
//! detached leg), catching cases the `attach` pipeline doesn't cover.

use mgen_core::{node_world_aabb, Aabb, Diagnostic, NodeId, SceneGraph};

const WEIGHT_SUM_TOLERANCE: f32 = 1e-3;

/// Bounding boxes are considered "touching" if they overlap or sit within
/// this distance of each other (in scene units — typically metres). This
/// absorbs small rounding errors and lets clearly-adjacent parts register as
/// connected even if the LLM emitted positions that are off by a fraction of
/// a millimetre.
const CONNECTIVITY_SLOP: f32 = 0.002;

pub fn validate_graph(graph: &SceneGraph) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    diags.extend(check_connectivity(graph));

    for skin in &graph.skins {
        if let Some(root) = skin.skeleton_root {
            for j in &skin.joints {
                if !graph.is_ancestor(root, *j) {
                    diags.push(Diagnostic::error(
                        "E1001",
                        format!(
                            "skin \"{}\" skeleton_root is not an ancestor of joint #{}",
                            skin.name, j.0
                        ),
                    ));
                }
            }
        }
        if skin.joints.len() != skin.inverse_bind_matrices.len() {
            diags.push(Diagnostic::error(
                "E1002",
                format!(
                    "skin \"{}\" has {} joints but {} inverse-bind matrices",
                    skin.name,
                    skin.joints.len(),
                    skin.inverse_bind_matrices.len()
                ),
            ));
        }
    }

    for (ni, node) in graph.nodes.iter().enumerate() {
        let Some(skin_id) = node.skin else { continue };
        let Some(skin) = graph.skins.get(skin_id.0 as usize) else {
            diags.push(Diagnostic::error(
                "E1003",
                format!("node #{ni} \"{}\" references unknown skin id {}", node.name, skin_id.0),
            ));
            continue;
        };
        let Some(mesh) = node.mesh.as_ref() else {
            diags.push(Diagnostic::error(
                "E1004",
                format!(
                    "node \"{}\" is bound to skin \"{}\" but has no mesh",
                    node.name, skin.name
                ),
            ));
            continue;
        };
        if mesh.joints.len() != mesh.positions.len()
            || mesh.weights.len() != mesh.positions.len()
        {
            diags.push(Diagnostic::error(
                "E1005",
                format!(
                    "node \"{}\" skinned mesh has {} positions but {} joints / {} weights",
                    node.name,
                    mesh.positions.len(),
                    mesh.joints.len(),
                    mesh.weights.len()
                ),
            ));
            continue;
        }
        let j_max = skin.joints.len() as u16;
        for (vi, (js, ws)) in mesh.joints.iter().zip(mesh.weights.iter()).enumerate() {
            for (slot, j) in js.iter().enumerate() {
                let w = ws[slot];
                if w > 0.0 && *j >= j_max {
                    diags.push(Diagnostic::error(
                        "E1006",
                        format!(
                            "node \"{}\" vertex {vi} slot {slot}: joint index {j} out of range (skin has {j_max} joints)",
                            node.name
                        ),
                    ));
                }
            }
            let sum: f32 = ws.iter().sum();
            if (sum - 1.0).abs() > WEIGHT_SUM_TOLERANCE {
                diags.push(Diagnostic::error(
                    "E1007",
                    format!(
                        "node \"{}\" vertex {vi} weights sum to {sum:.4}, expected ≈1.0",
                        node.name
                    ),
                ));
            }
        }
    }

    diags
}

/// Find clusters of mesh-bearing nodes whose world-space AABBs don't touch any
/// other cluster. In a correctly-assembled asset every visible part overlaps
/// (or is adjacent to) at least one other part, so multiple clusters usually
/// means the LLM emitted a `pos=[...]` that drifted. Returns a single
/// diagnostic listing the smaller clusters; the largest cluster is treated as
/// the "main body" of the scene.
///
/// Nodes whose tags include `floating` (directly, or via an ancestor) are
/// ignored — that's the opt-out for intentional gaps like a chandelier hanging
/// above a table.
fn check_connectivity(graph: &SceneGraph) -> Vec<Diagnostic> {
    let worlds = graph.world_transforms();
    let inherits_floating: Vec<bool> = compute_floating_flags(graph);

    // Gather mesh-bearing nodes with their world AABBs. Skip empty meshes and
    // nodes whose tags (or ancestors' tags) include `floating`.
    let mut entries: Vec<(NodeId, Aabb)> = Vec::new();
    for (i, n) in graph.nodes.iter().enumerate() {
        if n.mesh.is_none() {
            continue;
        }
        if inherits_floating[i] {
            continue;
        }
        let id = NodeId(i as u32);
        if let Some(aabb) = node_world_aabb(graph, id, worlds[i]) {
            entries.push((id, aabb.inflated(CONNECTIVITY_SLOP)));
        }
    }

    if entries.len() < 2 {
        return Vec::new();
    }

    // Union-find over the entries. Two entries merge if their inflated AABBs
    // intersect, OR if they're bound to the same skin — skinned meshes are
    // rigidly linked through their shared skeleton, so the geometric gap
    // between e.g. an arm mesh and the torso is not a real disconnection.
    let mut parent: Vec<usize> = (0..entries.len()).collect();
    let mut first_for_skin: std::collections::HashMap<u32, usize> =
        std::collections::HashMap::new();
    for (i, (id, _)) in entries.iter().enumerate() {
        if let Some(skin) = graph.nodes[id.0 as usize].skin {
            if let Some(&first) = first_for_skin.get(&skin.0) {
                union(&mut parent, first, i);
            } else {
                first_for_skin.insert(skin.0, i);
            }
        }
    }
    for i in 0..entries.len() {
        for j in (i + 1)..entries.len() {
            if entries[i].1.intersects(&entries[j].1) {
                union(&mut parent, i, j);
            }
        }
    }

    // Bucket entries by cluster root.
    let mut clusters: std::collections::BTreeMap<usize, Vec<NodeId>> =
        std::collections::BTreeMap::new();
    for (i, (id, _)) in entries.iter().enumerate() {
        let root = find(&mut parent, i);
        clusters.entry(root).or_default().push(*id);
    }

    if clusters.len() < 2 {
        return Vec::new();
    }

    // Pick the largest cluster as "main"; everything else is flagged.
    let mut bodies: Vec<Vec<NodeId>> = clusters.into_values().collect();
    bodies.sort_by_key(|c| std::cmp::Reverse(c.len()));
    let main = &bodies[0];
    let orphans = &bodies[1..];

    let name_list = |ids: &[NodeId]| -> String {
        ids.iter()
            .map(|id| format!("\"{}\"", graph.nodes[id.0 as usize].name))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut msg = format!(
        "scene has {} disconnected part cluster{} — the main body is [{}]",
        orphans.len() + 1,
        if orphans.len() == 0 { "" } else { "s" },
        name_list(main),
    );
    for (idx, orphan) in orphans.iter().enumerate() {
        msg.push_str(&format!(
            "; floating cluster {}: [{}]",
            idx + 1,
            name_list(orphan)
        ));
    }
    msg.push_str(
        ". Use `attach` to join these parts to the rest of the scene, or tag them \
         with `tags=\"floating\"` if the gap is intentional.",
    );

    vec![Diagnostic::error("E1101", msg)]
}

/// Per-node boolean: true if this node (or any ancestor) has `floating` in its
/// tag list. A `floating` tag exempts the subtree from the connectivity check.
fn compute_floating_flags(graph: &SceneGraph) -> Vec<bool> {
    let mut out = vec![false; graph.nodes.len()];
    fn walk(graph: &SceneGraph, id: NodeId, inherited: bool, out: &mut [bool]) {
        let n = &graph.nodes[id.0 as usize];
        let here = inherited || n.tags.iter().any(|t| t == "floating");
        out[id.0 as usize] = here;
        for c in &n.children {
            walk(graph, *c, here, out);
        }
    }
    for root in &graph.roots {
        walk(graph, *root, false, &mut out);
    }
    out
}

fn find(parent: &mut [usize], x: usize) -> usize {
    let mut root = x;
    while parent[root] != root {
        root = parent[root];
    }
    let mut cur = x;
    while parent[cur] != root {
        let next = parent[cur];
        parent[cur] = root;
        cur = next;
    }
    root
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra != rb {
        parent[ra] = rb;
    }
}

#[cfg(test)]
mod connectivity_tests {
    use super::*;
    use mgen_dsl::{lower, parse};

    fn diags(src: &str) -> Vec<Diagnostic> {
        let ast = parse(src).unwrap();
        let g = lower(&ast).unwrap();
        validate_graph(&g)
    }

    #[test]
    fn connected_chair_has_no_warning() {
        // The original chair example — legs touch seat, back touches seat.
        let d = diags(
            r#"
            scene {
              box "seat" (pos=[0, 0.5, 0], size=[1.0, 0.1, 1.0])
              box "back" (pos=[0, 1.0, -0.45], size=[1.0, 1.0, 0.1])
              box "leg_fl" (pos=[-0.45, 0.25, -0.45], size=[0.1, 0.5, 0.1])
              box "leg_fr" (pos=[ 0.45, 0.25, -0.45], size=[0.1, 0.5, 0.1])
              box "leg_bl" (pos=[-0.45, 0.25,  0.45], size=[0.1, 0.5, 0.1])
              box "leg_br" (pos=[ 0.45, 0.25,  0.45], size=[0.1, 0.5, 0.1])
            }
        "#,
        );
        assert!(d.is_empty(), "unexpected diagnostics: {d:?}");
    }

    #[test]
    fn floating_head_triggers_warning() {
        let d = diags(
            r#"
            scene {
              box "body" (pos=[0, 0, 0], size=[1, 2, 1])
              sphere "head" (pos=[0, 3, 0], radius=0.3)
            }
        "#,
        );
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, "E1101");
        assert!(d[0].message.contains("head"));
    }

    #[test]
    fn attach_pipeline_keeps_scene_connected() {
        // Same scene, but with `attach` — should reconnect.
        let d = diags(
            r#"
            scene {
              box "body" (size=[1, 2, 1])
              sphere "head" (radius=0.3)
            }
            attach (parent="body", child="head")
        "#,
        );
        assert!(d.is_empty(), "unexpected diagnostics: {d:?}");
    }

    #[test]
    fn floating_tag_opts_out() {
        let d = diags(
            r#"
            scene {
              box "body" (pos=[0, 0, 0], size=[1, 2, 1])
              sphere "halo" (pos=[0, 3, 0], radius=0.3, tags="floating")
            }
        "#,
        );
        assert!(d.is_empty(), "unexpected diagnostics: {d:?}");
    }

    #[test]
    fn slop_treats_near_touch_as_connected() {
        // A 1mm gap between two boxes — smaller than CONNECTIVITY_SLOP.
        let d = diags(
            r#"
            scene {
              box "a" (pos=[0, 0, 0], size=[1, 1, 1])
              box "b" (pos=[1.001, 0, 0], size=[1, 1, 1])
            }
        "#,
        );
        assert!(d.is_empty(), "unexpected diagnostics: {d:?}");
    }
}
