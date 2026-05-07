//! `mogen thumbnail` — render a PNG preview of a `.mog` file via the
//! headless GL pipeline in `mogen-render`. Output is suitable to feed back
//! into `mogen moghub publish --thumbnail`.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use mogen_dsl::module::FsLoader;
use mogen_render::headless::{save_thumbnail_png, ThumbnailOptions};

pub(crate) struct ThumbnailArgs {
    pub input: PathBuf,
    pub out: Option<PathBuf>,
    pub size: u32,
    pub yaw: Option<f32>,
    pub pitch: Option<f32>,
    pub bg: Option<String>,
}

pub(crate) fn thumbnail(args: ThumbnailArgs) -> Result<()> {
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| args.input.with_extension("png"));
    let filename = args.input.to_string_lossy().to_string();

    let src = std::fs::read_to_string(&args.input)
        .map_err(|e| anyhow!("reading {}: {e}", args.input.display()))?;

    let ast = mogen_dsl::parse(&src)?;

    let ast_diags = mogen_validate::validate_ast_with_source(&ast, args.input.parent());
    if mogen_core::has_errors(&ast_diags) {
        mogen_validate::render_human(&filename, &src, &ast_diags);
        return Err(anyhow!("refusing to render thumbnail: validation errors"));
    }

    let mut loader = FsLoader::new();
    let mut scene = mogen_dsl::lower::lower_with_loader(&ast, args.input.parent(), &mut loader)?;

    let graph_diags = mogen_validate::validate_graph(&scene);
    if mogen_core::has_errors(&graph_diags) {
        mogen_validate::render_human(&filename, &src, &graph_diags);
        return Err(anyhow!(
            "refusing to render thumbnail: post-lowering validation errors"
        ));
    }

    if let Some(base) = args.input.parent() {
        scene.resolve_texture_paths(base);
    }

    let mut opts = ThumbnailOptions {
        base_dir: args.input.parent().map(|p| p.to_path_buf()),
        ..Default::default()
    };
    opts.size = args.size;
    if let Some(y) = args.yaw {
        opts.yaw = y;
    }
    if let Some(p) = args.pitch {
        opts.pitch = p;
    }
    if let Some(hex) = args.bg.as_deref() {
        opts.bg = parse_hex_rgb(hex)?;
    }

    save_thumbnail_png(&scene, &opts, &out)?;
    eprintln!("Wrote thumbnail {} ({}×{})", out.display(), opts.size, opts.size);
    Ok(())
}

fn parse_hex_rgb(input: &str) -> Result<[u8; 3]> {
    let s = input.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "--bg expects 6-digit hex like `#2a2d33` (got `{input}`)"
        ));
    }
    let r = u8::from_str_radix(&s[0..2], 16).unwrap();
    let g = u8::from_str_radix(&s[2..4], 16).unwrap();
    let b = u8::from_str_radix(&s[4..6], 16).unwrap();
    Ok([r, g, b])
}
