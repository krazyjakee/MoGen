#[test]
fn legacy_sweep_keeps_its_kernel_output() {
    let source = "scene { sweep \"arm\" (profile=[[-0.1,-0.02],[0.1,-0.02],[0.1,0.02],[-0.1,0.02]],path=[[0,0,0],[0,0,1]],samples=8) }";
    let scene = mogen_dsl::lower(&mogen_dsl::parse(source).unwrap()).unwrap();
    let node = scene.nodes.iter().find(|n| n.name == "arm").unwrap();
    let expected = mogen_geom::sweep_mesh(
        &[[-0.1, -0.02], [0.1, -0.02], [0.1, 0.02], [-0.1, 0.02]],
        &[[0.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        8,
        0.0,
        &Default::default(),
        true,
        Default::default(),
    );
    assert_eq!(node.mesh.as_ref().unwrap().positions, expected.positions);
    assert_eq!(node.mesh.as_ref().unwrap().normals, expected.normals);
    assert_eq!(node.path_frame.as_ref().unwrap()["version"], 0);
}

#[test]
fn explicit_frame_remains_local_under_parent_rotation() {
    let source = "scene { group \"assembly\" (rot=[0,90,0]) { sweep \"arm\" (profile=[[-0.1,-0.02],[0.1,-0.02],[0.1,0.02],[-0.1,0.02]],path=[[0,0,0],[0,0,1]],frame_up=[0,1,0]) } }";
    let scene = mogen_dsl::lower(&mogen_dsl::parse(source).unwrap()).unwrap();
    let (i, node) = scene
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.name == "arm")
        .unwrap();
    let info = node.path_frame.as_ref().unwrap();
    assert_eq!(info["height"], serde_json::json!([0.0, 1.0, 0.0]));
    let world_tangent = scene.world_transforms()[i].transform_vector3(glam::Vec3::Z);
    assert!((world_tangent.x.abs() - 1.0).abs() < 1e-5);
}
