use mogen_core::{
    has_errors, validate_renderable_mesh, validate_renderable_scene, Mesh, SceneGraph, Span,
    Transform,
};

fn triangle() -> Mesh {
    Mesh::new(
        vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0.0, 0.0, 1.0]; 3],
        vec![0, 1, 2],
    )
}

#[test]
fn open_surfaces_and_absent_optional_channels_are_valid() {
    assert!(!has_errors(&validate_renderable_mesh(&triangle())));
}

#[test]
fn malformed_streams_are_diagnosed_without_panicking() {
    let cases: &[(&str, fn(&mut Mesh))] = &[
        ("E1200", |m| m.normals.clear()),
        ("E1200", |m| {
            m.normals.pop();
        }),
        ("E1200", |m| m.uvs.push([0.0; 2])),
        ("E1202", |m| m.positions[1][0] = f32::NAN),
        ("E1202", |m| m.positions[1][0] = f32::INFINITY),
        ("E1203", |m| m.indices[0] = u32::MAX),
        ("E1203", |m| m.indices.push(0)),
        ("E1204", |m| m.normals.fill([0.0; 3])),
        ("E1204", |m| m.normals[0][0] = f32::NAN),
        ("E1204", |m| m.normals[0][0] = f32::INFINITY),
        ("E1205", |m| m.uvs = vec![[f32::NAN, 0.0]; 3]),
        ("E1206", |m| m.colors = vec![[f32::INFINITY; 4]; 3]),
        ("E1207", |m| m.weights = vec![[f32::NAN; 4]; 3]),
    ];
    for (code, corrupt) in cases {
        let mut mesh = triangle();
        corrupt(&mut mesh);
        let diags = validate_renderable_mesh(&mesh);
        assert!(diags.iter().any(|d| d.code == *code), "{code}: {diags:?}");
    }
}

#[test]
fn degenerate_faces_allow_zero_normals_and_are_advisory() {
    let mut mesh = triangle();
    mesh.positions[2] = mesh.positions[0];
    mesh.normals.fill([0.0; 3]);
    let diags = validate_renderable_mesh(&mesh);
    assert!(!has_errors(&diags));
    assert!(diags.iter().any(|d| d.code == "W1208"));
    assert!(!has_errors(&validate_renderable_mesh(&Mesh::default())));
}

#[test]
fn tiny_triangles_still_require_normals() {
    let mut mesh = triangle();
    mesh.positions[1][0] = 1e-30;
    mesh.positions[2][1] = 1e-30;
    mesh.normals.fill([0.0; 3]);
    assert!(validate_renderable_mesh(&mesh)
        .iter()
        .any(|d| d.code == "E1204"));
}

#[test]
fn seam_vertices_and_hard_edges_are_not_modified() {
    let mut mesh = triangle();
    mesh.positions
        .extend([[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]]);
    mesh.normals.extend([[0.0, 1.0, 0.0]; 3]);
    mesh.indices.extend([3, 4, 5]);
    mesh.uvs = vec![
        [0.0, 0.0],
        [1.0, 0.0],
        [0.0, 1.0],
        [1.0, 1.0],
        [0.0, 0.0],
        [1.0, 0.0],
    ];
    let before = serde_json::to_value(&mesh).unwrap();
    assert!(!has_errors(&validate_renderable_mesh(&mesh)));
    assert_eq!(before, serde_json::to_value(mesh).unwrap());
}

#[test]
fn finite_bounds_cannot_hide_bad_vertices_or_ancestors() {
    let mut graph = SceneGraph::new();
    let root = graph.add_root("assembly", "group", Transform::IDENTITY);
    let id = graph.add_child(root, "arm", "sweep", Transform::IDENTITY);
    let mut mesh = triangle();
    mesh.positions.push([f32::NAN, 0.0, 0.0]);
    mesh.normals.push([0.0, 0.0, 1.0]);
    graph.nodes[id.0 as usize].mesh = Some(mesh);
    graph.nodes[id.0 as usize].source_span = Some(Span::new(5, 25));
    graph.nodes[id.0 as usize].origin = Some("parts/arm.mog".into());
    graph.nodes[root.0 as usize].transform.translation.x = f32::INFINITY;
    let diags = validate_renderable_scene(&graph);
    assert!(diags.iter().any(|d| d.code == "E1201"));
    let position = diags.iter().find(|d| d.code == "E1202").unwrap();
    assert!(position.message.contains("arm"));
    assert_eq!(position.span, Some(Span::new(5, 25)));
    assert_eq!(position.file.as_deref(), Some("parts/arm.mog"));
    let json = serde_json::to_value(&diags).unwrap();
    assert_eq!(json.as_array().unwrap().len(), diags.len());
}

#[test]
fn rejects_world_overflow_singular_scale_and_invalid_quaternions() {
    let mut graph = SceneGraph::new();
    let root = graph.add_root("parent", "group", Transform::IDENTITY);
    let child = graph.add_child(root, "part", "mesh", Transform::IDENTITY);
    graph.nodes[child.0 as usize].mesh = Some(triangle());
    graph.nodes[root.0 as usize].transform.scale.x = 0.0;
    assert!(validate_renderable_scene(&graph)
        .iter()
        .any(|d| d.code == "E1201"));
    graph.nodes[root.0 as usize].transform = Transform::IDENTITY;
    graph.nodes[root.0 as usize].transform.translation.x = f32::MAX;
    graph.nodes[child.0 as usize]
        .mesh
        .as_mut()
        .unwrap()
        .positions[1][0] = f32::MAX;
    assert!(validate_renderable_scene(&graph)
        .iter()
        .any(|d| d.code == "E1202"));
    graph.nodes[root.0 as usize].transform.rotation = glam::Quat::from_xyzw(0.0, 0.0, 0.0, 0.0);
    assert!(validate_renderable_scene(&graph)
        .iter()
        .any(|d| d.code == "E1201"));
}

#[test]
fn malformed_hierarchy_is_rejected_without_recursive_traversal() {
    let mut graph = SceneGraph::new();
    let id = graph.add_root("cycle", "group", Transform::IDENTITY);
    graph.nodes[0].children.push(id);
    graph.nodes[0].children.push(mogen_core::NodeId(99));
    assert!(validate_renderable_scene(&graph)
        .iter()
        .any(|d| d.code == "E1210"));
}

#[test]
fn repeated_defects_produce_bounded_diagnostics() {
    let mut mesh = triangle();
    mesh.positions = vec![[f32::NAN; 3]; 10_000];
    mesh.normals = vec![[0.0; 3]; 10_000];
    mesh.indices = vec![u32::MAX; 30_000];
    let diags = validate_renderable_mesh(&mesh);
    assert_eq!(diags.len(), 2);
    assert!(diags[0].message.contains("10000"));
    assert!(diags[1].message.contains("30000"));
}
