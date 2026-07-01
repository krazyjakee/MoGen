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

#[test]
fn faced_box_authored_uv_bakes_face_local_coords() {
    // +Z face carries an authored UV transform; giving it its own material
    // makes it a single-face child so the four baked UVs read out directly.
    // Spec worked example: uv = offset + scale * (localU, localV), where for
    // +Z the in-plane axes are (X, Y) and local coords are `v[ax] + size/2`.
    let g = lower_src(
        r#"scene {
            material "plain" (color=[0.5, 0.5, 0.5])
            material "art"   (color=[0.2, 0.4, 0.8])
            box "b" (
                size=[2, 2, 2],
                faces=[
                    "plain", "plain", "plain", "plain",
                    face("art", uv_scale=[2, 3], uv_offset=[0.5, 0.25]),
                    "plain"
                ]
            )
        }"#,
    );
    let art = g.find_material("art").unwrap();
    let kids = children_of(&g, "b");
    let art_child = kids.iter().find(|k| k.material == Some(art)).unwrap();
    let m = art_child.mesh.as_ref().unwrap();
    assert_eq!(m.positions.len(), 4, "+Z authored face is a single-face child");
    for (p, uv) in m.positions.iter().zip(m.uvs.iter()) {
        let local_u = p[0] + 1.0; // X + size.x/2
        let local_v = p[1] + 1.0; // Y + size.y/2
        let exp = [0.5 + 2.0 * local_u, 0.25 + 3.0 * local_v];
        assert!(
            (uv[0] - exp[0]).abs() < 1e-5 && (uv[1] - exp[1]).abs() < 1e-5,
            "uv {uv:?} != expected {exp:?} for corner {p:?}"
        );
    }
    // The box's min corner maps exactly to the offset.
    let has_origin = m.positions.iter().zip(m.uvs.iter()).any(|(p, uv)| {
        p[0] < 0.0 && p[1] < 0.0 && (uv[0] - 0.5).abs() < 1e-5 && (uv[1] - 0.25).abs() < 1e-5
    });
    assert!(has_origin, "min corner must map to the offset [0.5, 0.25]");
}

#[test]
fn faced_box_authored_uv_swap_transposes_axes() {
    // uv_swap=true swaps the two in-plane axes before scale/offset apply.
    let g = lower_src(
        r#"scene {
            material "plain" (color=[0.5, 0.5, 0.5])
            material "art"   (color=[0.2, 0.4, 0.8])
            box "b" (
                size=[2, 2, 2],
                faces=[
                    "plain", "plain", "plain", "plain",
                    face("art", uv_scale=[2, 3], uv_swap=true),
                    "plain"
                ]
            )
        }"#,
    );
    let art = g.find_material("art").unwrap();
    let kids = children_of(&g, "b");
    let art_child = kids.iter().find(|k| k.material == Some(art)).unwrap();
    let m = art_child.mesh.as_ref().unwrap();
    for (p, uv) in m.positions.iter().zip(m.uvs.iter()) {
        let local_u = p[0] + 1.0;
        let local_v = p[1] + 1.0;
        // swap: (localU, localV) -> (localV, localU) before scale; offset [0,0].
        let exp = [2.0 * local_v, 3.0 * local_u];
        assert!(
            (uv[0] - exp[0]).abs() < 1e-5 && (uv[1] - exp[1]).abs() < 1e-5,
            "swapped uv {uv:?} != expected {exp:?} for corner {p:?}"
        );
    }
}

#[test]
fn faced_box_face_call_without_uv_matches_bare_string() {
    // `face("m")` with no UV args is equivalent to the bare `"m"`: default Fit
    // UVs and it groups with bare `"m"` faces into a single child.
    let g = lower_src(
        r#"scene {
            material "m" (color=[0.5, 0.5, 0.5])
            box "b" (faces=[face("m"), "m", "m", "m", "m", "m"])
        }"#,
    );
    let kids = children_of(&g, "b");
    assert_eq!(kids.len(), 1, "face(\"m\") and \"m\" share a material → one child");
    let m = kids[0].mesh.as_ref().unwrap();
    for uv in &m.uvs {
        assert!(
            uv[0] >= -1e-6 && uv[0] <= 1.0 + 1e-6 && uv[1] >= -1e-6 && uv[1] <= 1.0 + 1e-6,
            "bare/face(\"m\") faces keep unit-square Fit UVs, got {uv:?}"
        );
    }
}

