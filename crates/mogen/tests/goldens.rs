//! Golden-GLB regression tests.
//!
//! Two source roots:
//!   * `examples/<name>.mog` — user-facing demos; their `.glb` lives next to
//!     them so `mogen build` produces the same artifact users see.
//!   * `crates/mogen/tests/fixtures/<name>.mog` — minimal regression fixtures
//!     that exist purely to lock compiler output. `.glb` lives next to the
//!     `.mog`, isolated from anything users browse.
//!
//! Each fixture is compiled through the full pipeline (parse → validate →
//! lower → export) and byte-diffed against its committed `.glb`. A mismatch
//! means the generator drifted — re-run with `MOGEN_GOLDENS_UPDATE=1` to
//! overwrite the snapshot in one step, then inspect before committing.
//!
//! gltf-validator (https://github.com/KhronosGroup/glTF-Validator) is invoked
//! when its `gltf_validator` binary is on PATH; absence is not a test failure.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Copy, Clone)]
enum Source {
    /// Lives in `examples/` at the workspace root.
    Example,
    /// Lives in `crates/mogen/tests/fixtures/`.
    Fixture,
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/mogen; climb two to the workspace.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn source_dir(src: Source) -> PathBuf {
    match src {
        Source::Example => workspace_root().join("examples"),
        Source::Fixture => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures"),
    }
}

fn compile_example(name: &str, src: Source) -> Vec<u8> {
    let src_path = source_dir(src).join(format!("{name}.mog"));
    let source = fs::read_to_string(&src_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", src_path.display()));

    let ast = mogen_dsl::parse(&source).expect("parse failed");
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

fn golden_path(name: &str, src: Source) -> PathBuf {
    source_dir(src).join(format!("{name}.glb"))
}

fn check_golden(name: &str, src: Source) {
    let produced = compile_example(name, src);
    let golden = golden_path(name, src);

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
    ($name:ident, $file:expr, $src:expr) => {
        #[test]
        fn $name() {
            check_golden($file, $src);
        }
    };
}

// User-facing demos: live in examples/, double as goldens. Demos that
// reference external texture PNGs (e.g. `windmill`) are intentionally
// excluded — without committed texture assets their bytes aren't stable.
golden_test!(chair, "chair", Source::Example);
golden_test!(chair_mat, "chair_mat", Source::Example);
golden_test!(chair_array, "chair_array", Source::Example);
golden_test!(chair_module, "chair_module", Source::Example);
golden_test!(table, "table", Source::Example);
golden_test!(sword, "sword", Source::Example);
golden_test!(drone, "drone", Source::Example);
golden_test!(simple_house, "simple_house", Source::Example);

// Test-only fixtures: minimal scenes that lock specific compiler behaviours.
golden_test!(hierarchy_test, "hierarchy_test", Source::Fixture);
golden_test!(mirror_test, "mirror_test", Source::Fixture);
golden_test!(wall_door, "wall_door", Source::Fixture);
golden_test!(door_open, "door_open", Source::Fixture);
