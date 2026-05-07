//! `mogen moghub publish` — CLI mirror of MoGen Studio's publish flow.
//!
//! Walks the entry `.mog`, bundles local imports + textures, optionally
//! attaches a thumbnail, parses the prior publish stamp out of
//! `meta(moghub_model_id, moghub_slug, moghub_version)` to drive
//! create-vs-update, and finally rewrites those three meta keys back
//! into the source file so the next publish targets the same model.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use mogen_dsl::module::FsLoader;
use mogen_moghub_client::{
    PublishFileInput, PublishRequest, PublishResponse, PublishTextureInput,
};
use mogen_render::headless::{render_thumbnail, ThumbnailOptions};

use super::moghub_textures::collect_publish_textures;

pub(crate) struct PublishArgs {
    pub input: PathBuf,
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Option<String>,
    pub license: Option<String>,
    pub visibility: Option<String>,
    pub message: Option<String>,
    pub thumbnail: Option<PathBuf>,
    pub filename: Option<String>,
    pub publish_as_module: Option<bool>,
    pub publish_as_new: bool,
    pub server: Option<String>,
}

pub(crate) fn publish(args: PublishArgs) -> Result<()> {
    let token = mogen_moghub_client::session_store::read_session().ok_or_else(|| {
        anyhow!("not logged in to moghub. Run 'mogen auth moghub login' first.")
    })?;
    let base_url = args
        .server
        .clone()
        .or_else(mogen_moghub_client::session_store::read_base_url)
        .unwrap_or_else(|| mogen_moghub_client::DEFAULT_BASE_URL.to_string());

    let input = args
        .input
        .canonicalize()
        .with_context(|| format!("resolving {}", args.input.display()))?;
    let source = std::fs::read_to_string(&input)
        .with_context(|| format!("reading {}", input.display()))?;
    let entry_dir = input
        .parent()
        .ok_or_else(|| anyhow!("input path has no parent directory: {}", input.display()))?
        .to_path_buf();

    // Defaults pulled from the source's `meta(...)` block — overridable
    // via flags. Mirrors `open_publish_dialog` in Studio.
    let (meta, has_imports) = match mogen_dsl::parse(&source) {
        Ok(ast) => {
            let m = mogen_dsl::extract_meta(&ast).unwrap_or_default();
            let has_imports = ast.iter().any(|n| n.kind == "import");
            (m, has_imports)
        }
        Err(_) => (Default::default(), false),
    };

    let title = args
        .title
        .clone()
        .or(meta.name.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "title is required — set meta(name=\"…\") in {} or pass --title",
                input.display()
            )
        })?;
    let description = args
        .description
        .clone()
        .or(meta.description.clone())
        .unwrap_or_default();
    let tags_input = args.tags.clone().unwrap_or_else(|| {
        meta.tags
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    });
    let tags: Vec<String> = tags_input
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .take(8)
        .collect();
    let license = args.license.clone().unwrap_or_else(|| "CC0-1.0".to_string());
    let visibility = args
        .visibility
        .clone()
        .unwrap_or_else(|| "public".to_string());
    let publish_message = args.message.clone().unwrap_or_default();
    let publish_as_module = args.publish_as_module.unwrap_or(!has_imports);

    let suggested_filename = args.filename.clone().unwrap_or_else(|| {
        input
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "scene.mog".to_string())
    });

    // Bundle imports.
    let imports = mogen_dsl::collect_local_import_files(&entry_dir, &source)
        .with_context(|| "collecting imports")?;
    for (name, _) in &imports {
        if name == &suggested_filename {
            bail!(
                "import filename collides with entry filename `{}` — \
                 rename one before publishing",
                suggested_filename
            );
        }
    }

    let mut files = vec![PublishFileInput {
        filename: suggested_filename.clone(),
        source: source.clone(),
        is_entry: true,
    }];
    for (name, src) in &imports {
        files.push(PublishFileInput {
            filename: name.clone(),
            source: src.clone(),
            is_entry: false,
        });
    }

    let textures = collect_publish_textures(&entry_dir, &source, &imports)
        .with_context(|| "bundling textures")?;
    log_bundle(&suggested_filename, &imports, &textures);

    let thumbnail_png = match &args.thumbnail {
        Some(path) => std::fs::read(path)
            .with_context(|| format!("reading thumbnail {}", path.display()))?,
        None => render_publish_thumbnail(&source, &entry_dir)
            .context("rendering publish thumbnail")?,
    };
    let thumbnail_png_base64 = STANDARD.encode(&thumbnail_png);

    // Prior publish identity — drives create-vs-update.
    let update_target = read_update_target(&source);
    let target_model_id = update_target
        .as_ref()
        .filter(|_| !args.publish_as_new)
        .map(|t| t.model_id.clone());
    let next_version = update_target
        .as_ref()
        .filter(|_| !args.publish_as_new)
        .map(|t| t.last_version + 1)
        .unwrap_or(1);

    let req = PublishRequest {
        title,
        description,
        license,
        visibility,
        publish_message,
        tags,
        files,
        textures,
        thumbnail_png_base64,
        parent_version_id: None,
        publish_as_module,
        target_model_id,
    };

    let client = mogen_moghub_client::MoghubClient::new(&base_url)
        .with_context(|| format!("constructing moghub client for {base_url}"))?
        .with_token(Some(token));

    if update_target.is_some() && !args.publish_as_new {
        eprintln!(
            "Publishing v{next_version} of existing model (model_id {})…",
            update_target.as_ref().unwrap().model_id
        );
    } else {
        eprintln!("Publishing new model to {base_url}…");
    }

    let resp = client
        .publish(&req)
        .with_context(|| format!("publishing to {base_url}"))?;
    let trimmed_base = base_url.trim_end_matches('/');
    println!("Published. {}{}", trimmed_base, resp.url_path);

    let stamped_slug = slug_from_url_path(&resp.url_path);
    stamp_publish_meta(&input, &resp, stamped_slug.as_deref(), next_version)?;

    Ok(())
}

