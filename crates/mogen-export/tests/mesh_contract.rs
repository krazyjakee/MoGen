use mogen_core::{Mesh, SceneGraph, Transform};

fn scene() -> SceneGraph {
    let mut scene = SceneGraph::new();
    let id = scene.add_root("arm", "mesh", Transform::IDENTITY);
    scene.nodes[id.0 as usize].mesh = Some(Mesh::new(
        vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0.0, 0.0, 1.0]; 3],
        vec![0, 1, 2],
    ));
    scene
}

#[test]
fn export_preserves_shared_diagnostics_before_merge_or_upload() {
    for defect in 0..4 {
        let mut scene = scene();
        let mesh = scene.nodes[0].mesh.as_mut().unwrap();
        match defect {
            0 => mesh.normals.fill([0.0; 3]),
            1 => mesh.indices[0] = u32::MAX,
            2 => mesh.positions[1][0] = f32::NAN,
            _ => mesh.normals.clear(),
        }
        let expected = mogen_core::validate_renderable_scene(&scene);
        let err =
            mogen_export::build_glb_with_options(&scene, &Default::default(), |_| {}).unwrap_err();
        let actual = &err
            .downcast_ref::<mogen_core::MeshContractError>()
            .unwrap()
            .diagnostics;
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        #[cfg(feature = "fbx")]
        {
            let err = mogen_export::build_fbx_with_options(&scene, &Default::default(), |_| {})
                .unwrap_err();
            assert!(err
                .downcast_ref::<mogen_core::MeshContractError>()
                .is_some());
        }
    }
}

#[test]
fn optional_channels_and_empty_meshes_export() {
    let mut scene = scene();
    mogen_export::build_glb_with_options(&scene, &Default::default(), |_| {}).unwrap();
    scene.nodes[0].mesh = Some(Mesh::default());
    let bytes = mogen_export::build_glb_with_options(&scene, &Default::default(), |_| {}).unwrap();
    let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json: serde_json::Value = serde_json::from_slice(&bytes[20..20 + len]).unwrap();
    assert!(json["nodes"][0].get("mesh").is_none());
}
