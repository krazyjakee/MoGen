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
//! lower → export) and compared against its committed `.glb`. The GLB
//! header, JSON chunk, BIN chunk header, and integer-typed accessor data
//! must match byte-for-byte; float-typed accessor data compares with a
//! small ULP tolerance so cross-host LLVM float scheduling/associativity
//! drift (Apple Silicon vs Intel on organic primitives like `wave`-with-
//! `range` and helical sweeps) doesn't masquerade as a regression. A
//! genuine drift — structural change or a float moved more than a few ULPs
//! — fails the test; re-run with `MOGEN_GOLDENS_UPDATE=1` to overwrite
//! once the change is intentional.
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

    let stem = name.rsplit('/').next().unwrap_or(name);
    let tmp = env::temp_dir().join(format!(
        "mogen-goldens-{}-{stem}.glb",
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
        if let Err(detail) = check_glb_match(&expected, &produced) {
            let stem = name.rsplit('/').next().unwrap_or(name);
            let diff_path = env::temp_dir().join(format!("mogen-goldens-{stem}.actual.glb"));
            let _ = fs::write(&diff_path, &produced);
            panic!(
                "{name}: GLB does not match {} (got {} bytes, expected {}).\n{detail}\nActual output written to {}.\nRe-run with MOGEN_GOLDENS_UPDATE=1 once the change is intentional.",
                golden.display(),
                produced.len(),
                expected.len(),
                diff_path.display()
            );
        }
    }

    try_gltf_validator(&golden);
}

/// Tolerance for float-accessor comparisons. Cross-host LLVM scheduling
/// occasionally re-associates `u * u * (3 - 2u)`-style smoothstep products
/// and similar helix-sweep expressions, producing 1-ULP shifts. For values
/// near zero those 1-ULP shifts can still translate to many "ULPs of the
/// value itself" (the helix-axis verts in `coil_spring` sit at ~7e-5 m and
/// drift by ~1.5e-10 m, which is ~21 ULP of `7e-5_f32`). So we compare with
/// a hybrid absolute+relative tolerance: a value matches if either the
/// absolute difference is under `ABS_TOL` or the relative difference is
/// under `REL_TOL`. Both are far below any visually meaningful change at
/// the scales mogen produces (mesh sizes in [10⁻²..10¹] m), but well above
/// observed cross-host noise.
const ABS_TOL: f32 = 1.0e-5;
const REL_TOL: f32 = 1.0e-5;

