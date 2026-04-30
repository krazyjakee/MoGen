//! Export-time transform that collapses groups of same-material, non-skinned
//! leaf sibling meshes into a single CSG-unioned mesh under each parent.
//!
//! The output scene preserves:
//! - hierarchy (non-mergeable nodes are kept with their original transforms),
//! - skins, skin joints, clips, animation targets (merge is skipped for any
//!   node reachable from those),
//! - UVs on merged meshes, as long as every operand in a group had UVs (the
//!   BSP clipper in `mogen-geom` interpolates them through splits); a narrow
//!   streak can show at a UV seam where the clip crosses the wrap, which is
//!   intrinsic to any per-vertex-attribute CSG.
//!
//! Two entry points:
//! - [`merge_sibling_meshes`] — global pass, merges under every parent.
//!   Triggered by `ExportOptions::merge_sibling_meshes`.
//! - [`merge_solid_groups`] — scoped pass, only merges under nodes tagged
//!   `"solid"` (produced by the `solid { … }` DSL kind). Also applies the
//!   coplanar-opposite cull when the solid node carries `cleanup=coplanar`.
//!
//! Connectors on merged leaves are dropped. They are resolved during DSL
//! lowering (see `mogen-dsl/src/attach.rs`) and never emitted to glTF, so by
//! the time this pass runs they carry no semantic value — keeping the gate on
//! them would block every primitive (which auto-synthesizes six face
//! connectors) from ever merging. Non-mergeable nodes still keep theirs
//! through `copy_node_shell`.

use std::collections::{HashMap, HashSet};

use mogen_core::{MaterialId, Mesh, NodeId, SceneGraph, SceneNode, Transform};

#[derive(Clone, Copy)]
enum CleanupMode {
    Standard,
    Coplanar,
}

/// Build a new scene graph by applying the merge transform. The original
/// graph is left untouched.
pub fn merge_sibling_meshes(scene: &SceneGraph, mut progress: impl FnMut(&str)) -> SceneGraph {
    progress("merging sibling meshes");
    merge_with(scene, |_src_parent, _scene| Some(CleanupMode::Standard))
}

/// Scoped merge: only collapses leaf siblings whose parent scene node carries
/// the `"solid"` tag (the lowering hook for the `solid { … }` DSL kind).
/// Clones the graph only if at least one solid node exists.
pub fn merge_solid_groups(scene: &SceneGraph, mut progress: impl FnMut(&str)) -> SceneGraph {
    let has_solid = scene
        .nodes
        .iter()
        .any(|n| n.tags.iter().any(|t| t == "solid"));
    if !has_solid {
        return scene.clone();
    }
    progress("merging solid groups");
    merge_with(scene, |src_parent, scene| {
        let p = src_parent?;
        let tags = &scene.nodes[p.0 as usize].tags;
        if !tags.iter().any(|t| t == "solid") {
            return None;
        }
        if tags.iter().any(|t| t == "cleanup=coplanar") {
            Some(CleanupMode::Coplanar)
        } else {
            Some(CleanupMode::Standard)
        }
    })
}

fn merge_with<P>(scene: &SceneGraph, policy: P) -> SceneGraph
where
    P: Fn(Option<NodeId>, &SceneGraph) -> Option<CleanupMode>,
{
    let protected = collect_protected(scene);

    let mut out = SceneGraph {
        nodes: Vec::new(),
        roots: Vec::new(),
        materials: scene.materials.clone(),
        joints: scene.joints.clone(),
        clips: scene.clips.clone(),
        skins: scene.skins.clone(),
    };
    let mut remap: HashMap<NodeId, NodeId> = HashMap::new();

    let new_roots = rebuild_level(
        scene,
        &scene.roots,
        None,
        &protected,
        &mut out,
        None,
        &mut remap,
        &policy,
    );
    out.roots = new_roots;

    // Skin / clip NodeIds referenced pre-merge may have shifted — every
    // "protected" node was copied so has a remap entry; rewrite those so the
    // emitted GLB references the new indices.
    for skin in &mut out.skins {
        for j in &mut skin.joints {
            if let Some(&n) = remap.get(j) {
                *j = n;
            }
        }
        if let Some(r) = skin.skeleton_root.as_mut() {
            if let Some(&n) = remap.get(r) {
                *r = n;
            }
        }
    }
    for clip in &mut out.clips {
        for t in &mut clip.tracks {
            if let Some(&n) = remap.get(&t.node) {
                t.node = n;
            }
        }
    }

    out
}

