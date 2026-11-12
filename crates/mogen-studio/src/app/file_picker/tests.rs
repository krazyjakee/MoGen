use super::*;

struct ImportFixture(PathBuf);

impl ImportFixture {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mogen-import assets-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn glb(&self) -> PathBuf {
        let path = self.0.join("Door (0x73edf993.g1m).GLB");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/weapons/sword.glb"),
            &path,
        )
        .unwrap();
        fs::canonicalize(path).unwrap()
    }
}

impl Drop for ImportFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn import_glb_into_saved_and_unsaved_tabs_compiles_real_geometry() {
    let fixture = ImportFixture::new();
    let asset = fixture.glb();
    for source_dir in [None, Some(fixture.0.as_path())] {
        let path = import_path(&asset, source_dir);
        assert_eq!(Path::new(&path).is_absolute(), source_dir.is_none());
        let (source, added) = crate::edit::insert_import_selection("scene {}\n", &[&path]);
        assert_eq!(added, 1);
        let result = crate::pipeline::compile(&source, source_dir);
        assert_eq!(
            result.stage,
            crate::pipeline::Stage::Ok,
            "{:?}",
            result.diagnostics
        );
        let graph = result.scene.unwrap();
        assert!(graph
            .nodes
            .iter()
            .filter_map(|node| node.mesh.as_ref())
            .any(|mesh| !mesh.positions.is_empty()));
    }
}

#[test]
fn import_module_from_unsaved_tab_compiles_and_is_listed_once() {
    let fixture = ImportFixture::new();
    let module = fixture.0.join("Door parts.mog");
    fs::write(&module, "module \"Door\" { box (size=1) }\n").unwrap();
    let module = fs::canonicalize(module).unwrap();
    let path = import_path(&module, None);
    let (source, added) =
        crate::edit::insert_import_selection("scene { use \"Door\" }\n", &[&path]);
    assert_eq!(added, 1);
    let result = crate::pipeline::compile(&source, None);
    assert_eq!(
        result.stage,
        crate::pipeline::Stage::Ok,
        "{:?}",
        result.diagnostics
    );
    assert_eq!(crate::edit::list_imports(&source)[0].path, path);
    assert_eq!(
        crate::edit::insert_import_selection(&source, &[&path]),
        (source, 0)
    );
}
