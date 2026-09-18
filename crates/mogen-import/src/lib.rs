//! Read a mesh file (glTF / GLB today; OBJ and FBX to follow) and write it as
//! `.mog` source.
//!
//! Like [`mogen-pascal`], the output is *source*, not geometry: the user gets a
//! file they own and can edit, and `mogen build` takes it from there. Unlike
//! the pascal importer there is no architectural vocabulary to map onto, so the
//! baseline conversion is faithful rather than clever — every triangle mesh
//! becomes `poly` nodes, every node a `group` with its local transform.
//!
//! The work is split into a pipeline so recovery passes (primitive fitting,
//! symmetry, CSG) can be slotted in later without touching the readers:
//!
//! ```text
//! ingest   format reader → MeshScene (+ extracted texture assets)
//! normalize  sanitise and uniquify every name, in source order
//! convert  ordered list of passes over the IR (see `convert`)
//! emit     MeshScene → `.mog` text, header comment from the Report
//! ```
//!
//! Every stage walks index-addressed `Vec`s in source order — no map
//! iteration — so the same file always produces byte-identical output.
//!
//! ```no_run
//! let out = mogen_import::import_file(
//!     std::path::Path::new("chair.glb"),
//!     "chair",
//!     &mogen_import::Options::default(),
//! )?;
//! std::fs::write("chair.mog", &out.source)?;
//! for asset in &out.assets {
//!     std::fs::create_dir_all(std::path::Path::new(&asset.path).parent().unwrap())?;
//!     std::fs::write(&asset.path, &asset.bytes)?;
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! [`mogen-pascal`]: https://github.com/krazyjakee/MoGen/tree/master/crates/mogen-pascal

pub mod convert;
pub mod emit;
mod fmt;
pub mod gltf_reader;
pub mod ir;
pub mod normalize;
mod report;

use std::path::Path;

use anyhow::{bail, Context, Result};

pub use ir::MeshScene;
pub use report::Report;

/// A mesh format `mogen import` can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `.glb` or `.gltf` (with external or embedded buffers).
    Gltf,
    /// `.obj` — recognised so the CLI can say it is not supported yet.
    Obj,
    /// `.fbx` — recognised so the CLI can say it is not supported yet.
    Fbx,
}

impl Format {
    /// Pick a reader from the file extension, case-insensitively. `None` for
    /// anything that is not a mesh format (the CLI then tries pascal JSON).
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "glb" | "gltf" => Some(Format::Gltf),
            "obj" => Some(Format::Obj),
            "fbx" => Some(Format::Fbx),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Gltf => "glTF",
            Format::Obj => "OBJ",
            Format::Fbx => "FBX",
        }
    }
}

/// Knobs for the conversion. The defaults are what `mogen import` uses.
#[derive(Debug, Clone)]
pub struct Options {
    /// Decimal places for positions and UVs. Five is 10 µm on a metre-scale
    /// asset — below anything a renderer can show, and short enough to keep
    /// the file readable.
    pub precision: usize,
    /// Directory, relative to the emitted `.mog`, that extracted textures are
    /// written into and referenced from. `None` derives `<scene>_textures`.
    pub texture_dir: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            precision: 5,
            texture_dir: None,
        }
    }
}

/// A file the import needs written next to the `.mog` — an extracted texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// Path relative to the emitted `.mog`, `/`-separated. This is exactly the
    /// string the material references.
    pub path: String,
    pub bytes: Vec<u8>,
}

/// The result of an import: the file to write, the files it references, and
/// what could not be carried over.
#[derive(Debug)]
pub struct Import {
    pub source: String,
    pub assets: Vec<Asset>,
    pub report: Report,
}

/// What a reader hands to the rest of the pipeline.
#[derive(Debug, Default)]
pub struct Ingested {
    pub scene: MeshScene,
    pub assets: Vec<Asset>,
    pub report: Report,
}

/// Import a mesh file from disk, choosing the reader from its extension.
pub fn import_file(path: &Path, scene_name: &str, opts: &Options) -> Result<Import> {
    let Some(format) = Format::from_path(path) else {
        bail!(
            "{} is not a mesh file this importer reads (.glb, .gltf)",
            path.display()
        );
    };
    let source_label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    match format {
        Format::Gltf => {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let base = path.parent().filter(|p| !p.as_os_str().is_empty());
            import_gltf(&bytes, base, &source_label, scene_name, opts)
        }
        Format::Obj | Format::Fbx => bail!(
            "{} import is not supported yet — convert to .glb first (e.g. with Blender)",
            format.label()
        ),
    }
}

/// Import glTF / GLB bytes. `base` resolves external buffer and image URIs;
/// `None` rejects them (a self-contained `.glb` never needs it).
pub fn import_gltf(
    bytes: &[u8],
    base: Option<&Path>,
    source_label: &str,
    scene_name: &str,
    opts: &Options,
) -> Result<Import> {
    let texture_dir = opts
        .texture_dir
        .clone()
        .unwrap_or_else(|| format!("{}_textures", normalize::sanitize(scene_name, "imported")));
    let ingested = gltf_reader::read(bytes, base, &texture_dir)?;
    Ok(finish(
        ingested,
        Format::Gltf,
        source_label,
        scene_name,
        opts,
    ))
}

/// normalize → convert → emit, shared by every reader.
fn finish(
    ingested: Ingested,
    format: Format,
    source_label: &str,
    scene_name: &str,
    opts: &Options,
) -> Import {
    let Ingested {
        mut scene,
        assets,
        mut report,
    } = ingested;
    normalize::normalize(&mut scene);
    convert::run(&mut scene, &mut report, &convert::default_passes());
    report.tally(&scene);
    let source = emit::emit(
        &scene,
        &report,
        &assets,
        format,
        source_label,
        scene_name,
        opts,
    );
    // Only ship textures a surviving material still references.
    let used = emit::referenced_assets(&scene);
    let assets = assets
        .into_iter()
        .enumerate()
        .filter(|(i, _)| used.contains(i))
        .map(|(_, a)| a)
        .collect();
    Import {
        source,
        assets,
        report,
    }
}
