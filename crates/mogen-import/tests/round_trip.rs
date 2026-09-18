//! Every committed MoGen GLB, imported and rebuilt.
//!
//! These are the files most likely to be imported first (MoGen's own output),
//! and they span much of what the importer has to carry: deep hierarchy,
//! shared meshes, transparent and emissive materials, hard and soft edges,
//! and `floating` tags. (Textures and non-MoGen structure are covered by
//! `gltf_features.rs`.) For each:
//!
//! 1. import → the `.mog` parses, validates and lowers with no errors;
//! 2. build → the rebuilt GLB draws the same number of triangles, over the
//!    same world-space bounds, as the original;
//! 3. importing twice gives byte-identical text.
//!
//! The bounds check is a coarse stand-in for the fidelity metric tracked in
//! its own issue; it catches the transform, hierarchy and dropped-geometry
//! mistakes a baseline importer is most likely to make.

use std::fs;
use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3};
use mogen_import::{import_file, Options};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn golden_glbs() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut out = Vec::new();
    for dir in [
        root.join("examples"),
        root.join("crates/mogen/tests/fixtures"),
    ] {
        collect(&dir, &mut out);
    }
    out.sort();
    assert!(!out.is_empty(), "no golden GLBs found");
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "glb") {
            out.push(p);
        }
    }
}

/// Triangle count and world AABB of a GLB's default scene.
fn measure(bytes: &[u8]) -> (usize, Vec3, Vec3) {
    let (doc, buffers, _) = gltf::import_slice(bytes).expect("GLB reads");
    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .expect("a scene");
    let mut tris = 0;
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    let mut stack: Vec<(gltf::Node, Mat4)> = scene.nodes().map(|n| (n, Mat4::IDENTITY)).collect();
    while let Some((node, parent)) = stack.pop() {
        let world = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            for prim in mesh.primitives() {
                if prim.mode() != gltf::mesh::Mode::Triangles {
                    continue;
                }
                let r = prim.reader(|b| Some(&buffers[b.index()].0[..]));
                let Some(pos) = r.read_positions() else {
                    continue;
                };
                let pos: Vec<[f32; 3]> = pos.collect();
                tris += match r.read_indices() {
                    Some(i) => i.into_u32().count() / 3,
                    None => pos.len() / 3,
                };
                for p in pos {
                    let w = world.transform_point3(Vec3::from(p));
                    lo = lo.min(w);
                    hi = hi.max(w);
                }
            }
        }
        for c in node.children() {
            stack.push((c, world));
        }
    }
    (tris, lo, hi)
}

fn build(mog: &Path) -> Vec<u8> {
    let src = fs::read_to_string(mog).unwrap();
    let ast = mogen_dsl::parse(&src).unwrap_or_else(|e| panic!("{}: parse: {e}", mog.display()));
    let diags = mogen_validate::validate_ast_with_source(&ast, mog.parent());
    assert!(
        !mogen_core::has_errors(&diags),
        "{}: AST errors: {}",
        mog.display(),
        mogen_validate::render_json("import", &diags)
    );
    let mut graph = mogen_dsl::lower_with_source(&ast, mog.parent())
        .unwrap_or_else(|e| panic!("{}: lower: {e:#}", mog.display()));
    graph.resolve_texture_paths(mog.parent().unwrap());
    let diags = mogen_validate::validate_graph(&graph);
    assert!(
        !mogen_core::has_errors(&diags),
        "{}: graph errors: {}",
        mog.display(),
        mogen_validate::render_json("import", &diags)
    );
    let out = mog.with_extension("rebuilt.glb");
    mogen_export::write_glb(&graph, &out)
        .unwrap_or_else(|e| panic!("{}: export: {e:#}", mog.display()));
    fs::read(&out).unwrap()
}

#[test]
fn every_golden_glb_imports_and_rebuilds_faithfully() {
    let tmp = std::env::temp_dir().join(format!("mogen-import-rt-{}", std::process::id()));
    let mut failures = Vec::new();
    for glb in golden_glbs() {
        let stem = glb.file_stem().unwrap().to_string_lossy().into_owned();
        let dir = tmp.join(&stem);
        fs::create_dir_all(&dir).unwrap();

        let a = import_file(&glb, &stem, &Options::default())
            .unwrap_or_else(|e| panic!("{}: import: {e:#}", glb.display()));
        let b = import_file(&glb, &stem, &Options::default()).unwrap();
        assert_eq!(
            a.source,
            b.source,
            "{}: import is not deterministic",
            glb.display()
        );
        assert_eq!(a.assets, b.assets);

        let mog = dir.join(format!("{stem}.mog"));
        fs::write(&mog, &a.source).unwrap();
        for asset in &a.assets {
            let p = dir.join(&asset.path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, &asset.bytes).unwrap();
        }

        let original = measure(&fs::read(&glb).unwrap());
        let rebuilt = measure(&build(&mog));

        let extent = (original.2 - original.1).length().max(1e-3);
        let tol = extent * 1e-3;
        let bounds_ok = (original.1 - rebuilt.1).abs().max_element() <= tol
            && (original.2 - rebuilt.2).abs().max_element() <= tol;
        if original.0 != rebuilt.0 || !bounds_ok {
            failures.push(format!(
                "{stem}: original {} tris {:?}..{:?}, rebuilt {} tris {:?}..{:?}",
                original.0, original.1, original.2, rebuilt.0, rebuilt.1, rebuilt.2
            ));
        }
    }
    let _ = fs::remove_dir_all(&tmp);
    assert!(
        failures.is_empty(),
        "round trip drifted:\n{}",
        failures.join("\n")
    );
}
