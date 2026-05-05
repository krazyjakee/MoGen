//! `mogen publish` — POST a `.mog` file to MoGHub.
//!
//! Reads the source from `path`, defaults title/description/tags to the
//! file's `meta(...)` block (CLI flags override), and POSTs `/api/models`
//! using the token loaded from shared storage (keyring → fallback file
//! → `MOGHUB_SESSION` env).
//!
//! Sibling `.mog` files reachable through `import "..."` are bundled into
//! the same request so the published model is rebuildable on the consumer
//! side. Registry uses (`use "@user/slug[@v]"`) are external dependencies
//! — moghub records them in `mog.lock` and the consumer resolves them
//! through the registry, so they're intentionally NOT re-uploaded here.
//! Image textures are not yet bundled (the `PublishFileInput.source`
//! field is text-only).
//!
//! The slug is server-side: moghub derives it from the title and bumps
//! a numeric suffix on collision, so the client doesn't send one.

use std::io::Cursor;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{ImageBuffer, Rgba};
use mogen_moghub_client::{MoghubClient, PublishFileInput, PublishRequest};
use mogen_render::headless::{render_thumbnail, ThumbnailOptions};

use crate::commands::auth::load_session;

pub(crate) struct PublishArgs {
    pub(crate) input: PathBuf,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) visibility: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) base_url: Option<String>,
    /// Tri-state — `Some(true)` forces module, `Some(false)` forces
    /// scene, `None` auto-detects (module ⇔ no top-level `import`).
    pub(crate) module: Option<bool>,
    pub(crate) parent_version_id: Option<String>,
}

pub(crate) fn publish(args: PublishArgs) -> Result<()> {
    let token = load_session().ok_or_else(|| {
        anyhow!(
            "no MoGHub session found — run `mogen login` first \
             (or set MOGHUB_SESSION)"
        )
    })?;

    let source = std::fs::read_to_string(&args.input)
        .with_context(|| format!("reading {}", args.input.display()))?;
    let filename = args
        .input
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "scene.mog".to_string());

    let ast = mogen_dsl::parse(&source)
        .map_err(|e| anyhow!("parsing {}: {e}", args.input.display()))?;
    let meta = mogen_dsl::extract_meta(&ast).unwrap_or_default();

    let title = args.title.clone().or_else(|| meta.name.clone()).ok_or_else(|| {
        anyhow!(
            "no title — pass --title or add `meta(name = \"…\")` to {}",
            args.input.display()
        )
    })?;
    if title.trim().is_empty() {
        bail!("title is empty — pass --title or set a non-empty meta(name = …)");
    }
    let description = args
        .description
        .clone()
        .or_else(|| meta.description.clone())
        .unwrap_or_default();
    let tag_source = if args.tags.is_empty() {
        meta.tags.clone()
    } else {
        args.tags.clone()
    };
    let tags: Vec<String> = tag_source
        .iter()
        .map(|t| t.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .take(8)
        .collect();

    let publish_as_module = args
        .module
        .unwrap_or_else(|| !ast.iter().any(|n| n.kind == "import"));

    let license = args.license.clone().unwrap_or_else(|| "CC0-1.0".to_string());
    let visibility = args
        .visibility
        .clone()
        .unwrap_or_else(|| "public".to_string());

    let base_url = pick_base_url(args.base_url.as_deref());
    let client = MoghubClient::new(&base_url)?.with_token(Some(token));

    // Best-effort thumbnail render. A failed render (no display backend on a
    // truly headless server, broken GL driver, etc.) shouldn't block the
    // publish — falling back to no thumbnail keeps the model uploadable from
    // CI / SSH sessions where headless GL isn't wired up.
    let thumbnail_png_base64 = match render_publish_thumbnail(&args.input) {
        Ok(bytes) => Some(STANDARD.encode(bytes)),
        Err(e) => {
            eprintln!("warning: skipping thumbnail — {e}");
            None
        }
    };

    // Bundle every sibling `.mog` reachable from the entry's `import`
    // statements. Registry uses are skipped — moghub stores those as
    // `mog.lock` pins, not as bundled bytes.
    let entry_dir = args.input.parent().ok_or_else(|| {
        anyhow!(
            "{} has no parent directory; pass an absolute path",
            args.input.display()
        )
    })?;
    let imports = mogen_dsl::collect_local_import_files(entry_dir, &source)
        .map_err(|e| anyhow!("collecting imports for {}: {e}", args.input.display()))?;
    if imports.iter().any(|(name, _)| name == &filename) {
        bail!(
            "import filename collides with entry filename `{filename}` — \
             rename the import or the entry before publishing"
        );
    }

    let mut files = Vec::with_capacity(1 + imports.len());
    files.push(PublishFileInput {
        filename: filename.clone(),
        source,
        is_entry: true,
    });
    for (name, src) in imports {
        files.push(PublishFileInput {
            filename: name,
            source: src,
            is_entry: false,
        });
    }
    let import_count = files.len() - 1;

    let req = PublishRequest {
        title: title.clone(),
        description,
        license,
        visibility,
        publish_message: args.message.clone().unwrap_or_default(),
        tags,
        files,
        thumbnail_png_base64,
        parent_version_id: args.parent_version_id.clone(),
        publish_as_module,
    };

    if import_count > 0 {
        println!(
            "Publishing {} (+ {import_count} import{}) as “{title}” to {base_url}…",
            args.input.display(),
            if import_count == 1 { "" } else { "s" },
        );
    } else {
        println!("Publishing {} as “{title}” to {base_url}…", args.input.display());
    }
    let resp = client
        .publish(&req)
        .map_err(|e| anyhow!("publish failed: {e}"))?;
    // `PublishResponse` carries `url_path`; the version number isn't
    // surfaced separately. Browsing the URL is the cheapest way to see
    // whether this published a v1 or bumped an existing model.
    println!(
        "Published: {}{}",
        base_url.trim_end_matches('/'),
        resp.url_path,
    );
    Ok(())
}

