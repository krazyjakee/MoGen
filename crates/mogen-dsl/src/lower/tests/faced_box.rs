use super::*;
use crate::lower::*;
use crate::parser::parse;

fn children_of<'a>(g: &'a SceneGraph, name: &str) -> Vec<&'a mogen_core::SceneNode> {
    let parent = find_mesh_node(g, name);
    parent.children.iter().map(|c| &g.nodes[c.0 as usize]).collect()
}

#[test]
fn faced_box_groups_faces_by_material() {
    // +Y is "lid", the other five faces are "side": two distinct materials, so
    // two frozen children (a 5-face group and a 1-face group), not six.
    let g = lower_src(
        r#"scene {
            material "lid"  (color=[0.8, 0.6, 0.2])
            material "side" (color=[0.3, 0.3, 0.3])
            box "crate" (
                size=[2, 1, 1],
                faces=["side", "side", "lid", "side", "side", "side"]
            )
        }"#,
    );
    let crate_node = find_mesh_node(&g, "crate");
    assert!(crate_node.mesh.is_none(), "faced box wrapper must carry no mesh of its own");
    assert!(crate_node.editable, "wrapper stays editable");

    let kids = children_of(&g, "crate");
    assert_eq!(kids.len(), 2, "two materials → two children");
    for k in &kids {
        assert!(!k.editable, "face-group children are frozen");
        assert!(k.material.is_some(), "each face group binds a material");
        let m = k.mesh.as_ref().expect("face group has a mesh");
        assert_eq!(m.indices.len() % 6, 0, "each face is a quad (6 indices)");
    }
    // Total faces across the children must equal six (5 sides + 1 lid).
    let total_quads: usize = kids
        .iter()
        .map(|k| k.mesh.as_ref().unwrap().indices.len() / 6)
        .sum();
    assert_eq!(total_quads, 6, "all six faces emitted exactly once");

    let lid = g.find_material("lid").unwrap();
    let lid_child = kids.iter().find(|k| k.material == Some(lid)).unwrap();
    assert_eq!(lid_child.mesh.as_ref().unwrap().indices.len() / 6, 1, "lid is a single face");
}

#[test]
fn faced_box_with_one_material_emits_one_child() {
    let g = lower_src(
        r#"scene {
            material "m" (color=[0.5, 0.5, 0.5])
            box "b" (faces=["m", "m", "m", "m", "m", "m"])
        }"#,
    );
    let kids = children_of(&g, "b");
    assert_eq!(kids.len(), 1, "uniform material collapses to one child");
    assert_eq!(kids[0].mesh.as_ref().unwrap().indices.len() / 6, 6, "all six faces in one group");
}

#[test]
fn faced_box_empty_entry_falls_back_to_node_material() {
    // The box carries mat="body"; the empty faces use it, "trim" overrides +Y.
    let g = lower_src(
        r#"scene {
            material "body" (color=[0.2, 0.2, 0.2])
            material "trim" (color=[0.9, 0.9, 0.1])
            box "b" (mat="body", faces=["", "", "trim", "", "", ""])
        }"#,
    );
    let body = g.find_material("body").unwrap();
    let trim = g.find_material("trim").unwrap();
    let kids = children_of(&g, "b");
    assert_eq!(kids.len(), 2);
    assert!(kids.iter().any(|k| k.material == Some(body)), "empty entries reuse the box material");
    assert!(kids.iter().any(|k| k.material == Some(trim)), "named entry overrides one face");
}

#[test]
fn faced_box_rejects_wrong_face_count() {
    let src = r#"scene {
        material "m" (color=[0.5, 0.5, 0.5])
        box "b" (faces=["m", "m", "m"])
    }"#;
    let ast = parse(src).unwrap();
    assert!(lower(&ast).is_err(), "faces must have exactly 6 entries");
}

#[test]
fn faced_box_rejects_unknown_material() {
    let src = r#"scene {
        box "b" (faces=["nope", "nope", "nope", "nope", "nope", "nope"])
    }"#;
    let ast = parse(src).unwrap();
    assert!(lower(&ast).is_err(), "unknown face material must error");
}

#[test]
fn faced_box_anchor_shifts_all_faces() {
    // anchor=bottom moves the whole box up so its base sits on y=0; every face
    // group must shift together, so the lowest vertex across all children is 0.
    let g = lower_src(
        r#"scene {
            material "m" (color=[0.5, 0.5, 0.5])
            box "b" (size=[1, 2, 1], anchor="bottom", faces=["m","m","m","m","m","m"])
        }"#,
    );
    let kids = children_of(&g, "b");
    let min_y = kids
        .iter()
        .flat_map(|k| k.mesh.as_ref().unwrap().positions.iter())
        .map(|p| p[1])
        .fold(f32::INFINITY, f32::min);
    assert!(min_y.abs() < 1e-3, "anchored base should rest at y=0 (got {min_y})");
}
