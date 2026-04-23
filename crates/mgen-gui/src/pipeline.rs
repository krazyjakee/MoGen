use std::path::Path;

use mgen_core::{Diagnostic, SceneGraph, Severity};

pub struct CompileResult {
    pub scene: Option<SceneGraph>,
    pub diagnostics: Vec<Diagnostic>,
    pub stage: Stage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Parse,
    ValidateAst,
    Lower,
    ValidateGraph,
    Ok,
}

pub fn compile(src: &str) -> CompileResult {
    let ast = match mgen_dsl::parse(src) {
        Ok(a) => a,
        Err(e) => {
            return CompileResult {
                scene: None,
                diagnostics: vec![Diagnostic::error("E0001", format!("parse: {e}"))],
                stage: Stage::Parse,
            };
        }
    };

    let mut diags = mgen_validate::validate_ast(&ast);
    if has_errors(&diags) {
        return CompileResult {
            scene: None,
            diagnostics: diags,
            stage: Stage::ValidateAst,
        };
    }

    let scene = match mgen_dsl::lower(&ast) {
        Ok(s) => s,
        Err(e) => {
            diags.push(Diagnostic::error("E0701", format!("lowering: {e}")));
            return CompileResult {
                scene: None,
                diagnostics: diags,
                stage: Stage::Lower,
            };
        }
    };

    diags.extend(mgen_validate::validate_graph(&scene));
    if has_errors(&diags) {
        return CompileResult {
            scene: Some(scene),
            diagnostics: diags,
            stage: Stage::ValidateGraph,
        };
    }

    CompileResult {
        scene: Some(scene),
        diagnostics: diags,
        stage: Stage::Ok,
    }
}

/// Export `scene` as a GLB, first resolving every relative texture path on
/// the scene's materials against `source_dir` (the directory of the `.mg`
/// file the user opened). Without this, a material authored
/// `base_color_texture="oak.png"` would be resolved against the GUI process's
/// cwd and almost never found.
pub fn write_glb_with_source(
    scene: &SceneGraph,
    path: &Path,
    source_dir: Option<&Path>,
) -> anyhow::Result<()> {
    if let Some(dir) = source_dir {
        let mut resolved = scene.clone();
        resolved.resolve_texture_paths(dir);
        mgen_export::write_glb(&resolved, path)
    } else {
        mgen_export::write_glb(scene, path)
    }
}

fn has_errors(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| matches!(d.severity, Severity::Error))
}
