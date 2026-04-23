//! `mgen textures` — walk a `.mg` AST, generate albedo PNGs for every
//! material via Gemini 2.5 Flash Image, derive the companion PBR maps
//! (normal / metallic-roughness / occlusion) locally via [`crate::pbr_maps`],
//! and splice the resulting `*_texture="…"` attrs into the source file.
//!
//! Splicing is a pure text edit driven by the parser's byte spans so we don't
//! lose formatting or comments. Prompt assembly and file naming live here too
//! because they're only useful in this command.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::imageops::FilterType as ResampleFilter;
use image::{ExtendedColorType, ImageEncoder, ImageFormat};
use mgen_core::Span;
use mgen_dsl::ast::{Node, Value};

use crate::gemini::{GeminiClient, GeminiError};
use crate::image::GeneratedImage;
use crate::image_cache::{default_image_cache_dir, ImageCache};
use crate::pbr_maps::{derive_pbr_maps, PbrMapOptions};
#[cfg(test)]
use crate::image::DEFAULT_IMAGE_MODEL;

/// Default cap on the longer side of every LLM-generated albedo, in pixels.
/// 512² is a 4× reduction from what Gemini 2.5 Flash Image returns (~1024²)
/// and keeps tileable PBR detail usable at normal camera distance; derived
/// normal / MR / AO maps inherit the size, so one downscale cascades.
pub const DEFAULT_TEXTURE_SIZE: u32 = 512;

/// How many times to retry an `IMAGE_RECITATION`-rejected prompt with a small
/// stylistic variation before giving up. Each retry sends a fresh request and
/// burns API quota, so keep this modest.
const RECITATION_RETRIES: u32 = 3;

/// A single material the AST reports — name + full node span + attrs.
/// We keep a reference to the [`Node`] so callers can inspect existing attrs
/// (e.g. to skip materials that already declare a texture).
pub struct MaterialHit<'a> {
    pub node: &'a Node,
    pub name: String,
}

/// Extract every top-level `material` node from the parsed AST.
pub fn collect_materials<'a>(ast: &'a [Node]) -> Vec<MaterialHit<'a>> {
    ast.iter()
        .filter(|n| n.kind == "material")
        .filter_map(|n| {
            n.name.as_ref().map(|name| MaterialHit {
                node: n,
                name: name.clone(),
            })
        })
        .collect()
}

/// Build the image prompt for one material. Includes:
///   - material name (strongest signal — "oak" vs "denim" drives the output),
///   - authored color as an RGB hex hint (preserves artist intent),
///   - a rough/polished word from `roughness`,
///   - an optional subject hint parsed from the DSL's `// prompt:` header.
pub fn build_prompt(hit: &MaterialHit<'_>, style: &str, subject: Option<&str>) -> String {
    let color = hit.node.attr("color").and_then(|v| match v {
        Value::Vec3([r, g, b]) => Some([*r, *g, *b]),
        _ => None,
    });
    let roughness = hit.node.attr("roughness").and_then(|v| match v {
        Value::Number(n) => Some(*n),
        _ => None,
    });

    let mut s = String::new();
    s.push_str(
        "Seamless tileable PBR base-color (albedo) texture. \
         Flat overhead lighting, no directional shadows, no baked-in ambient occlusion, \
         no highlights. The image must tile perfectly when placed edge-to-edge. \
         Output a square image.\n\n",
    );
    s.push_str(&format!("Material name: {}\n", hit.name));
    if let Some([r, g, b]) = color {
        s.push_str(&format!(
            "Target color (approximate, hex): {}\n",
            rgb_to_hex(r, g, b)
        ));
    }
    if let Some(r) = roughness {
        s.push_str(&format!("Surface finish: {}\n", roughness_word(r)));
    }
    s.push_str(&format!("Style: {style}\n"));
    if let Some(ctx) = subject {
        let trimmed = ctx.trim();
        if !trimmed.is_empty() {
            s.push_str(&format!("Subject context (for mood/era only): {trimmed}\n"));
        }
    }
    s
}

fn rgb_to_hex(r: f32, g: f32, b: f32) -> String {
    let c = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", c(r), c(g), c(b))
}

fn roughness_word(r: f32) -> &'static str {
    if r >= 0.85 {
        "very rough, fully matte"
    } else if r >= 0.6 {
        "rough, matte"
    } else if r >= 0.35 {
        "semi-gloss"
    } else if r >= 0.15 {
        "smooth, glossy"
    } else {
        "polished, near-mirror"
    }
}

