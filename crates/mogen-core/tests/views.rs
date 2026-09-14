use mogen_core::{AssetView, SceneGraph, Transform};

#[test]
fn named_views_face_the_labeled_side() {
    for view in [
        AssetView::Front,
        AssetView::ThreeQuarter,
        AssetView::Presentation,
    ] {
        let (yaw, _) = view.camera();
        assert!(yaw.cos() < 0.0, "{view:?} must look from asset front (-Z)");
    }
    assert!(AssetView::Back.camera().0.cos() > 0.0);
    assert!(AssetView::RearThreeQuarter.camera().0.cos() > 0.0);
}

#[test]
fn explicit_front_follows_named_node_and_parents() {
    let mut graph = SceneGraph::new();
    let parent = graph.add_root("assembly", "group", Transform::IDENTITY);
    graph.nodes[parent.0 as usize].transform.rotation =
        glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
    graph.add_child(parent, "marker", "group", Transform::IDENTITY);
    graph.meta = Some(mogen_core::Meta {
        front: Some("-z".into()),
        front_node: Some("marker".into()),
        ..Default::default()
    });
    let (yaw, _) = AssetView::Front.camera_for(&graph).unwrap();
    assert!(yaw.sin() < -0.99);
    graph.meta.as_mut().unwrap().front_node = Some("missing".into());
    assert!(AssetView::Front
        .camera_for(&graph)
        .unwrap_err()
        .contains("E0140"));
}