/// Re-parse, validate, and lower the entry source, then call into the
/// headless renderer to produce a 512×512 RGBA frame and PNG-encode it.
/// Mirrors the framing the Studio uses for its `Generate Thumbnail` action
/// (and the publish dialog's preview) so a CLI-published thumbnail looks
/// the same as one captured from the GUI.
///
/// Returns the PNG bytes ready to base64-encode. Errors here are
/// non-fatal at the call site — the publish proceeds without a thumbnail.
fn render_publish_thumbnail(input: &std::path::Path) -> Result<Vec<u8>> {
    let source = std::fs::read_to_string(input)
        .with_context(|| format!("reading {} for thumbnail", input.display()))?;
    let ast = mogen_dsl::parse(&source)
        .map_err(|e| anyhow!("thumbnail: parse {}: {e}", input.display()))?;
    let diags = mogen_validate::validate_ast_with_source(&ast, input.parent());
    if mogen_core::has_errors(&diags) {
        bail!("thumbnail: source has validation errors");
    }
    let mut scene = mogen_dsl::lower_with_source(&ast, input.parent())
        .map_err(|e| anyhow!("thumbnail: lower {}: {e}", input.display()))?;
    let graph_diags = mogen_validate::validate_graph(&scene);
    if mogen_core::has_errors(&graph_diags) {
        bail!("thumbnail: scene graph has validation errors");
    }
    if let Some(base) = input.parent() {
        scene.resolve_texture_paths(base);
    }
    let opts = ThumbnailOptions {
        base_dir: input.parent().map(|p| p.to_path_buf()),
        ..Default::default()
    };
    let pixels = render_thumbnail(&scene, &opts)?;
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(opts.size, opts.size, pixels)
            .ok_or_else(|| anyhow!("thumbnail: pixel buffer mismatched output size"))?;
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .context("thumbnail: encode PNG")?;
    Ok(png)
}

fn pick_base_url(explicit: Option<&str>) -> String {
    if let Some(s) = explicit {
        return s.to_string();
    }
    if let Ok(env) = std::env::var("MOGHUB_URL") {
        let env = env.trim();
        if !env.is_empty() {
            return env.to_string();
        }
    }
    mogen_moghub_client::DEFAULT_BASE_URL.to_string()
}