/// Parse the first `// prompt: …` line produced by `embed_seed_header`, if
/// present. Used as subject-context enrichment for per-material prompts.
pub fn parse_prompt_header(src: &str) -> Option<String> {
    for line in src.lines().take(8) {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("// prompt:") {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Splice edit: add `attr="rel_path"` into the attr list whose outer node
/// covers `span`, unless the attr is already present. Produced by
/// [`run_plan`] and consumed by [`splice_textures`].
#[derive(Debug, Clone)]
pub struct Edit {
    pub span: Span,
    pub attr: &'static str,
    pub rel_path: String,
}

/// Apply a batch of [`Edit`]s to `src`. Edits that touch the same material
/// node are merged into a single rewrite, and any attr already present in
/// that node is left untouched.
pub fn splice_textures(src: &str, edits: &[Edit]) -> Result<String> {
    // Group edits by material span so we rewrite each node at most once.
    let mut by_span: HashMap<(usize, usize), Vec<&Edit>> = HashMap::new();
    for e in edits {
        by_span
            .entry((e.span.start, e.span.end))
            .or_default()
            .push(e);
    }

    // Apply in reverse span order so earlier byte offsets aren't invalidated.
    let mut keys: Vec<(usize, usize)> = by_span.keys().copied().collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.0));

    let mut out = src.to_string();
    for k in keys {
        let group = by_span.remove(&k).unwrap();
        let span = Span { start: k.0, end: k.1 };
        out = splice_many(&out, span, &group)?;
    }
    Ok(out)
}

/// Rewrite one material node, inserting every requested attr that isn't
/// already declared. If the node has no attr list (`material "x"` with no
/// parens), one is added.
fn splice_many(src: &str, span: Span, edits: &[&Edit]) -> Result<String> {
    if span.end > src.len() || span.start > span.end {
        bail!("bad span {:?} for source of len {}", span, src.len());
    }
    let slice = &src[span.start..span.end];

    let open = slice.find('(');
    let close = slice.rfind(')');

    let mut out = String::with_capacity(src.len() + 128);
    out.push_str(&src[..span.start]);

    match (open, close) {
        (Some(o), Some(c)) if c > o => {
            let body = &slice[o + 1..c];
            let new_attrs: Vec<String> = edits
                .iter()
                .filter(|e| !attr_already_present(body, e.attr))
                .map(|e| format!(r#"{}="{}""#, e.attr, e.rel_path))
                .collect();

            out.push_str(&slice[..=o]);
            if body.trim().is_empty() {
                out.push_str(&new_attrs.join(", "));
            } else {
                out.push_str(body);
                if !new_attrs.is_empty() {
                    let trimmed = body.trim_end();
                    if !trimmed.ends_with(',') {
                        out.push_str(", ");
                    }
                    out.push_str(&new_attrs.join(", "));
                }
            }
            out.push_str(&slice[c..]);
        }
        _ => {
            let new_attrs: Vec<String> = edits
                .iter()
                .map(|e| format!(r#"{}="{}""#, e.attr, e.rel_path))
                .collect();
            out.push_str(slice);
            out.push_str(&format!(" ({})", new_attrs.join(", ")));
        }
    }

    out.push_str(&src[span.end..]);
    Ok(out)
}

/// Rough check whether `attr=` already appears in the parenthesised body of
/// a material declaration. Looks for the attr name followed (after whitespace)
/// by `=`. Fine for our well-formed AST inputs — we never call this on
/// arbitrary text.
fn attr_already_present(body: &str, attr: &str) -> bool {
    let needle = attr;
    let mut idx = 0;
    while let Some(pos) = body[idx..].find(needle) {
        let abs = idx + pos;
        let before_ok = abs == 0
            || matches!(
                body.as_bytes()[abs - 1],
                b',' | b'(' | b' ' | b'\t' | b'\n'
            );
        let after = &body[abs + needle.len()..];
        let after_ok = after.trim_start().starts_with('=');
        if before_ok && after_ok {
            return true;
        }
        idx = abs + needle.len();
    }
    false
}

/// Convert a material name into a filesystem-safe stem.
pub fn safe_filename_stem(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if c == '_' || c == '-' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('_');
        }
        // Drop everything else.
    }
    if out.is_empty() {
        "material".to_string()
    } else {
        out
    }
}

// --- plan / execution ------------------------------------------------------

/// Which texture slot a [`Plan`] targets. The attr name and filename suffix
/// are derived from this — callers never spell them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Albedo,
    Normal,
    MetallicRoughness,
    Occlusion,
}

impl SlotKind {
    pub fn attr(self) -> &'static str {
        match self {
            SlotKind::Albedo => "base_color_texture",
            SlotKind::Normal => "normal_texture",
            SlotKind::MetallicRoughness => "metallic_roughness_texture",
            SlotKind::Occlusion => "occlusion_texture",
        }
    }
    pub fn suffix(self) -> &'static str {
        match self {
            SlotKind::Albedo => "_albedo.png",
            SlotKind::Normal => "_normal.png",
            SlotKind::MetallicRoughness => "_metallicRoughness.png",
            SlotKind::Occlusion => "_ao.png",
        }
    }
    pub fn short_name(self) -> &'static str {
        match self {
            SlotKind::Albedo => "albedo",
            SlotKind::Normal => "normal",
            SlotKind::MetallicRoughness => "metalRough",
            SlotKind::Occlusion => "ao",
        }
    }
}