/// NodeIds that must survive the merge pass unchanged (referenced by skins or
/// clip tracks). They are copied into the new scene and remapped; they are
/// never candidates for merging.
fn collect_protected(scene: &SceneGraph) -> HashSet<NodeId> {
    let mut set = HashSet::new();
    for skin in &scene.skins {
        for j in &skin.joints {
            set.insert(*j);
        }
        if let Some(r) = skin.skeleton_root {
            set.insert(r);
        }
    }
    for clip in &scene.clips {
        for t in &clip.tracks {
            set.insert(t.node);
        }
    }
    set
}

fn rebuild_level<P>(
    scene: &SceneGraph,
    ids: &[NodeId],
    src_parent: Option<NodeId>,
    protected: &HashSet<NodeId>,
    out: &mut SceneGraph,
    new_parent: Option<NodeId>,
    remap: &mut HashMap<NodeId, NodeId>,
    policy: &P,
) -> Vec<NodeId>
where
    P: Fn(Option<NodeId>, &SceneGraph) -> Option<CleanupMode>,
{
    // Split each child into "copy as-is" and "merge candidate" buckets. The
    // only candidates are *leaves* with a plain (non-skinned) mesh, no skin
    // binding, not referenced by skins or clips. Preserving ordering matters
    // for determinism so we also remember original position.
    let merge_here = policy(src_parent, scene);
    let mut passthrough: Vec<NodeId> = Vec::new();
    let mut groups: HashMap<Option<MaterialId>, Vec<NodeId>> = HashMap::new();
    for &id in ids {
        let n = &scene.nodes[id.0 as usize];
        if merge_here.is_some() && is_mergeable(n, id, protected) {
            groups.entry(n.material).or_default().push(id);
        } else {
            passthrough.push(id);
        }
    }

    let mut result: Vec<NodeId> = Vec::new();

    for id in passthrough {
        let new_id = copy_node_shell(scene, id, out, new_parent);
        remap.insert(id, new_id);
        let kids: Vec<NodeId> = scene.nodes[id.0 as usize].children.clone();
        let new_kids = rebuild_level(
            scene,
            &kids,
            Some(id),
            protected,
            out,
            Some(new_id),
            remap,
            policy,
        );
        out.nodes[new_id.0 as usize].children = new_kids;
        result.push(new_id);
    }

    let cleanup = merge_here.unwrap_or(CleanupMode::Standard);
    for (material, ids) in groups {
        if ids.len() < 2 {
            let id = ids[0];
            let new_id = copy_node_shell(scene, id, out, new_parent);
            remap.insert(id, new_id);
            result.push(new_id);
            continue;
        }

        let merged_mesh = merge_group_meshes(scene, &ids, cleanup);

        // If the CSG pass produced nothing usable, fall back to keeping the
        // originals rather than writing an empty node that would break the
        // bounds computation in the exporter.
        if merged_mesh.positions.is_empty() || merged_mesh.indices.is_empty() {
            for id in ids {
                let new_id = copy_node_shell(scene, id, out, new_parent);
                remap.insert(id, new_id);
                result.push(new_id);
            }
            continue;
        }

        let merged = build_merged_node(scene, &ids, material, merged_mesh, new_parent);
        let new_id = push_node(out, merged);
        result.push(new_id);
    }

    result
}

fn is_mergeable(n: &SceneNode, id: NodeId, protected: &HashSet<NodeId>) -> bool {
    if protected.contains(&id) {
        return false;
    }
    if !n.children.is_empty() {
        return false;
    }
    if n.skin.is_some() {
        return false;
    }
    match &n.mesh {
        Some(m) if !m.is_skinned() => true,
        _ => false,
    }
}

fn copy_node_shell(
    scene: &SceneGraph,
    id: NodeId,
    out: &mut SceneGraph,
    new_parent: Option<NodeId>,
) -> NodeId {
    let src = &scene.nodes[id.0 as usize];
    let mut copied = src.clone();
    copied.parent = new_parent;
    copied.children.clear();
    push_node(out, copied)
}

fn push_node(out: &mut SceneGraph, node: SceneNode) -> NodeId {
    let id = NodeId(out.nodes.len() as u32);
    out.nodes.push(node);
    id
}

