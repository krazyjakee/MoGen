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

pub fn compile(src: &str, source_dir: Option<&Path>) -> CompileResult {
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