pub struct TexturesArgs {
    pub input: PathBuf,
    pub out: Option<PathBuf>,
    pub glb: Option<PathBuf>,
    pub textures_dir: PathBuf, // relative to `.mg`
    pub style: String,
    pub model: String,
    pub force: bool,
    pub dry_run: bool,
    pub no_build: bool,
    pub no_cache: bool,
    pub api_key: Option<String>,
    /// Disable every derived PBR map. Albedo still gets generated.
    pub no_pbr: bool,
    pub no_normal: bool,
    pub no_metallic_roughness: bool,
    pub no_occlusion: bool,
    pub normal_strength: f32,
    /// Cap on the longer side of generated albedos, in pixels. `0` disables
    /// the downscale and keeps whatever Gemini returned. Derived maps (normal
    /// / metallic-roughness / AO) inherit this size.
    pub texture_size: u32,
}

impl TexturesArgs {
    #[cfg(test)]
    pub fn with_defaults(input: PathBuf) -> Self {
        Self {
            input,
            out: None,
            glb: None,
            textures_dir: PathBuf::from("textures"),
            style: "photorealistic".to_string(),
            model: DEFAULT_IMAGE_MODEL.to_string(),
            force: false,
            dry_run: false,
            no_build: false,
            no_cache: false,
            api_key: None,
            no_pbr: false,
            no_normal: false,
            no_metallic_roughness: false,
            no_occlusion: false,
            normal_strength: PbrMapOptions::default().normal_strength,
            texture_size: DEFAULT_TEXTURE_SIZE,
        }
    }
}

/// One-line plan entry printed during dry-run and real runs alike. A single
/// material produces up to four plans (one per slot) so the reported table
/// mirrors exactly what run_plan is going to do.
pub struct Plan {
    pub material: String,
    pub span: Span,
    pub kind: SlotKind,
    pub action: PlanAction,
    pub rel_path: PathBuf,
    /// Non-empty only for `SlotKind::Albedo` — image prompt for the LLM call.
    pub prompt: String,
    /// For albedo, the relative path declared in the `.mg` when the slot was
    /// already textured. Lets [`run_plan`] read those bytes from disk so
    /// derived maps can still be produced without re-running the LLM.
    pub existing_albedo_path: Option<PathBuf>,
}