fn merge_group_meshes(scene: &SceneGraph, ids: &[NodeId], cleanup: CleanupMode) -> Mesh {
    let baked: Vec<Mesh> = ids
        .iter()
        .map(|&id| {
            let n = &scene.nodes[id.0 as usize];
            let mesh = n
                .mesh
                .as_ref()
                .expect("is_mergeable ensures mesh is Some");
            mogen_geom::transform_mesh(mesh, n.transform.to_mat4())
        })
        .collect();
    let unioned = mogen_geom::union_many(&baked);
    // Weld verts generated by BSP clipping and cull any degenerate fragments
    // before handing off to the exporter — same cleanup applied to in-DSL
    // CSG output.
    let cleaned = mogen_geom::clean_csg_output(&unioned);
    match cleanup {
        CleanupMode::Standard => cleaned,
        CleanupMode::Coplanar => mogen_geom::cull_coplanar_opposites(&cleaned),
    }
}

fn build_merged_node(
    scene: &SceneGraph,
    _ids: &[NodeId],
    material: Option<MaterialId>,
    mesh: Mesh,
    new_parent: Option<NodeId>,
) -> SceneNode {
    let mut name = String::from("merged");
    if let Some(mid) = material {
        let mat_name = &scene.materials[mid.0 as usize].name;
        if !mat_name.is_empty() {
            name = format!("merged_{mat_name}");
        }
    }
    // Emitter picks up `kind` into glTF extras for debugging; label the node
    // so tools downstream can see it came from the merge pass.
    SceneNode {
        name,
        kind: "merged".into(),
        transform: Transform::default(),
        mesh: Some(mesh),
        material,
        skin: None,
        parent: new_parent,
        children: Vec::new(),
        connectors: Vec::new(),
        tags: vec!["merged".into()],
        role: None,
        source_span: None,
        editable: false,
        relative_placed: false,
        attach_binding: None,
        use_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogen_core::{Connector, Material, Transform};
    use mogen_geom::box_mesh;

    fn scene_with_two_boxes(same_material: bool) -> SceneGraph {
        let mut s = SceneGraph::new();
        let oak = s.add_material(Material::new("oak"));
        let pine = s.add_material(Material::new("pine"));
        let root = s.add_root("group", "group", Transform::default());
        let a = s.add_child(root, "a", "box", Transform::from_translation([0.0, 0.0, 0.0].into()));
        let b = s.add_child(
            root,
            "b",
            "box",
            Transform::from_translation([0.5, 0.0, 0.0].into()),
        );
        s.set_mesh(a, box_mesh([1.0, 1.0, 1.0], mogen_core::UvMode::default()));
        s.set_mesh(b, box_mesh([1.0, 1.0, 1.0], mogen_core::UvMode::default()));
        s.set_material(a, oak);
        s.set_material(b, if same_material { oak } else { pine });
        s
    }

    #[test]
    fn merges_two_same_material_siblings_into_one_leaf() {
        let s = scene_with_two_boxes(true);
        let out = merge_sibling_meshes(&s, |_| {});
        // One group root + one merged leaf.
        let leaf_count = out.nodes.iter().filter(|n| n.mesh.is_some()).count();
        assert_eq!(leaf_count, 1, "expected the two boxes to collapse to one");
        let merged = out.nodes.iter().find(|n| n.kind == "merged").unwrap();
        assert!(!merged.mesh.as_ref().unwrap().positions.is_empty());
        assert!(!merged.editable);
    }

    #[test]
    fn does_not_merge_across_materials() {
        let s = scene_with_two_boxes(false);
        let out = merge_sibling_meshes(&s, |_| {});
        let leaf_count = out.nodes.iter().filter(|n| n.mesh.is_some()).count();
        assert_eq!(leaf_count, 2, "different materials must stay separate leaves");
    }

    #[test]
    fn leaves_solo_mesh_alone() {
        // One mesh under the group — nothing to merge, output should match
        // the input structurally.
        let mut s = SceneGraph::new();
        let mat = s.add_material(Material::new("oak"));
        let root = s.add_root("group", "group", Transform::default());
        let a = s.add_child(root, "a", "box", Transform::default());
        s.set_mesh(a, box_mesh([1.0, 1.0, 1.0], mogen_core::UvMode::default()));
        s.set_material(a, mat);

        let out = merge_sibling_meshes(&s, |_| {});
        assert_eq!(out.nodes.len(), 2);
        assert!(out.nodes.iter().all(|n| n.kind != "merged"));
    }

    #[test]
    fn merges_siblings_that_carry_auto_connectors() {
        // Every DSL primitive lowers with a set of face connectors — regression
        // guard for the bug where `is_mergeable` rejected anything with
        // connectors, leaving primitive-only scenes byte-identical after merge.
        let mut s = scene_with_two_boxes(true);
        for n in s.nodes.iter_mut().skip(1) {
            n.connectors.push(Connector::from_at_dir(
                "top",
                [0.0, 0.5, 0.0].into(),
                [0.0, 1.0, 0.0].into(),
                "face",
                None,
            ));
        }
        let out = merge_sibling_meshes(&s, |_| {});
        let leaf_count = out.nodes.iter().filter(|n| n.mesh.is_some()).count();
        assert_eq!(leaf_count, 1);
        let merged = out.nodes.iter().find(|n| n.kind == "merged").unwrap();
        assert!(merged.connectors.is_empty());
    }

    #[test]
    fn solid_merge_scopes_to_tagged_parent_only() {
        // Two solid groups, each with two same-material box siblings; one also
        // sits beside a plain group with two same-material boxes that must NOT
        // merge because its parent carries no "solid" tag.
        let mut s = SceneGraph::new();
        let oak = s.add_material(Material::new("oak"));

        let solid_root = s.add_root("shell", "solid", Transform::default());
        s.nodes[solid_root.0 as usize].tags.push("solid".into());
        let a = s.add_child(solid_root, "a", "box", Transform::default());
        let b = s.add_child(
            solid_root,
            "b",
            "box",
            Transform::from_translation([0.5, 0.0, 0.0].into()),
        );
        s.set_mesh(a, box_mesh([1.0, 1.0, 1.0], mogen_core::UvMode::default()));
        s.set_mesh(b, box_mesh([1.0, 1.0, 1.0], mogen_core::UvMode::default()));
        s.set_material(a, oak);
        s.set_material(b, oak);

        let plain_root = s.add_root("other", "group", Transform::default());
        let c = s.add_child(plain_root, "c", "box", Transform::default());
        let d = s.add_child(
            plain_root,
            "d",
            "box",
            Transform::from_translation([2.0, 0.0, 0.0].into()),
        );
        s.set_mesh(c, box_mesh([1.0, 1.0, 1.0], mogen_core::UvMode::default()));
        s.set_mesh(d, box_mesh([1.0, 1.0, 1.0], mogen_core::UvMode::default()));
        s.set_material(c, oak);
        s.set_material(d, oak);

        let out = merge_solid_groups(&s, |_| {});
        // solid subtree collapses to one leaf; plain subtree keeps both.
        let merged_leaves = out
            .nodes
            .iter()
            .filter(|n| n.kind == "merged")
            .count();
        assert_eq!(merged_leaves, 1);
        let plain_kids = out
            .nodes
            .iter()
            .filter(|n| n.parent.is_some())
            .filter(|n| {
                let p = n.parent.unwrap();
                out.nodes[p.0 as usize].kind == "group"
            })
            .count();
        assert_eq!(plain_kids, 2, "non-solid parent must not merge its kids");
    }

    #[test]
    fn solid_merge_no_op_without_solid_tag() {
        // Same as scene_with_two_boxes but no solid node anywhere; output must
        // be structurally equivalent (no merge happens).
        let s = scene_with_two_boxes(true);
        let out = merge_solid_groups(&s, |_| {});
        let leaf_count = out.nodes.iter().filter(|n| n.mesh.is_some()).count();
        assert_eq!(leaf_count, 2);
    }

    #[test]
    fn merged_output_preserves_uvs_when_operands_have_them() {
        // Both box meshes come from `box_mesh` which always emits UVs, so the
        // unioned merge result must also carry UVs (the CSG threads them
        // through, and `clean_csg_output` keeps them when present).
        let s = scene_with_two_boxes(true);
        let out = merge_sibling_meshes(&s, |_| {});
        let merged = out.nodes.iter().find(|n| n.kind == "merged").unwrap();
        let mesh = merged.mesh.as_ref().unwrap();
        assert!(mesh.has_uvs(), "merged mesh should keep UVs from input primitives");
        assert_eq!(mesh.positions.len(), mesh.uvs.len());
    }
}