#[test]
fn faced_box_mixes_authored_and_bare_faces() {
    // Converter-shaped input: most faces bare, one authored, distinct sizes on
    // each axis. The authored +Z face bakes world-local UVs; with scale chosen
    // as 1/extent the mapping spans 0..1 across the face.
    let g = lower_src(
        r#"scene {
            material "wall"  (color=[0.6, 0.6, 0.6])
            material "panel" (color=[0.2, 0.5, 0.9])
            box "wall_seg" (
                size=[4, 3, 1],
                faces=[
                    "wall", "wall", "wall", "wall",
                    face("panel", uv_scale=[0.25, 0.333333], uv_offset=[0, 0]),
                    "wall"
                ]
            )
        }"#,
    );
    let panel = g.find_material("panel").unwrap();
    let kids = children_of(&g, "wall_seg");
    let panel_child = kids.iter().find(|k| k.material == Some(panel)).unwrap();
    let pm = panel_child.mesh.as_ref().unwrap();
    // +Z face on [4,3,1]: X∈[-2,2], Y∈[-1.5,1.5]; local coords 0..4 and 0..3.
    let max_uv = pm
        .uvs
        .iter()
        .fold([f32::MIN; 2], |a, u| [a[0].max(u[0]), a[1].max(u[1])]);
    let min_uv = pm
        .uvs
        .iter()
        .fold([f32::MAX; 2], |a, u| [a[0].min(u[0]), a[1].min(u[1])]);
    assert!(min_uv[0].abs() < 1e-5 && min_uv[1].abs() < 1e-5, "authored UV min at [0,0]");
    assert!((max_uv[0] - 1.0).abs() < 1e-4, "u spans 0..1 over 4m at 0.25 (got {})", max_uv[0]);
    assert!((max_uv[1] - 1.0).abs() < 1e-3, "v spans 0..1 over 3m at 1/3 (got {})", max_uv[1]);

    // Bare "wall" faces are untouched by the authored transform: their UVs match
    // a plain (no-authored-faces) box built the same way, face-for-face.
    let wall = g.find_material("wall").unwrap();
    let wall_child = kids.iter().find(|k| k.material == Some(wall)).unwrap();
    let reference = lower_src(
        r#"scene {
            material "wall" (color=[0.6, 0.6, 0.6])
            box "wall_seg" (size=[4, 3, 1], faces=["wall","wall","wall","wall","wall","wall"])
        }"#,
    );
    let ref_child = &children_of(&reference, "wall_seg")[0];
    let ref_uvs = &ref_child.mesh.as_ref().unwrap().uvs;
    for uv in &wall_child.mesh.as_ref().unwrap().uvs {
        assert!(
            ref_uvs.iter().any(|r| (r[0] - uv[0]).abs() < 1e-6 && (r[1] - uv[1]).abs() < 1e-6),
            "bare face UV {uv:?} must match the unmodified box path"
        );
    }
}

#[test]
fn faces_all_bare_strings_still_parse_as_liststring() {
    // Backward-compat: a faces list with no face(...) entries must stay a plain
    // ListString so existing files and tooling see no behavioural change.
    use crate::ast::Value;
    let ast = parse(r#"box "b" (faces=["a","a","a","a","a","a"])"#).unwrap();
    match ast[0].attr("faces").unwrap() {
        Value::ListString(v) => assert_eq!(v.len(), 6),
        other => panic!("expected ListString, got {other:?}"),
    }
}

#[test]
fn faces_with_face_call_parse_as_facelist() {
    use crate::ast::Value;
    let ast =
        parse(r#"box "b" (faces=[face("a", uv_scale=[1,1]),"a","a","a","a","a"])"#).unwrap();
    match ast[0].attr("faces").unwrap() {
        Value::FaceList(v) => {
            assert_eq!(v.len(), 6);
            assert!(v[0].uv.is_some(), "face(...) entry carries a UV transform");
            assert!(v[1].uv.is_none(), "bare string entry carries no UV transform");
        }
        other => panic!("expected FaceList, got {other:?}"),
    }
}

#[test]
fn faced_box_anchor_shifts_default_connectors() {
    // Default connectors (top/bottom/etc.) must shift with the anchor so they
    // stay flush with their faces. For anchor=bottom on a [1,2,1] box:
    //   bottom connector was at y=-1, after shift sits at y=0.
    //   top connector was at y=+1, after shift sits at y=+2.
    let g = lower_src(
        r#"scene {
            material "m" (color=[0.5, 0.5, 0.5])
            box "b" (size=[1, 2, 1], anchor="bottom", faces=["m","m","m","m","m","m"])
        }"#,
    );
    let node = find_mesh_node(&g, "b");
    let bottom = node
        .connectors
        .iter()
        .find(|c| c.name == "bottom")
        .expect("box should have a bottom connector");
    let top = node
        .connectors
        .iter()
        .find(|c| c.name == "top")
        .expect("box should have a top connector");
    assert!(
        bottom.pos.y.abs() < 1e-3,
        "bottom connector should be at y=0 after anchor=bottom (got {})",
        bottom.pos.y
    );
    assert!(
        (top.pos.y - 2.0).abs() < 1e-3,
        "top connector should be at y=2 after anchor=bottom (got {})",
        top.pos.y
    );
}
