#[test]
fn mesh_contract_rejects_before_opening_a_graphics_context() {
    let mut scene = mogen_core::SceneGraph::new();
    scene.add_root("broken", "mesh", mogen_core::Transform::IDENTITY);
    scene.nodes[0].mesh = Some(mogen_core::Mesh::new(
        vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0.0; 3]; 3],
        vec![0, 1, 2],
    ));
    let err = mogen_render::headless::render_thumbnail(&scene, &Default::default()).unwrap_err();
    assert!(err
        .downcast_ref::<mogen_core::MeshContractError>()
        .unwrap()
        .diagnostics
        .iter()
        .any(|d| d.code == "E1204"));
}