pub enum PlanAction {
    /// Call the LLM (albedo only).
    Generate,
    /// Load a cached LLM PNG (albedo only).
    CacheHit,
    /// Derive locally from the albedo PNG (PBR maps only).
    Derive,
    /// Do nothing — either the attr is already present, or the user disabled
    /// this map kind via a flag.
    Skip(&'static str),
}

fn pbr_opts(args: &TexturesArgs, material: &Node) -> PbrMapOptions {
    let roughness = material.attr("roughness").and_then(|v| match v {
        Value::Number(n) => Some(*n),
        _ => None,
    });
    let metallic = material.attr("metallic").and_then(|v| match v {
        Value::Number(n) => Some(*n),
        _ => None,
    });
    let mut opts = PbrMapOptions::default();
    if let Some(r) = roughness {
        opts.roughness_base = r.clamp(0.0, 1.0);
    }
    if let Some(m) = metallic {
        opts.metallic = m.clamp(0.0, 1.0);
    }
    if args.normal_strength.is_finite() && args.normal_strength > 0.0 {
        opts.normal_strength = args.normal_strength;
    }
    opts
}

/// Build the plan without calling the API. Used by `--dry-run` and exposed
/// for testing.
pub fn build_plan(
    src: &str,
    ast: &[Node],
    args: &TexturesArgs,
    cache: Option<&ImageCache>,
) -> Vec<Plan> {
    let subject = parse_prompt_header(src);
    let hits = collect_materials(ast);
    let mut plans = Vec::new();

    for h in hits {
        let stem = safe_filename_stem(&h.name);

        // --- albedo slot ---
        let albedo_path = args.textures_dir.join(format!("{stem}{}", SlotKind::Albedo.suffix()));
        let existing_albedo = attr_path(h.node, SlotKind::Albedo.attr());
        let (albedo_action, albedo_prompt) = if existing_albedo.is_some() && !args.force {
            (PlanAction::Skip("already has base_color_texture"), String::new())
        } else {
            let prompt = build_prompt(&h, &args.style, subject.as_deref());
            let cached = cache
                .map(|c| c.lookup(&ImageCache::key(&args.model, &prompt)).is_some())
                .unwrap_or(false);
            let action = if cached {
                PlanAction::CacheHit
            } else {
                PlanAction::Generate
            };
            (action, prompt)
        };
        plans.push(Plan {
            material: h.name.clone(),
            span: h.node.span,
            kind: SlotKind::Albedo,
            action: albedo_action,
            rel_path: albedo_path,
            prompt: albedo_prompt,
            existing_albedo_path: existing_albedo.clone(),
        });

        // --- derived maps ---
        for (kind, disabled) in [
            (SlotKind::Normal, args.no_normal),
            (SlotKind::MetallicRoughness, args.no_metallic_roughness),
            (SlotKind::Occlusion, args.no_occlusion),
        ] {
            if args.no_pbr || disabled {
                continue;
            }
            let rel_path = args.textures_dir.join(format!("{stem}{}", kind.suffix()));
            let action = if attr_path(h.node, kind.attr()).is_some() && !args.force {
                PlanAction::Skip("already present")
            } else {
                PlanAction::Derive
            };
            plans.push(Plan {
                material: h.name.clone(),
                span: h.node.span,
                kind,
                action,
                rel_path,
                prompt: String::new(),
                existing_albedo_path: None,
            });
        }
    }
    plans
}

fn attr_path(node: &Node, key: &str) -> Option<PathBuf> {
    match node.attr(key)? {
        Value::String(s) | Value::Ident(s) => Some(PathBuf::from(s)),
        _ => None,
    }
}

/// Execute the plan: generate/cache-hit each PNG, write it into the `.mg`
/// directory's texture folder, and return the [`Edit`]s the splicer should
/// apply.
pub fn run_plan(
    client: Option<&GeminiClient>,
    model: &str,
    args: &TexturesArgs,
    ast: &[Node],
    plans: &[Plan],
    base_dir: &Path,
    cache: Option<&ImageCache>,
) -> Result<Vec<Edit>> {
    // Group plans by (span, material) so we process each material's slots
    // together and can reuse the albedo bytes for its derived maps.
    let mut by_material: Vec<(String, Span, Vec<&Plan>)> = Vec::new();
    for p in plans {
        let key = (p.material.clone(), (p.span.start, p.span.end));
        match by_material.iter_mut().find(|(m, s, _)| m == &key.0 && (s.start, s.end) == key.1) {
            Some((_, _, v)) => v.push(p),
            None => by_material.push((p.material.clone(), p.span, vec![p])),
        }
    }

    // Lookup by material name for per-material PBR options (roughness/metallic
    // live on the AST, not the Plan).
    let material_nodes: HashMap<String, &Node> = ast
        .iter()
        .filter(|n| n.kind == "material")
        .filter_map(|n| n.name.as_ref().map(|nm| (nm.clone(), n)))
        .collect();

    let mut edits = Vec::new();

    for (mat_name, _span, mat_plans) in by_material {
        // Process albedo first so its bytes are ready for any Derive plans.
        let albedo_plan = mat_plans.iter().find(|p| p.kind == SlotKind::Albedo).copied();
        let mut albedo_bytes: Option<Vec<u8>> = None;

        if let Some(p) = albedo_plan {
            match &p.action {
                PlanAction::Generate | PlanAction::CacheHit => {
                    let bytes = resolve_albedo_bytes(client, model, p, cache, args.texture_size)?;
                    let abs = base_dir.join(&p.rel_path);
                    write_png(&abs, &bytes)?;
                    edits.push(Edit {
                        span: p.span,
                        attr: p.kind.attr(),
                        rel_path: to_forward_slashes(&p.rel_path),
                    });
                    albedo_bytes = Some(bytes);
                }
                PlanAction::Skip(_) => {
                    // Attempt to load an existing albedo from disk so we can
                    // still derive PBR maps for this material without
                    // re-running the LLM. Failure here is non-fatal.
                    if let Some(existing) = &p.existing_albedo_path {
                        let abs = base_dir.join(existing);
                        if let Ok(bytes) = fs::read(&abs) {
                            albedo_bytes = Some(bytes);
                        }
                    }
                }
                PlanAction::Derive => {
                    bail!("albedo plan should not carry Derive action");
                }
            }
        }

        // Derived maps — one call to `derive_pbr_maps` per material with the
        // union of requested slots. Saves repeated luminance/blur work.
        let wants_normal = mat_plans
            .iter()
            .any(|p| p.kind == SlotKind::Normal && matches!(p.action, PlanAction::Derive));
        let wants_mr = mat_plans
            .iter()
            .any(|p| p.kind == SlotKind::MetallicRoughness && matches!(p.action, PlanAction::Derive));
        let wants_ao = mat_plans
            .iter()
            .any(|p| p.kind == SlotKind::Occlusion && matches!(p.action, PlanAction::Derive));

        if wants_normal || wants_mr || wants_ao {
            let Some(bytes) = albedo_bytes else {
                // No albedo to derive from — e.g. albedo skipped and no
                // existing file on disk. Skip derived maps silently rather
                // than erroring the whole run.
                continue;
            };
            let node = material_nodes
                .get(&mat_name)
                .ok_or_else(|| anyhow!("internal: no AST node for material {mat_name}"))?;
            let mut opts = pbr_opts(args, node);
            opts.generate_normal = wants_normal;
            opts.generate_metallic_roughness = wants_mr;
            opts.generate_occlusion = wants_ao;

            let maps = derive_pbr_maps(&bytes, &opts)
                .with_context(|| format!("deriving PBR maps for material {mat_name}"))?;

            for p in &mat_plans {
                if !matches!(p.action, PlanAction::Derive) {
                    continue;
                }
                let png = match p.kind {
                    SlotKind::Normal => maps.normal_png.as_ref(),
                    SlotKind::MetallicRoughness => maps.metallic_roughness_png.as_ref(),
                    SlotKind::Occlusion => maps.occlusion_png.as_ref(),
                    SlotKind::Albedo => continue,
                };
                if let Some(bytes) = png {
                    let abs = base_dir.join(&p.rel_path);
                    write_png(&abs, bytes)?;
                    edits.push(Edit {
                        span: p.span,
                        attr: p.kind.attr(),
                        rel_path: to_forward_slashes(&p.rel_path),
                    });
                }
            }
        }
    }
    Ok(edits)
}

fn resolve_albedo_bytes(
    client: Option<&GeminiClient>,
    model: &str,
    plan: &Plan,
    cache: Option<&ImageCache>,
    max_side: u32,
) -> Result<Vec<u8>> {
    let key = ImageCache::key(model, &plan.prompt);
    if let Some(c) = cache {
        if let Some(cached_path) = c.lookup(&key) {
            let raw = fs::read(&cached_path)
                .with_context(|| format!("reading cached image {}", cached_path.display()))?;
            return resize_and_recompress_albedo(&raw, max_side);
        }
    }
    let client = client.ok_or_else(|| {
        anyhow!(
            "no GeminiClient available and no cache hit for material {}",
            plan.material
        )
    })?;
    let img = generate_with_recitation_retry(client, model, &plan.prompt, RECITATION_RETRIES)
        .map_err(|e: GeminiError| anyhow!("gemini image: {e}"))?;
    if let Some(c) = cache {
        // Cache the *original* model output under the base prompt key so that
        // changing `--texture-size` on a future run re-resizes from the full
        // resolution instead of a pre-shrunk copy.
        let _ = c.store(&key, &img.png_bytes);
    }
    resize_and_recompress_albedo(&img.png_bytes, max_side)
}

/// Downscale the albedo so its longer side is at most `max_side` and
/// re-encode with the PNG `Best` compression preset. Returns the original
/// bytes when `max_side == 0` or the image is already within the cap — no
/// point re-encoding if we have nothing to change.
fn resize_and_recompress_albedo(png: &[u8], max_side: u32) -> Result<Vec<u8>> {
    if max_side == 0 {
        return Ok(png.to_vec());
    }
    let img = image::load_from_memory_with_format(png, ImageFormat::Png)
        .context("decoding albedo PNG for resize")?;
    let (w, h) = (img.width(), img.height());
    if w <= max_side && h <= max_side {
        return Ok(png.to_vec());
    }
    // Lanczos3 is the slowest-but-sharpest resampler in the image crate.
    // PBR textures downscale once at generation time and are then read many
    // times, so quality wins over speed here.
    let resized = img
        .resize(max_side, max_side, ResampleFilter::Lanczos3)
        .to_rgb8();
    let mut buf = Vec::new();
    PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive)
        .write_image(
            resized.as_raw(),
            resized.width(),
            resized.height(),
            ExtendedColorType::Rgb8,
        )
        .context("encoding resized albedo PNG")?;
    Ok(buf)
}