/// Compare a produced GLB against its committed golden, byte-exact
/// everywhere except float accessor data, which compares with the
/// `ABS_TOL`/`REL_TOL` hybrid. Returns `Ok(())` on a semantic match,
/// otherwise a short description of the first handful of offending
/// bytes/floats.
fn check_glb_match(expected: &[u8], actual: &[u8]) -> Result<(), String> {
    if expected.len() != actual.len() {
        return Err(format!(
            "GLB length differs: got {} bytes, expected {}",
            actual.len(),
            expected.len()
        ));
    }
    if expected.len() < 20 {
        return Err("GLB too short to contain a header + JSON chunk header".into());
    }
    if expected[..12] != actual[..12] {
        return Err("GLB header (magic/version/length) differs".into());
    }
    if expected[12..20] != actual[12..20] {
        return Err("JSON chunk header differs".into());
    }
    let json_len = u32::from_le_bytes(expected[12..16].try_into().unwrap()) as usize;
    let json_start = 20;
    let json_end = json_start + json_len;
    if json_end > expected.len() {
        return Err("JSON chunk length exceeds GLB length".into());
    }
    if expected[json_start..json_end] != actual[json_start..json_end] {
        return Err("JSON chunk body differs (structural drift)".into());
    }
    let bin_chunk_start = json_end;
    let bin_data_start = bin_chunk_start + 8;
    if bin_data_start > expected.len() {
        // JSON-only GLB — already verified above.
        return Ok(());
    }
    if expected[bin_chunk_start..bin_data_start] != actual[bin_chunk_start..bin_data_start] {
        return Err("BIN chunk header differs".into());
    }

    // Parse the JSON chunk and mark every BIN byte that belongs to a
    // FLOAT accessor (componentType 5126). Everything else compares
    // byte-for-byte.
    let json_str = std::str::from_utf8(&expected[json_start..json_end])
        .map_err(|e| format!("JSON chunk is not UTF-8: {e}"))?
        .trim_end_matches(['\0', ' ']);
    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("JSON parse: {e}"))?;
    let bin_len = expected.len() - bin_data_start;
    let mut is_f32 = vec![false; bin_len];
    if let (Some(accs), Some(bvs)) = (
        parsed.get("accessors").and_then(|v| v.as_array()),
        parsed.get("bufferViews").and_then(|v| v.as_array()),
    ) {
        for acc in accs {
            if acc.get("componentType").and_then(|v| v.as_u64()) != Some(5126) {
                continue;
            }
            let Some(bv_idx) = acc.get("bufferView").and_then(|v| v.as_u64()) else {
                continue;
            };
            let Some(bv) = bvs.get(bv_idx as usize) else { continue };
            let bv_offset = bv.get("byteOffset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let bv_length = bv.get("byteLength").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let acc_offset = acc.get("byteOffset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let count = acc.get("count").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let n_components = match acc.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "SCALAR" => 1,
                "VEC2" => 2,
                "VEC3" => 3,
                "VEC4" | "MAT2" => 4,
                "MAT3" => 9,
                "MAT4" => 16,
                _ => continue,
            };
            let span = count.saturating_mul(n_components).saturating_mul(4);
            let start = bv_offset + acc_offset;
            let end = (start + span).min(bv_offset + bv_length).min(bin_len);
            if start <= end {
                is_f32[start..end].fill(true);
            }
        }
    }

    let bin_e = &expected[bin_data_start..];
    let bin_a = &actual[bin_data_start..];
    let mut violations: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bin_len {
        if i + 4 <= bin_len && (0..4).all(|k| is_f32[i + k]) {
            let e = f32::from_le_bytes(bin_e[i..i + 4].try_into().unwrap());
            let a = f32::from_le_bytes(bin_a[i..i + 4].try_into().unwrap());
            if !approx_eq_f32(e, a) {
                let diff = (e - a).abs();
                violations.push(format!(
                    "BIN+{i}: f32 {e:e} vs {a:e} (Δ {diff:e})"
                ));
                if violations.len() >= 10 {
                    break;
                }
            }
            i += 4;
        } else {
            if bin_e[i] != bin_a[i] {
                violations.push(format!(
                    "BIN+{i}: byte {} vs {} (non-float region)",
                    bin_e[i], bin_a[i]
                ));
                if violations.len() >= 10 {
                    break;
                }
            }
            i += 1;
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} BIN difference(s) beyond float tolerance (abs {ABS_TOL:e}, rel {REL_TOL:e}):\n  {}",
            violations.len(),
            violations.join("\n  ")
        ))
    }
}

/// Hybrid absolute/relative f32 equality. Two values match if their
/// absolute difference is within `ABS_TOL`, or their relative difference
/// (against the larger magnitude) is within `REL_TOL`. NaNs never compare
/// equal, equal sign-bit zeros do.
fn approx_eq_f32(a: f32, b: f32) -> bool {
    if a == b {
        return true;
    }
    if a.is_nan() || b.is_nan() {
        return false;
    }
    let diff = (a - b).abs();
    if diff <= ABS_TOL {
        return true;
    }
    let scale = a.abs().max(b.abs());
    diff <= REL_TOL * scale
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
golden_test!(chair, "furniture/chair", Source::Example);
golden_test!(chair_mat, "furniture/chair_mat", Source::Example);
golden_test!(chair_array, "furniture/chair_array", Source::Example);
golden_test!(chair_module, "furniture/chair_module", Source::Example);
golden_test!(table, "furniture/table", Source::Example);
golden_test!(sword, "weapons/sword", Source::Example);
golden_test!(drone, "vehicles/drone", Source::Example);
golden_test!(simple_house, "buildings/simple_house", Source::Example);
// Organic-shape primitives showcase examples — locked as goldens so a
// regression in `coil`/`wave`/`heightfield`/`bezier_patch`/`metaball`
// surfaces immediately.
golden_test!(coil_spring, "features/coil_spring", Source::Example);
golden_test!(wave_water, "nature/wave_water", Source::Example);
golden_test!(heightfield_terrain, "nature/heightfield_terrain", Source::Example);
golden_test!(bezier_fender, "features/bezier_fender", Source::Example);
golden_test!(metaball_blob, "nature/metaball_blob", Source::Example);

// Test-only fixtures: minimal scenes that lock specific compiler behaviours.
golden_test!(hierarchy_test, "hierarchy_test", Source::Fixture);
golden_test!(mirror_test, "mirror_test", Source::Fixture);
golden_test!(wall_door, "wall_door", Source::Fixture);
golden_test!(door_open, "door_open", Source::Fixture);
