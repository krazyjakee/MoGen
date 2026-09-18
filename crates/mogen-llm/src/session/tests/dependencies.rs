use super::super::*;
use crate::GenerateConfig;

#[test]
fn imported_dependencies_are_captured_and_stale_changes_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let module = "module \"chair\" () { box \"seat\" (size=[1,1,1]) }";
    std::fs::write(dir.path().join("chair.mog"), module).unwrap();
    let source = "import \"chair.mog\"\nscene { use \"chair\" () }";
    let w = ModelingWorkspace::new(source.into(), Some(dir.path().into()), vec![], None).unwrap();
    let old = w.revision().unwrap();
    assert_eq!(dependencies(source, Some(dir.path())).unwrap().len(), 1);
    std::fs::write(
        dir.path().join("chair.mog"),
        module.replace("1,1,1", "2,1,1"),
    )
    .unwrap();
    assert!(w.check_revision(&old).is_err());
    assert!(dependencies("import \"missing.mog\"\nscene {}", Some(dir.path())).is_err());
}

#[test]
fn recover_dependency_snapshot_without_overwriting_current_project() {
    let dir = tempfile::tempdir().unwrap();
    let module = "module \"chair\" () { box \"seat\" (size=[1,1,1]) }";
    std::fs::write(dir.path().join("chair.mog"), module).unwrap();
    let source = "import \"chair.mog\"\nscene { use \"chair\" () }";
    let mut p = ModelingProject::default();
    let i = p
        .record(
            source.into(),
            Some(dir.path()),
            &GenerateConfig::new("chair"),
            "fixture",
            "valid".into(),
            vec![],
        )
        .unwrap();
    std::fs::write(dir.path().join("chair.mog"), "new external edit").unwrap();
    let restored = p.candidates[i]
        .restore_copy(&dir.path().join("copy"))
        .unwrap();
    compile(
        &std::fs::read_to_string(restored).unwrap(),
        Some(&dir.path().join("copy")),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("chair.mog")).unwrap(),
        "new external edit"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("copy/chair.mog")).unwrap(),
        module
    );
}

#[test]
fn recovery_preserves_dependencies_named_like_the_entry_point() {
    let dir = tempfile::tempdir().unwrap();
    let module = "module \"chair\" () { box \"seat\" (size=[1,1,1]) }";
    std::fs::write(dir.path().join("restored.mog"), module).unwrap();
    std::fs::create_dir(dir.path().join("restored-1.mog")).unwrap();
    std::fs::write(
        dir.path().join("restored-1.mog/other.mog"),
        "module \"other\" () {}",
    )
    .unwrap();
    let source =
        "import \"restored.mog\"\nimport \"restored-1.mog/other.mog\"\nscene { use \"chair\" () }";
    let mut project = ModelingProject::default();
    let i = project
        .record(
            source.into(),
            Some(dir.path()),
            &GenerateConfig::new("chair"),
            "fixture",
            "valid".into(),
            vec![],
        )
        .unwrap();
    let destination = dir.path().join("copy");
    let entry = project.candidates[i].restore_copy(&destination).unwrap();
    assert_eq!(entry, destination.join("restored-2.mog"));
    assert_eq!(
        std::fs::read_to_string(destination.join("restored.mog")).unwrap(),
        module
    );
    let recovered = std::fs::read_to_string(entry).unwrap();
    assert_eq!(
        dependencies(&recovered, Some(&destination)).unwrap(),
        project.candidates[i].dependencies
    );
    compile(&recovered, Some(&destination)).unwrap();
}

#[test]
fn binary_mesh_dependencies_are_scoped_snapshotted_and_revision_checked() {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join("project");
    std::fs::create_dir(&project_dir).unwrap();
    std::fs::write(project_dir.join("part.glb"), b"original mesh bytes").unwrap();
    std::fs::write(project_dir.join("albedo"), b"texture bytes").unwrap();
    let source = "material \"surface\" (base_color_texture=albedo)\nscene { mesh \"part\" (src=\"part.glb\") }";
    let workspace =
        ModelingWorkspace::new(source.into(), Some(project_dir.clone()), vec![], None).unwrap();
    let expected = workspace.revision().unwrap();
    let mut project = ModelingProject::default();
    let i = project
        .record(
            source.into(),
            Some(&project_dir),
            &GenerateConfig::new("part"),
            "fixture",
            "snapshot".into(),
            vec![],
        )
        .unwrap();
    let candidate = &project.candidates[i];
    assert_eq!(candidate.dependencies.len(), 2);
    assert_eq!(
        candidate.dependencies[std::path::Path::new("part.glb")],
        b"original mesh bytes"
    );
    std::fs::write(project_dir.join("part.glb"), b"external edit").unwrap();
    assert!(workspace.check_revision(&expected).is_err());
    let destination = dir.path().join("recovered");
    candidate.restore_copy(&destination).unwrap();
    assert_eq!(
        std::fs::read(destination.join("part.glb")).unwrap(),
        b"original mesh bytes"
    );
    std::fs::write(dir.path().join("outside.glb"), b"outside project").unwrap();
    let error = compile(
        "scene { mesh \"part\" (src=\"../outside.glb\") }",
        Some(&project_dir),
    )
    .unwrap_err();
    assert!(error.to_string().contains("outside the modeling project"));
}

#[test]
fn measurements_reject_changed_imports() {
    let dir = tempfile::tempdir().unwrap();
    let module = "module \"joint\" () { box \"a\" () box \"b\" (pos=[0,1,0]) }";
    std::fs::write(dir.path().join("joint.mog"), module).unwrap();
    let source = "import \"joint.mog\"\nscene { use \"joint\" () }";
    let w = ModelingWorkspace::new(source.into(), Some(dir.path().into()), vec![], None).unwrap();
    let old = w.revision().unwrap();
    let result = w.measure(&old, "a", "b", None, None).unwrap();
    assert_eq!(result["revision"], old);
    std::fs::write(
        dir.path().join("joint.mog"),
        module.replace("0,1,0", "0,2,0"),
    )
    .unwrap();
    assert!(w.measure(&old, "a", "b", None, None).is_err());
}
