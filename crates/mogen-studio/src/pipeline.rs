use std::path::Path;
use std::sync::Arc;

use mogen_core::{Diagnostic, SceneGraph, Severity, Span};
use mogen_export::{ExportOptions, ImposterAtlas};

pub struct CompileResult {
    /// Shared so the viewer and any other read-only consumers (autocomplete,
    /// inspector panels) can hold the scene without deep-cloning on tab
    /// switch — the scene is immutable post-compile.
    pub scene: Option<Arc<SceneGraph>>,
    pub diagnostics: Vec<Diagnostic>,
    pub stage: Stage,
    /// Per-node AST source spans, indexed by `NodeId.0`. Populated from
    /// `SceneNode::source_span` so the inspector can jump the editor caret
    /// without walking the graph. Parallel to `scene.nodes` — `None` at a
    /// slot means that node has no 1:1 DSL source (array/mirror/grid copy).
    pub node_spans: Vec<Option<Span>>,
}

impl CompileResult {
    fn new(scene: Option<SceneGraph>, diagnostics: Vec<Diagnostic>, stage: Stage) -> Self {
        let node_spans = scene
            .as_ref()
            .map(|s| s.nodes.iter().map(|n| n.source_span).collect())
            .unwrap_or_default();
        Self {
            scene: scene.map(Arc::new),
            diagnostics,
            stage,
            node_spans,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Parse,
    ValidateAst,
    Lower,
    ValidateGraph,
    Ok,
}

/// True if `path` names a MOGB binary container (`.mogb`) rather than a text
/// `.mog`. Extension-only check — content is not sniffed.
pub fn is_binary_source(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mogb"))
        .unwrap_or(false)
}

/// Read a `.mog` or `.mogb` file as DSL source text. `.mogb` is decoded through
/// `mogen_binary`; `.mog` is read verbatim. The editor, compiler, and every
/// other consumer only ever see text — the binary container is transparent, so
/// callers that previously did `fs::read_to_string` route through here instead.
pub fn read_source(path: &Path) -> std::io::Result<String> {
    if is_binary_source(path) {
        let bytes = std::fs::read(path)?;
        mogen_binary::unpack_to_source(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    } else {
        std::fs::read_to_string(path)
    }
}

/// Encode DSL source text into the on-disk byte payload for `path`: a MOGB
/// container for `.mogb`, UTF-8 text otherwise. Fails for a `.mogb` target
/// whose source doesn't parse (the caller should surface the error and leave
/// the existing file untouched rather than write a corrupt container).
pub fn encode_source(path: &Path, src: &str) -> std::io::Result<Vec<u8>> {
    if is_binary_source(path) {
        mogen_binary::pack_source(src)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    } else {
        Ok(src.as_bytes().to_vec())
    }
}

pub fn compile(src: &str, source_dir: Option<&Path>) -> CompileResult {
    // A `.mog` file that contains only `module "X" () { … }` (e.g. each
    // wizard per-object file) lowers to an empty scene because nothing
    // instantiates the module. Detect that pattern and append a transient
    // `scene { use "X" () }` so the previewer renders the body. The on-disk
    // source is untouched — the rewrite lives only in this compile call.
    let preview_src;
    let src = match mogen_dsl::synthesise_standalone_module_use(src) {
        Some(rewritten) => {
            preview_src = rewritten;
            preview_src.as_str()
        }
        None => src,
    };

    let ast = match mogen_dsl::parse(src) {
        Ok(a) => a,
        Err(e) => {
            return CompileResult::new(
                None,
                vec![Diagnostic::error("E0001", format!("parse: {e}"))],
                Stage::Parse,
            );
        }
    };

    let mut diags = mogen_validate::validate_ast_with_source(&ast, source_dir);
    if has_errors(&diags) {
        return CompileResult::new(None, diags, Stage::ValidateAst);
    }

    let scene = match mogen_dsl::lower_with_source(&ast, source_dir) {
        Ok(s) => s,
        Err(e) => {
            diags.push(Diagnostic::error("E0701", format!("lowering: {e}")));
            return CompileResult::new(None, diags, Stage::Lower);
        }
    };

    diags.extend(mogen_validate::validate_graph(&scene));
    if has_errors(&diags) {
        return CompileResult::new(Some(scene), diags, Stage::ValidateGraph);
    }

    CompileResult::new(Some(scene), diags, Stage::Ok)
}

/// Export `scene` as a GLB with the given `opts`, first resolving every
/// relative texture path on the scene's materials against `source_dir` (the
/// directory of the `.mog` file the user opened). Without this, a material
/// authored `base_color_texture="oak.png"` would be resolved against the GUI
/// process's cwd and almost never found. `progress` reports coarse stage
/// transitions ("merging sibling meshes", "packing textures", "writing glb")
/// so the Build GLB modal can show what's happening during a slow build.
pub fn write_glb_with_source_and_options<F: Fn(&str)>(
    scene: &SceneGraph,
    path: &Path,
    source_dir: Option<&Path>,
    opts: &ExportOptions,
    progress: F,
) -> anyhow::Result<()> {
    if let Some(dir) = source_dir {
        let mut resolved = scene.clone();
        resolved.resolve_texture_paths(dir);
        mogen_export::write_glb_with_options(&resolved, path, opts, progress)
    } else {
        mogen_export::write_glb_with_options(scene, path, opts, progress)
    }
}

/// Variant of [`write_glb_with_source_and_options`] that hands a caller-
/// supplied pre-baked imposter atlas to the exporter, sidestepping the
/// writer's own headless bake. Studio uses this when
/// `bundle_lods_and_imposter` is on — eframe owns the only winit
/// `EventLoop`, so the writer-internal bake fails with
/// `EventLoopError::RecreationAttempt`. Studio bakes via the live GL
/// context on the viewer thread first and threads the atlas through here.
pub fn write_glb_with_source_options_and_imposter<F: Fn(&str)>(
    scene: &SceneGraph,
    path: &Path,
    source_dir: Option<&Path>,
    opts: &ExportOptions,
    prebaked_imposter: Option<ImposterAtlas>,
    progress: F,
) -> anyhow::Result<()> {
    if let Some(dir) = source_dir {
        let mut resolved = scene.clone();
        resolved.resolve_texture_paths(dir);
        mogen_export::write_glb_with_prebaked_imposter(
            &resolved,
            path,
            opts,
            prebaked_imposter,
            progress,
        )
    } else {
        mogen_export::write_glb_with_prebaked_imposter(
            scene,
            path,
            opts,
            prebaked_imposter,
            progress,
        )
    }
}

fn has_errors(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| matches!(d.severity, Severity::Error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fresh_tempdir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mogen-studio-pipeline-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn is_binary_source_matches_only_mogb() {
        assert!(is_binary_source(Path::new("chair.mogb")));
        assert!(is_binary_source(Path::new("chair.MOGB")));
        assert!(!is_binary_source(Path::new("chair.mog")));
        assert!(!is_binary_source(Path::new("chair")));
    }

    #[test]
    fn read_source_reads_text_mog_verbatim() {
        let dir = fresh_tempdir("read-text");
        let path = dir.join("chair.mog");
        std::fs::write(&path, "box (size=[1,1,1])\n").unwrap();
        let src = read_source(&path).expect("read_source");
        assert_eq!(src, "box (size=[1,1,1])\n");
    }

    #[test]
    fn read_source_decodes_mogb() {
        let dir = fresh_tempdir("read-mogb");
        let src = "box (size=[1,1,1])\n";
        let ast = mogen_dsl::parse(src).unwrap();
        let bytes = mogen_binary::encode(&ast);
        let path = dir.join("chair.mogb");
        std::fs::write(&path, &bytes).unwrap();

        let decoded_src = read_source(&path).expect("read_source");
        let reparsed = mogen_dsl::parse(&decoded_src).expect("reparse");
        assert!(mogen_binary::nodes_equivalent(&ast, &reparsed));
    }

    #[test]
    fn read_source_surfaces_corrupt_mogb_as_error() {
        let dir = fresh_tempdir("read-corrupt");
        let path = dir.join("broken.mogb");
        std::fs::write(&path, b"not a mogb file").unwrap();
        assert!(read_source(&path).is_err());
    }

    #[test]
    fn encode_source_passes_text_through_unchanged() {
        let src = "box (size=[1,1,1])\n";
        let bytes = encode_source(Path::new("chair.mog"), src).expect("encode_source");
        assert_eq!(bytes, src.as_bytes());
    }

    #[test]
    fn encode_source_round_trips_through_mogb() {
        let src = "box (size=[1,1,1])\n";
        let bytes = encode_source(Path::new("chair.mogb"), src).expect("encode_source");
        let decoded_src = mogen_binary::unpack_to_source(&bytes).expect("decode");
        let original = mogen_dsl::parse(src).unwrap();
        let reparsed = mogen_dsl::parse(&decoded_src).unwrap();
        assert!(mogen_binary::nodes_equivalent(&original, &reparsed));
    }

    #[test]
    fn encode_source_rejects_unparseable_mogb_target() {
        // A `.mogb` target must parse to encode; malformed DSL source should
        // error rather than silently write a corrupt container.
        let err = encode_source(Path::new("broken.mogb"), "box (size=[").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
