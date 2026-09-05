//! **A caller can ask for a tessellation density**, so the same AST lowers to
//! genuinely different geometry more than once.
//!
//! The DSL already had three ways for a *file* to state its density — the
//! top-level `lod_scale (value=N)`, each import's own, and a per-node `lod=N`.
//! What it had no way to express was a **caller's** request, and that is the one
//! a LOD chain needs: a consumer has to lower one scene at several densities
//! without editing the source between them.
//!
//! This matters because LOD here is *re-tessellation*, not decimation. A `.mog`
//! sphere is a sphere, so a coarse level is the same analytic description
//! sampled more coarsely — exact at every level, where a decimated mesh is
//! approximate at all of them.
//!
//! # The trap this design exists for
//!
//! `LOD_SCALE` is **replaced** by `LodOriginScaleGuard` on entry to every
//! imported subtree, deliberately, so one file's `lod_scale` cannot leak across
//! an `import`. A caller's request folded into that same cell is therefore
//! *erased* at the first import — the root file coarsens and every imported
//! subtree stays at full density, silently. `an_import_honours_the_request_too`
//! is the test that fails when the request is not kept separate.

use anyhow::Result;
use std::path::Path;

use mogen_dsl::module::{LoadedFile, Loader};
use mogen_dsl::{lower_with_loader, lower_with_loader_lod, parse};

/// Total triangles across every mesh in the graph — the thing a density is
/// supposed to move.
fn tris(graph: &mogen_core::SceneGraph) -> usize {
    graph.nodes.iter().filter_map(|n| n.mesh.as_ref()).map(|m| m.indices.len() / 3).sum()
}

fn lower_at(src: &str, lod: f32) -> Result<mogen_core::SceneGraph> {
    let ast = parse(src).expect("parses");
    let mut fs = mogen_dsl::FsLoader::new();
    lower_with_loader_lod(&ast, None, &mut fs, lod)
}

/// A sphere: its default tessellation is rings × segments, so a density
/// multiplier moves its triangle count in both directions.
const SPHERE: &str = r#"scene "s" { sphere "ball" (radius=1) }"#;

/// **N densities, N distinct meshes** — issue krazyjakee/moggy#168's exit.
#[test]
fn one_source_lowers_to_distinct_geometry_at_distinct_densities() {
    let counts: Vec<usize> =
        [0.25f32, 0.5, 1.0, 2.0].iter().map(|d| tris(&lower_at(SPHERE, *d).expect("lowers"))).collect();

    // Strictly increasing: each density is a different mesh, and in the
    // direction asked for. Equality anywhere would mean the knob saturated or
    // was ignored.
    for w in counts.windows(2) {
        assert!(w[0] < w[1], "densities did not produce distinct, ordered meshes: {counts:?}");
    }
}

/// **1.0 is exactly the old behaviour**, so every existing caller is untouched.
///
/// The control that matters: `lower_with_loader` must produce the *same* graph
/// as an explicit `1.0`, or "existing callers are unaffected" is a claim rather
/// than a property. Asserted on the triangle count and the node count together —
/// a request that changed the node structure would slip past either alone.
#[test]
fn a_request_of_one_is_the_untouched_lowering() {
    let ast = parse(SPHERE).expect("parses");
    let mut fs = mogen_dsl::FsLoader::new();
    let plain = lower_with_loader(&ast, None, &mut fs).expect("lowers");

    let explicit = lower_at(SPHERE, 1.0).expect("lowers");
    assert_eq!(tris(&plain), tris(&explicit));
    assert_eq!(plain.nodes.len(), explicit.nodes.len());

    // …and the knob is not simply inert: something else really does move it.
    assert_ne!(tris(&plain), tris(&lower_at(SPHERE, 4.0).expect("lowers")));
}

/// **A malformed request is 1.0, not zero.** `extract_lod_scale` already applies
/// this rule to the DSL directive, for the reason it must: a scale of 0 or NaN
/// silently destroys every mesh in the scene rather than failing loudly.
#[test]
fn a_malformed_request_falls_back_to_one_rather_than_erasing_the_scene() {
    let baseline = tris(&lower_at(SPHERE, 1.0).expect("lowers"));
    for bad in [0.0f32, -1.0, f32::NAN, f32::INFINITY] {
        let got = tris(&lower_at(SPHERE, bad).expect("lowers"));
        assert_eq!(got, baseline, "a request of {bad} did not fall back to 1.0");
    }
}

