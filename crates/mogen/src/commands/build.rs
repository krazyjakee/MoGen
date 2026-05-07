use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Result};
use mogen_dsl::module::FsLoader;

use crate::format::{format_duration, print_build_summary};
use crate::spinner::Spinner;

/// Output container the build command should emit. Decided from the
/// `--format` flag (when supplied) or the output path's extension.
#[derive(Debug, Clone, Copy)]
pub(crate) enum BuildFormat {
    Glb,
    Fbx,
}

impl BuildFormat {
    pub(crate) fn from_path(out: &std::path::Path) -> Self {
        match out.extension().and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("fbx") => BuildFormat::Fbx,
            _ => BuildFormat::Glb,
        }
    }
}

pub(crate) fn build(input: PathBuf, out: PathBuf, format: Option<BuildFormat>) -> Result<()> {
    let start = Instant::now();
    let label = input
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| input.to_string_lossy().into_owned());
    let filename = input.to_string_lossy().to_string();

    let mut spinner = Spinner::new(&format!("build {label}: reading"), &[]);

    let src = match fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            spinner.abandon_with_message(format!("build {label}: couldn't read file"));
            return Err(anyhow::Error::new(e).context(format!("reading {}", input.display())));
        }
    };

    spinner.set_message(format!("build {label}: parsing"));
    let ast = match mogen_dsl::parse(&src) {
        Ok(a) => a,
        Err(e) => {
            spinner.abandon_with_message(format!("build {label}: parse error"));
            return Err(e);
        }
    };

    spinner.set_message(format!("build {label}: validating DSL"));
    let diags = mogen_validate::validate_ast_with_source(&ast, input.parent());
    if mogen_core::has_errors(&diags) {
        spinner.abandon_with_message(format!("build {label}: validation failed"));
        mogen_validate::render_human(&filename, &src, &diags);
        return Err(anyhow!("refusing to build: validation errors"));
    }
    if !diags.is_empty() {
        // Warnings — render them, but keep going. Render between spinner state
        // changes so codespan output doesn't collide with the live spinner line.
        spinner.handle().pb.suspend(|| {
            mogen_validate::render_human(&filename, &src, &diags);
        });
    }

    spinner.set_message(format!("build {label}: lowering scene"));
    let mut loader = FsLoader::new();
    let mut scene = match mogen_dsl::lower::lower_with_loader(&ast, input.parent(), &mut loader) {
        Ok(s) => s,
        Err(e) => {
            spinner.abandon_with_message(format!("build {label}: lowering failed"));
            return Err(e);
        }
    };

    spinner.set_message(format!("build {label}: checking scene graph"));
    let graph_diags = mogen_validate::validate_graph(&scene);
    if mogen_core::has_errors(&graph_diags) {
        spinner.abandon_with_message(format!("build {label}: graph validation failed"));
        mogen_validate::render_human(&filename, &src, &graph_diags);
        return Err(anyhow!("refusing to build: post-lowering validation errors"));
    }
    if !graph_diags.is_empty() {
        spinner.handle().pb.suspend(|| {
            mogen_validate::render_human(&filename, &src, &graph_diags);
        });
    }

    // Texture paths in the DSL are authored relative to the source `.mog` file,
    // not the process cwd. Resolve them here so the exporter sees absolute
    // paths regardless of how mogen was invoked.
    if let Some(base) = input.parent() {
        scene.resolve_texture_paths(base);
    }

    let format = format.unwrap_or_else(|| BuildFormat::from_path(&out));
    let (label_str, write_result) = match format {
        BuildFormat::Glb => {
            spinner.set_message(format!("build {label}: writing GLB"));
            ("GLB", mogen_export::write_glb(&scene, &out))
        }
        BuildFormat::Fbx => {
            spinner.set_message(format!("build {label}: writing FBX"));
            ("FBX", mogen_export::write_fbx(&scene, &out))
        }
    };
    if let Err(e) = write_result {
        spinner.abandon_with_message(format!("build {label}: {label_str} export failed"));
        return Err(e);
    }

    let elapsed = start.elapsed();
    spinner.finish_with_message(format!(
        "build {label}: done in {}",
        format_duration(elapsed)
    ));
    print_build_summary(&out, &scene, elapsed);
    Ok(())
}
