#[test]
fn comparison_camera_is_unchanged_when_a_candidate_grows_out_of_frame() {
    let mut scene = mogen_core::SceneGraph::new();
    scene.add_root("arm", "mesh", mogen_core::Transform::IDENTITY);
    scene.nodes[0].mesh = Some(mogen_core::Mesh::new(
        vec![[0.0, 0.0, 0.0], [0.1, 0.0, 0.0], [0.0, 0.1, 0.0]],
        vec![[0.0, 0.0, 1.0]; 3],
        vec![0, 1, 2],
    ));
    let camera = mogen_render::OrbitCamera::default();
    let before = mogen_render::capture_info(&scene, &camera, "before", "front", None);
    scene.nodes[0].transform.translation.x = 100.0;
    let after = mogen_render::capture_info(&scene, &camera, "after", "front", None);
    assert_eq!(before.out_of_frame_vertices, 0);
    assert_eq!(after.out_of_frame_vertices, 3);
    assert_eq!(after.cropped_parts, vec!["arm"]);
    assert_eq!(before.target, after.target);
    assert_eq!(before.yaw, after.yaw);
    assert_eq!(before.distance, after.distance);
    assert_eq!(after.revision, "after");
}