/// **The request multiplies the source's own scale rather than replacing it**,
/// so an authored detail hierarchy survives every density anyone asks for.
///
/// Two nodes, one marked `lod=2`. Whatever the caller requests, the marked one
/// must stay denser than its neighbour — an override would flatten them.
#[test]
fn an_authored_detail_hierarchy_survives_the_request() {
    let src = r#"scene "s" { sphere "hero" (radius=1, lod=2) }"#;
    let plain = r#"scene "s" { sphere "hero" (radius=1) }"#;

    for d in [0.5f32, 1.0, 2.0] {
        let marked = tris(&lower_at(src, d).expect("lowers"));
        let bare = tris(&lower_at(plain, d).expect("lowers"));
        assert!(
            marked > bare,
            "at density {d} the `lod=2` node was not denser than its plain twin \
             ({marked} vs {bare}) — the request replaced the authored scale instead of \
             multiplying it"
        );
    }
}

/// **An imported subtree honours the request too** — the trap in this module's
/// header, as a test.
///
/// `LodOriginScaleGuard` *replaces* `LOD_SCALE` on entry to imported geometry,
/// so a request stored in that same cell is erased there. The failure is silent
/// and partial: the root file coarsens, the import does not. Keeping the
/// request in its own cell is what makes this pass.
#[test]
fn an_import_honours_the_request_too() {
    /// Serves one importable file out of memory, so the test needs no fixtures
    /// on disk and the import graph is exactly what is written here.
    struct OneFile;
    impl Loader for OneFile {
        fn load(&mut self, spec: &str, _b: Option<&Path>) -> Result<LoadedFile> {
            assert_eq!(spec, "parts.mog");
            Ok(LoadedFile {
                canonical: std::path::PathBuf::from("parts.mog"),
                source: r#"module "part" () { sphere "s" (radius=1) }"#.to_string(),
            })
        }
    }

    let src = "import \"parts.mog\"\nscene \"s\" { use \"part\" () }\n";
    let ast = parse(src).expect("parses");

    let at = |d: f32| {
        let mut l = OneFile;
        tris(&lower_with_loader_lod(&ast, None, &mut l, d).expect("lowers"))
    };

    let (coarse, base, fine) = (at(0.25), at(1.0), at(4.0));
    assert!(coarse < base, "an imported subtree ignored a coarse request: {coarse} vs {base}");
    assert!(fine > base, "an imported subtree ignored a fine request: {fine} vs {base}");
}

#[test]
fn imported_lod_preserves_subtree_multipliers_and_restores_siblings() {
    struct Parts;
    impl Loader for Parts {
        fn load(&mut self, spec: &str, _base: Option<&Path>) -> Result<LoadedFile> {
            let source = match spec {
                "parts.mog" => {
                    r#"
                    lod_scale (value=0.5)
                    module "part" {
                        group (lod=2) {
                            sphere "nested" (radius=1, lod=0.5)
                            use "other"
                        }
                        sphere "import_sibling" (radius=1)
                    }
                "#
                }
                "other.mog" => {
                    r#"
                    lod_scale (value=0.25)
                    module "other" { sphere "other" (radius=1) }
                "#
                }
                _ => anyhow::bail!("unexpected import: {spec}"),
            };
            Ok(LoadedFile {
                canonical: spec.into(),
                source: source.into(),
            })
        }
    }
    let ast = parse(
        r#"
        import "parts.mog"
        import "other.mog"
        lod_scale (value=2)
        scene {
            group (lod=2) { use "part" }
            sphere "local_sibling" (radius=1)
        }
    "#,
    )
    .unwrap();
    let vertices = |graph: &mogen_core::SceneGraph, name: &str| {
        graph
            .nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap()
            .mesh
            .as_ref()
            .unwrap()
            .positions
            .len()
    };
    for request in [0.5, 1.0, 2.0] {
        let graph = lower_with_loader_lod(&ast, None, &mut Parts, request).unwrap();
        // Imported file scales replace the caller file's scale; all enclosing
        // per-node multipliers survive, including across another import.
        for (name, scale) in [
            ("nested", 0.5 * 2.0 * 2.0 * 0.5),
            ("other", 0.25 * 2.0 * 2.0),
            ("import_sibling", 0.5 * 2.0),
            ("local_sibling", 2.0),
        ] {
            let expected = lower_at(SPHERE, scale * request).unwrap();
            assert_eq!(
                vertices(&graph, name),
                vertices(&expected, "ball"),
                "{name}, request={request}"
            );
        }
    }
}