fn write_png(abs: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(abs, bytes).with_context(|| format!("writing {}", abs.display()))?;
    Ok(())
}

fn to_forward_slashes(rel: &Path) -> String {
    rel.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// Call `generate_image`, retrying on `IMAGE_RECITATION` with a short
/// stylistic-variation suffix. Recitation rejections are sticky for the exact
/// same prompt — re-issuing as-is just burns quota — so each retry appends a
/// distinct hint that nudges the model toward a different sample without
/// changing what the texture is supposed to depict.
pub fn generate_with_recitation_retry(
    client: &GeminiClient,
    model: &str,
    base_prompt: &str,
    max_retries: u32,
) -> Result<GeneratedImage, GeminiError> {
    let mut attempt: u32 = 0;
    loop {
        let prompt = if attempt == 0 {
            base_prompt.to_string()
        } else {
            let hint = recitation_variation_hint(attempt);
            format!("{base_prompt}\nVariation hint: {hint}")
        };
        match client.generate_image(model, &prompt) {
            Ok(img) => return Ok(img),
            Err(GeminiError::InvalidResponse(msg))
                if msg.contains("IMAGE_RECITATION") && attempt < max_retries =>
            {
                attempt += 1;
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

fn recitation_variation_hint(attempt: u32) -> &'static str {
    // Keep the hints short, neutral, and texture-relevant so they don't
    // override the material's own descriptors.
    const HINTS: &[&str] = &[
        "use a slightly different grain pattern and unique micro-detail layout",
        "vary the surface micro-structure and shift colour balance subtly",
        "change the dominant texture frequency and re-arrange surface features",
        "introduce uncommon imperfections and a fresh distribution of detail",
    ];
    HINTS[(attempt as usize - 1) % HINTS.len()]
}

/// Resolve the cache (if enabled) without panicking — mirrors the pattern
/// used by the text-side system-instruction cache.
pub fn maybe_cache(no_cache: bool) -> Option<ImageCache> {
    if no_cache {
        return None;
    }
    default_image_cache_dir().map(ImageCache::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mgen_dsl::parse;

    fn parse_or_panic(src: &str) -> Vec<Node> {
        parse(src).expect("parse")
    }

    #[test]
    fn collects_top_level_materials_only() {
        let src = r#"material "wood" (color=[0.5, 0.3, 0.1])
material "fabric" (color=[0.2, 0.3, 0.5])
scene { box "b" (size=[1,1,1]) }"#;
        let ast = parse_or_panic(src);
        let mats = collect_materials(&ast);
        assert_eq!(mats.len(), 2);
        assert_eq!(mats[0].name, "wood");
        assert_eq!(mats[1].name, "fabric");
    }

    fn e(span: Span, attr: &'static str, rel: &str) -> Edit {
        Edit { span, attr, rel_path: rel.to_string() }
    }

    #[test]
    fn splice_inserts_before_closing_paren() {
        let src = r#"material "wood" (color=[0.5, 0.3, 0.1], roughness=0.8)"#;
        let ast = parse_or_panic(src);
        let node = &ast[0];
        let out = splice_textures(
            src,
            &[e(node.span, "base_color_texture", "textures/wood_albedo.png")],
        )
        .unwrap();
        assert!(out.contains(r#", base_color_texture="textures/wood_albedo.png")"#));
        assert!(out.contains("color=[0.5, 0.3, 0.1]"));
        assert!(out.contains("roughness=0.8"));
    }

    #[test]
    fn splice_handles_empty_attr_list() {
        let src = r#"material "x" ()"#;
        let ast = parse_or_panic(src);
        let node = &ast[0];
        let out = splice_textures(
            src,
            &[e(node.span, "base_color_texture", "textures/x_albedo.png")],
        )
        .unwrap();
        assert_eq!(
            out,
            r#"material "x" (base_color_texture="textures/x_albedo.png")"#
        );
    }

    #[test]
    fn splice_preserves_trailing_comma() {
        let src = r#"material "x" (color=[1,0,0],)"#;
        let ast = parse_or_panic(src);
        let node = &ast[0];
        let out =
            splice_textures(src, &[e(node.span, "base_color_texture", "t.png")]).unwrap();
        assert!(out.contains(r#"color=[1,0,0],base_color_texture="t.png""#));
    }

    #[test]
    fn splice_many_attrs_on_same_material() {
        let src = r#"material "wood" (color=[1,0,0])"#;
        let ast = parse_or_panic(src);
        let node = &ast[0];
        let out = splice_textures(
            src,
            &[
                e(node.span, "base_color_texture", "t/wood_albedo.png"),
                e(node.span, "normal_texture", "t/wood_normal.png"),
                e(node.span, "metallic_roughness_texture", "t/wood_mr.png"),
                e(node.span, "occlusion_texture", "t/wood_ao.png"),
            ],
        )
        .unwrap();
        assert!(out.contains(r#"base_color_texture="t/wood_albedo.png""#));
        assert!(out.contains(r#"normal_texture="t/wood_normal.png""#));
        assert!(out.contains(r#"metallic_roughness_texture="t/wood_mr.png""#));
        assert!(out.contains(r#"occlusion_texture="t/wood_ao.png""#));
        // Original attr intact.
        assert!(out.contains("color=[1,0,0]"));
    }

    #[test]
    fn splice_skips_attrs_already_present() {
        let src = r#"material "wood" (color=[1,0,0], normal_texture="old.png")"#;
        let ast = parse_or_panic(src);
        let node = &ast[0];
        let out = splice_textures(
            src,
            &[
                e(node.span, "base_color_texture", "t/a.png"),
                // This one duplicates an existing attr; must not be inserted.
                e(node.span, "normal_texture", "t/n.png"),
            ],
        )
        .unwrap();
        assert!(out.contains(r#"base_color_texture="t/a.png""#));
        // Only one normal_texture definition — the original one.
        assert_eq!(out.matches("normal_texture=").count(), 1);
        assert!(out.contains(r#"normal_texture="old.png""#));
    }

    #[test]
    fn splice_many_in_reverse_order() {
        let src = "material \"a\" (color=[1,0,0])\nmaterial \"b\" (color=[0,1,0])\n";
        let ast = parse_or_panic(src);
        let out = splice_textures(
            &src,
            &[
                e(ast[0].span, "base_color_texture", "a.png"),
                e(ast[1].span, "base_color_texture", "b.png"),
            ],
        )
        .unwrap();
        assert!(out.contains(r#", base_color_texture="a.png""#));
        assert!(out.contains(r#", base_color_texture="b.png""#));
        let a = out.find("\"a\"").unwrap();
        let b = out.find("\"b\"").unwrap();
        assert!(a < b);
    }

    #[test]
    fn safe_filename_stem_sanitizes() {
        assert_eq!(safe_filename_stem("oak wood"), "oak_wood");
        assert_eq!(safe_filename_stem("Rust/Iron"), "rustiron");
        assert_eq!(safe_filename_stem("my-mat_01"), "my-mat_01");
        assert_eq!(safe_filename_stem(""), "material");
    }

    #[test]
    fn build_prompt_includes_name_color_and_roughness() {
        let src = r#"material "oak" (color=[0.55, 0.35, 0.18], roughness=0.75)"#;
        let ast = parse_or_panic(src);
        let hits = collect_materials(&ast);
        let p = build_prompt(&hits[0], "photorealistic", None);
        assert!(p.contains("Material name: oak"));
        assert!(p.contains("#8C592E"));
        assert!(p.contains("rough, matte"));
        assert!(p.contains("photorealistic"));
    }

    #[test]
    fn build_plan_produces_slot_per_material_by_default() {
        let src = r#"material "a" (color=[1,0,0])"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(PathBuf::from("x.mg"));
        let plans = build_plan(src, &ast, &args, None);
        assert_eq!(plans.len(), 4);
        assert_eq!(plans[0].kind, SlotKind::Albedo);
        assert!(matches!(plans[0].action, PlanAction::Generate));
        assert!(matches!(plans[1].action, PlanAction::Derive));
        assert!(matches!(plans[2].action, PlanAction::Derive));
        assert!(matches!(plans[3].action, PlanAction::Derive));
    }

    #[test]
    fn build_plan_no_pbr_yields_only_albedo() {
        let src = r#"material "a" (color=[1,0,0])"#;
        let ast = parse_or_panic(src);
        let mut args = TexturesArgs::with_defaults(PathBuf::from("x.mg"));
        args.no_pbr = true;
        let plans = build_plan(src, &ast, &args, None);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].kind, SlotKind::Albedo);
    }

    #[test]
    fn build_plan_skips_already_textured_slots() {
        let src = r#"material "a" (color=[1,0,0], base_color_texture="existing.png")"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(PathBuf::from("x.mg"));
        let plans = build_plan(src, &ast, &args, None);
        // Albedo skipped but captures existing path for derivation.
        let albedo = &plans[0];
        assert!(matches!(albedo.action, PlanAction::Skip(_)));
        assert_eq!(
            albedo.existing_albedo_path.as_deref(),
            Some(std::path::Path::new("existing.png"))
        );
        // Derived slots are still scheduled.
        assert!(matches!(plans[1].action, PlanAction::Derive));
    }

    #[test]
    fn build_plan_force_retextures_existing() {
        let src = r#"material "a" (base_color_texture="old.png")"#;
        let ast = parse_or_panic(src);
        let mut args = TexturesArgs::with_defaults(PathBuf::from("x.mg"));
        args.force = true;
        let plans = build_plan(src, &ast, &args, None);
        assert!(matches!(plans[0].action, PlanAction::Generate));
    }

    #[test]
    fn parse_prompt_header_reads_first_8_lines() {
        let src = "// mgen-generate seed=1\n// prompt: a wooden stool\nmaterial \"a\" ()\n";
        assert_eq!(parse_prompt_header(src).as_deref(), Some("a wooden stool"));
    }

    #[test]
    fn parse_prompt_header_absent() {
        assert!(parse_prompt_header("material \"a\" ()").is_none());
    }

    fn synth_albedo_png(w: u32, h: u32) -> Vec<u8> {
        // Non-trivial gradient so Lanczos3 has something to resample and the
        // output size is representative of real albedo content.
        let mut img = image::RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let r = ((x * 255) / w.max(1)) as u8;
                let g = ((y * 255) / h.max(1)) as u8;
                let b = (((x ^ y) * 255) / w.max(h).max(1)) as u8;
                img.put_pixel(x, y, image::Rgb([r, g, b]));
            }
        }
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn png_dims(bytes: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).unwrap();
        (img.width(), img.height())
    }

    #[test]
    fn resize_noop_when_disabled() {
        let png = synth_albedo_png(64, 64);
        let out = resize_and_recompress_albedo(&png, 0).unwrap();
        assert_eq!(out, png);
    }

    #[test]
    fn resize_noop_when_already_small_enough() {
        let png = synth_albedo_png(64, 64);
        let out = resize_and_recompress_albedo(&png, 128).unwrap();
        assert_eq!(out, png);
    }

    #[test]
    fn resize_downscales_and_shrinks_bytes() {
        let png = synth_albedo_png(256, 256);
        let out = resize_and_recompress_albedo(&png, 64).unwrap();
        assert_eq!(png_dims(&out), (64, 64));
        // Best-compressed 64² should be dramatically smaller than the 256²
        // input — if this ever flips, we've lost the compression win.
        assert!(out.len() < png.len() / 4, "expected >4× shrink, got {} vs {}", out.len(), png.len());
    }

    #[test]
    fn resize_preserves_aspect_ratio() {
        let png = synth_albedo_png(256, 128);
        let out = resize_and_recompress_albedo(&png, 64).unwrap();
        assert_eq!(png_dims(&out), (64, 32));
    }

    #[test]
    fn attr_already_present_matches_whole_words() {
        let body = r#"color=[1,0,0], normal_texture="a.png""#;
        assert!(attr_already_present(body, "normal_texture"));
        assert!(attr_already_present(body, "color"));
        // Mustn't match a prefix of another attr: "texture" should not match
        // "normal_texture" here because there's no `texture=` in the body.
        assert!(!attr_already_present(body, "texture"));
        assert!(!attr_already_present(body, "base_color_texture"));
    }
}
