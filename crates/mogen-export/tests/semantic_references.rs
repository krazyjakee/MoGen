#![cfg(feature = "merge")]
#[test]
fn merged_export_keeps_relationship_and_guide_owners() {
    for source in [
        include_str!("../../../examples/features/joint_measurements.mog"),
        include_str!("../../../examples/furniture/guided_cushion.mog"),
    ] {
        let scene = mogen_dsl::lower(&mogen_dsl::parse(source).unwrap()).unwrap();
        let merged = mogen_export::merge::merge_sibling_meshes(&scene, |_| {});
        for (a, b) in scene.relationships.iter().zip(&merged.relationships) {
            assert_eq!(scene.get(a.child).name, merged.get(b.child).name);
            assert_eq!(scene.get(a.target).name, merged.get(b.target).name);
        }
        for (a, b) in scene.guides.iter().zip(&merged.guides) {
            assert_eq!(scene.get(a.target).name, merged.get(b.target).name);
        }
        assert_eq!(scene.relationships.len(), merged.relationships.len());
        assert_eq!(scene.guides.len(), merged.guides.len());
    }
}