struct UpdateTarget {
    model_id: String,
    #[allow(dead_code)]
    slug: String,
    last_version: i32,
}

fn read_update_target(source: &str) -> Option<UpdateTarget> {
    let model_id = mogen_dsl::read_meta_attr(source, "moghub_model_id")?;
    let slug = mogen_dsl::read_meta_attr(source, "moghub_slug")?;
    let last_version: i32 = mogen_dsl::read_meta_attr(source, "moghub_version")?
        .parse()
        .ok()?;
    Some(UpdateTarget {
        model_id,
        slug,
        last_version,
    })
}

fn slug_from_url_path(url_path: &str) -> Option<String> {
    let trimmed = url_path.trim_start_matches('/');
    let mut parts = trimmed.split('/');
    if parts.next()? != "m" {
        return None;
    }
    let _user = parts.next()?;
    let slug = parts.next()?;
    Some(slug.to_string())
}

fn stamp_publish_meta(
    input: &Path,
    resp: &PublishResponse,
    slug: Option<&str>,
    version: i32,
) -> Result<()> {
    let mut src = std::fs::read_to_string(input)
        .with_context(|| format!("re-reading {} for stamp", input.display()))?;
    src = mogen_dsl::upsert_meta_attr(&src, "moghub_model_id", &resp.model_id);
    if let Some(s) = slug {
        src = mogen_dsl::upsert_meta_attr(&src, "moghub_slug", s);
    }
    src = mogen_dsl::upsert_meta_attr(&src, "moghub_version", &version.to_string());
    std::fs::write(input, src)
        .with_context(|| format!("writing stamped meta to {}", input.display()))?;
    Ok(())
}

/// Parse + lower the entry source to a SceneGraph and render a PNG
/// thumbnail through the headless GL pipeline. Defaults match the
/// Studio's preview framing so a CLI publish and a Studio publish of
/// the same `.mog` ship visually identical thumbnails.
fn render_publish_thumbnail(source: &str, entry_dir: &Path) -> Result<Vec<u8>> {
    let ast = mogen_dsl::parse(source).context("parsing source for thumbnail render")?;
    let mut loader = FsLoader::new();
    let mut scene = mogen_dsl::lower::lower_with_loader(&ast, Some(entry_dir), &mut loader)
        .context("lowering source for thumbnail render")?;
    scene.resolve_texture_paths(entry_dir);

    let opts = ThumbnailOptions {
        base_dir: Some(entry_dir.to_path_buf()),
        ..ThumbnailOptions::default()
    };
    let pixels = render_thumbnail(&scene, &opts).context("headless thumbnail render")?;

    let mut png = Vec::with_capacity(pixels.len() / 4);
    image::write_buffer_with_format(
        &mut Cursor::new(&mut png),
        &pixels,
        opts.size,
        opts.size,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .context("encoding thumbnail PNG")?;
    Ok(png)
}

fn log_bundle(
    entry: &str,
    imports: &[(String, String)],
    textures: &[PublishTextureInput],
) {
    eprintln!(
        "Bundled {} (entry) + {} import{} + {} texture{}",
        entry,
        imports.len(),
        if imports.len() == 1 { "" } else { "s" },
        textures.len(),
        if textures.len() == 1 { "" } else { "s" },
    );
}
