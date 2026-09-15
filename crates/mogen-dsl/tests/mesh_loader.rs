//! **`mesh (src=…)` goes through the `Loader`**, so a host with no filesystem
//! can serve an external mesh.
//!
//! `import "…"` has always been injectable — that is what lets an in-browser
//! caller answer it out of a pre-fetched file map. `mesh (src=…)` was not: the
//! primitive read its `.glb` with a direct `fs::read` against a thread-local
//! source directory, *outside the loader seam entirely*. So a caller could
//! satisfy every import in a scene and still fail on the first external mesh,
//! which meant a scene using one could not be lowered in a browser at all.
//!
//! `Loader::load_binary` closes that. It has a default impl that performs the
//! same disk resolution, so this is strictly additive — see
//! `only_a_loader_that_answers_changes_anything` below, which is the control.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use mogen_dsl::module::{FsLoader, LoadedFile, Loader};
use mogen_dsl::{lower_with_loader, lower_with_source, parse};

/// A real `.glb` from the examples tree, read as bytes. Using a genuine file
/// rather than a fabricated buffer keeps the test about the *seam*: if the
/// bytes reach the tessellator, a mesh comes out with vertices in it.
fn glb_bytes() -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/weapons/sword.glb");
    std::fs::read(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()))
}

/// A loader with **no filesystem at all** — every path method refuses, and only
/// `load_binary` answers, out of memory. That is the shape of the browser
/// caller this feature exists for, and it is why the refusals are `bail!`
/// rather than a fallback: if lowering succeeds through this loader, nothing it
/// needed came off disk.
struct MemoryLoader {
    spec: String,
    bytes: Vec<u8>,
    /// How many times `load_binary` was asked, so "it served the mesh" is not
    /// confused with "the primitive quietly read the file itself".
    calls: usize,
}

impl Loader for MemoryLoader {
    fn load(&mut self, spec: &str, _base: Option<&Path>) -> Result<LoadedFile> {
        bail!("this loader has no filesystem, and nothing should have asked it for `{spec}`")
    }

    fn load_binary(&mut self, spec: &str, _base: Option<&Path>) -> Result<Vec<u8>> {
        self.calls += 1;
        if spec == self.spec {
            return Ok(self.bytes.clone());
        }
        bail!("no such asset: {spec}")
    }
}

/// The spec names a file that **is not on disk anywhere**, so a lowering that
/// reached the filesystem cannot pass by accident.
const ABSENT: &str = "assets/not-on-disk-anywhere.glb";

fn scene() -> Vec<mogen_dsl::Node> {
    parse(&format!("scene \"s\" {{\n  mesh \"m\" (src=\"{ABSENT}\")\n}}\n")).expect("parses")
}

#[test]
fn a_loader_can_serve_a_mesh_that_is_not_on_disk() {
    let bytes = glb_bytes();
    assert!(bytes.len() > 100, "the fixture GLB is empty");
    assert!(
        !PathBuf::from(ABSENT).exists(),
        "the test's premise is that this path does not resolve"
    );

    let mut loader = MemoryLoader { spec: ABSENT.to_string(), bytes, calls: 0 };
    let graph = lower_with_loader(&scene(), None, &mut loader).expect("lowers through the loader");

    assert_eq!(loader.calls, 1, "the loader was not the route the mesh took");
    // …and the bytes really became geometry, rather than an empty node that
    // happens not to error.
    let verts: usize = graph.nodes.iter().filter_map(|n| n.mesh.as_ref()).map(|m| m.positions.len()).sum();
    assert!(verts > 0, "the scene lowered with no vertices — the GLB was not tessellated");
}

/// **The control.** The same scene through the ordinary filesystem path must
/// still *fail*, because the file genuinely is not there.
///
/// Without this, the test above is satisfied by a build where `mesh (src=…)`
/// silently produced an empty mesh instead of an error — which is exactly the
/// failure a "it lowered!" assertion invites.
#[test]
fn only_a_loader_that_answers_changes_anything() {
    let err = lower_with_source(&scene(), None).expect_err("a missing GLB must still be an error");
    let text = format!("{err:#}");
    assert!(
        text.contains(ABSENT),
        "the disk path's error should still name the file it could not read: {text}"
    );
}

