use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Result};
use mogen_render::headless::{save_thumbnail_png, ThumbnailOptions};

use crate::format::format_duration;
use crate::spinner::Spinner;

/// Args for the `render` subcommand. Mirrors `build` for the front half of the
/// pipeline (parse → validate AST → lower → validate graph → texture-path
/// resolve) and then hands the resulting scene to `mogen-render`'s headless
/// thumbnail path instead of writing a GLB.
pub(crate) struct RenderArgs {
    pub(crate) input: PathBuf,
    pub(crate) out: Option<PathBuf>,
    pub(crate) size: u32,
    pub(crate) yaw_deg: f32,
    pub(crate) pitch_deg: f32,
    pub(crate) bg: Option<[u8; 3]>,
}

pub(crate) fn render(args: RenderArgs) -> Result<()> {
    let RenderArgs {
        input,
        out,
        size,
        yaw_deg,
        pitch_deg,
        bg,
    } = args;
    let start = Instant::now();
    let label = input
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| input.to_string_lossy().into_owned());
    let filename = input.to_string_lossy().to_string();
    let out = out.unwrap_or_else(|| input.with_extension("thumb.png"));

    let mut spinner = Spinner::new(&format!("render {label}: reading"), &[]);

    let src = match fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            spinner.abandon_with_message(format!("render {label}: couldn't read file"));
            return Err(anyhow::Error::new(e).context(format!("reading {}", input.display())));
        }
    };

    spinner.set_message(format!("render {label}: parsing"));
    let ast = match mogen_dsl::parse(&src) {
        Ok(a) => a,
        Err(e) => {
            spinner.abandon_with_message(format!("render {label}: parse error"));
            return Err(e);
        }
    };

    spinner.set_message(format!("render {label}: validating DSL"));
    let diags = mogen_validate::validate_ast(&ast);
    if mogen_core::has_errors(&diags) {
        spinner.abandon_with_message(format!("render {label}: validation failed"));
        mogen_validate::render_human(&filename, &src, &diags);
        return Err(anyhow!("refusing to render: validation errors"));
    }
    if !diags.is_empty() {
        spinner.handle().pb.suspend(|| {
            mogen_validate::render_human(&filename, &src, &diags);
        });
    }

    spinner.set_message(format!("render {label}: lowering scene"));
    let mut scene = match mogen_dsl::lower_with_source(&ast, input.parent()) {
        Ok(s) => s,
        Err(e) => {
            spinner.abandon_with_message(format!("render {label}: lowering failed"));
            return Err(e);
        }
    };

    spinner.set_message(format!("render {label}: checking scene graph"));
    let graph_diags = mogen_validate::validate_graph(&scene);
    if mogen_core::has_errors(&graph_diags) {
        spinner.abandon_with_message(format!("render {label}: graph validation failed"));
        mogen_validate::render_human(&filename, &src, &graph_diags);
        return Err(anyhow!("refusing to render: post-lowering validation errors"));
    }
    if !graph_diags.is_empty() {
        spinner.handle().pb.suspend(|| {
            mogen_validate::render_human(&filename, &src, &graph_diags);
        });
    }

    if let Some(base) = input.parent() {
        scene.resolve_texture_paths(base);
    }

    let opts = ThumbnailOptions {
        size,
        yaw: yaw_deg.to_radians(),
        pitch: pitch_deg.to_radians(),
        bg: bg.unwrap_or(ThumbnailOptions::default().bg),
        base_dir: input.parent().map(|p| p.to_path_buf()),
    };

    spinner.set_message(format!("render {label}: rasterising {size}px"));
    if let Err(e) = save_thumbnail_png(&scene, &opts, &out) {
        spinner.abandon_with_message(format!("render {label}: render failed"));
        return Err(e);
    }

    let elapsed = start.elapsed();
    spinner.finish_with_message(format!(
        "render {label}: wrote {} in {}",
        out.display(),
        format_duration(elapsed)
    ));
    Ok(())
}

/// Parse a `--bg` arg in `R,G,B` form (each in 0..=255). Used by the CLI front
/// to convert the user's flag into the `[u8; 3]` the thumbnail options take.
pub(crate) fn parse_bg(s: &str) -> Result<[u8; 3], String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 3 {
        return Err(format!("expected R,G,B (got {s:?})"));
    }
    let mut out = [0u8; 3];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p
            .trim()
            .parse::<u8>()
            .map_err(|e| format!("channel {i}: {e}"))?;
    }
    Ok(out)
}
