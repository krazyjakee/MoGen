//! Golden-GLB regression tests.
//!
//! Each canonical example under `examples/<name>.mog` is compiled through the
//! full pipeline (parse → validate → lower → export) and its bytes are diffed
//! against the committed snapshot at `examples/<name>.glb`. A mismatch means
//! the generator's output has drifted — rerun `mogen build` on the example and
//! inspect the diff before committing the new snapshot.
//!
//! Update mode: set `MOGEN_GOLDENS_UPDATE=1` and the test overwrites the
//! snapshot instead of failing, so a deliberate change can be landed in one
//! step.
//!
//! gltf-validator (https://github.com/KhronosGroup/glTF-Validator) is invoked
//! when its `gltf_validator` binary is on PATH; absence is not a test failure.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/mogen; climb two to the workspace.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn compile_example(name: &str) -> Vec<u8> {
    let root = workspace_root();
    let src_path = root.join("examples").join(format!("{name}.mog"));
    let src = fs::read_to_string(&src_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", src_path.display()));

    let ast = mogen_dsl::parse(&src).expect("parse failed");
    let diags = mogen_validate::validate_ast(&ast);
    assert!(
        !mogen_core::has_errors(&diags),
        "{name}: validation errors: {}",
        mogen_validate::render_json("<test>", &diags)
    );
    let scene = mogen_dsl::lower(&ast).expect("lower failed");

    let tmp = env::temp_dir().join(format!(
        "mogen-goldens-{}-{name}.glb",
        std::process::id()
    ));
    mogen_export::write_glb(&scene, &tmp).expect("write_glb failed");
    let bytes = fs::read(&tmp).expect("reading temp glb");
    let _ = fs::remove_file(&tmp);
    bytes
}

fn golden_path(name: &str) -> PathBuf {
    workspace_root().join("examples").join(format!("{name}.glb"))
}

fn check_golden(name: &str) {
    let produced = compile_example(name);
    let golden = golden_path(name);

    if env::var_os("MOGEN_GOLDENS_UPDATE").is_some() {
        fs::write(&golden, &produced).expect("writing golden");
        eprintln!("updated golden: {}", golden.display());
        return;
    }

    let expected = fs::read(&golden)
        .unwrap_or_else(|e| panic!("no committed golden at {}: {e}. Run with MOGEN_GOLDENS_UPDATE=1 to create.", golden.display()));

    if produced != expected {
        let diff_path = env::temp_dir().join(format!("mogen-goldens-{name}.actual.glb"));
        let _ = fs::write(&diff_path, &produced);
        panic!(
            "{name}: GLB bytes differ from {} (got {} bytes, expected {}). Actual output written to {}. Re-run with MOGEN_GOLDENS_UPDATE=1 once the change is intentional.",
            golden.display(),
            produced.len(),
            expected.len(),
            diff_path.display()
        );
    }

    try_gltf_validator(&golden);
}

/// Best-effort glTF spec check. Absence of the binary is fine — the main
/// contract is the golden byte match. If present and it reports an error,
/// we surface it as a test failure so we never ship an invalid GLB.
fn try_gltf_validator(path: &Path) {
    let binary = env::var("MOGEN_GLTF_VALIDATOR").unwrap_or_else(|_| "gltf_validator".into());
    let status = Command::new(&binary)
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if !s.success() => panic!(
            "gltf_validator reported errors on {} (exit {s})",
            path.display()
        ),
        _ => {}
    }
}

macro_rules! golden_test {
    ($name:ident, $example:expr) => {
        #[test]
        fn $name() {
            check_golden($example);
        }
    };
}

golden_test!(chair, "chair");
golden_test!(chair_mat, "chair_mat");
golden_test!(chair_array, "chair_array");
golden_test!(chair_module, "chair_module");
golden_test!(hierarchy_test, "hierarchy_test");
golden_test!(mirror_test, "mirror_test");
golden_test!(wall_door, "wall_door");
golden_test!(door_open, "door_open");
golden_test!(windmill, "windmill");
golden_test!(table, "table");
golden_test!(sword, "sword");
golden_test!(drone, "drone");
golden_test!(simple_house, "simple_house");