/// **The default `load_binary` resolves the way the primitive always did**, and
/// it is tested *directly* because nothing else can see it.
///
/// Every loader that does not override it — `FsLoader`, i.e. every desktop
/// lowering — reaches disk through this method, and yet a break in it is
/// **invisible end to end**: the pre-pass fails, the primitive falls back to
/// reading the file itself, and the scene lowers correctly anyway. A deliberate
/// break (drop `base_dir` from the resolution) was measured to pass all three
/// tests above. That is the fallback masking the very path it backs up, which
/// is why the default impl now *calls* `read_glb_bytes` rather than restating
/// its rule — and why the assertions below are on the method, not on a lowering.
#[test]
fn the_default_load_binary_resolves_relative_specs_against_the_base_dir() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/weapons");
    let mut fs_loader = FsLoader::new();

    let relative = fs_loader.load_binary("sword.glb", Some(&dir)).expect("relative + base");
    assert_eq!(relative, glb_bytes(), "the default impl read a different file");

    let absolute = fs_loader
        .load_binary(dir.join("sword.glb").to_str().expect("utf-8 path"), None)
        .expect("an absolute spec needs no base");
    assert_eq!(absolute, relative, "absolute and relative disagree on the same file");

    // The control: `base_dir` is genuinely consulted, so "it found the file"
    // is not satisfied by an impl that ignores the base and got lucky on cwd.
    assert!(
        fs_loader.load_binary("sword.glb", Some(Path::new("/nonexistent-base"))).is_err(),
        "the base directory is not being used"
    );
}

/// A loader that declines a spec leaves the primitive reading the file itself,
/// so **every loader written before `load_binary` existed keeps working**.
///
/// Asserted with a real file: the loader refuses, and the mesh loads anyway.
#[test]
fn a_declining_loader_falls_back_to_the_disk_read() {
    struct Declines(usize);
    impl Loader for Declines {
        fn load(&mut self, _s: &str, _b: Option<&Path>) -> Result<LoadedFile> {
            bail!("no imports in this scene")
        }
        fn load_binary(&mut self, _s: &str, _b: Option<&Path>) -> Result<Vec<u8>> {
            self.0 += 1;
            bail!("I do not serve binaries")
        }
    }

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/weapons");
    let ast = parse("scene \"s\" {\n  mesh \"m\" (src=\"sword.glb\")\n}\n").expect("parses");

    let mut loader = Declines(0);
    let graph = lower_with_loader(&ast, Some(&dir), &mut loader).expect("falls back to disk");
    assert_eq!(loader.0, 1, "the loader should have been asked first");
    let verts: usize = graph.nodes.iter().filter_map(|n| n.mesh.as_ref()).map(|m| m.positions.len()).sum();
    assert!(verts > 0, "the fallback did not load the mesh");
}

#[test]
fn expanded_mesh_instances_load_once_and_keep_independent_geometry() {
    let ast = parse(&format!(
        r#"
        module "unused" {{ mesh (src="unused.glb") }}
        module "part" {{ mesh (src="{ABSENT}") }}
        scene {{
            use "part"
            mesh "anchored" (src="{ABSENT}", anchor=bottom)
            use "part"
        }}
    "#
    ))
    .unwrap();
    let mut loader = MemoryLoader {
        spec: ABSENT.into(),
        bytes: glb_bytes(),
        calls: 0,
    };
    let graph = lower_with_loader(&ast, None, &mut loader).unwrap();
    assert_eq!(
        loader.calls, 1,
        "deduplicate after expansion and unused module removal"
    );
    let meshes: Vec<_> = graph.nodes.iter().filter_map(|n| n.mesh.as_ref()).collect();
    assert_eq!(meshes.len(), 3);
    assert_eq!(meshes[0].positions, meshes[2].positions);
    let min_y = meshes[1]
        .positions
        .iter()
        .map(|p| p[1])
        .fold(f32::INFINITY, f32::min);
    assert!(min_y.abs() < 1e-5, "anchor should affect only its own mesh");
}

#[test]
fn mesh_bytes_do_not_leak_between_lowerings() {
    let mut loader = MemoryLoader {
        spec: ABSENT.into(),
        bytes: glb_bytes(),
        calls: 0,
    };
    lower_with_loader(&scene(), None, &mut loader).unwrap();
    loader.spec = "a-different-asset.glb".into();
    assert!(lower_with_loader(&scene(), None, &mut loader).is_err());
    assert_eq!(loader.calls, 2);
}
