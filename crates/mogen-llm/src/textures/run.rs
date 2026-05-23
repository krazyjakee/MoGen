use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::imageops::FilterType as ResampleFilter;
use image::{DynamicImage, ExtendedColorType, ImageEncoder, Rgba, RgbaImage};
use mogen_core::Span;
use mogen_dsl::ast::{Node, Value};

use crate::gemini::GeminiError;
use crate::image::GeneratedImage;
use crate::image_client::{ImageClient, ImageError};
use crate::pbr_maps::{derive_pbr_maps, PbrMapOptions};

use super::plan::{Plan, PlanAction, SlotKind, TexturesArgs};
use super::prompt::NOTE_PREFIX;
use super::splice::Edit;

/// How many times to retry an `IMAGE_RECITATION`-rejected prompt with a small
/// stylistic variation before giving up. Each retry sends a fresh request and
/// burns API quota, so keep this modest.
const RECITATION_RETRIES: u32 = 3;

/// Per-material progress emitted by [`run_plan`] as it walks the plan. Consumers
/// pass an [`Fn`] into `progress_cb` to receive these — the CLI prints them,
/// the GUI threads them to its status line.
#[derive(Debug, Clone)]
pub struct TextureProgress {
    /// 1-based index of the material currently being processed.
    pub current: u32,
    /// Total number of materials with at least one slot to touch.
    pub total: u32,
    pub material: String,
    pub stage: TextureStage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureStage {
    /// Calling the image model for the albedo.
    Generating,
    /// A PNG already existed at the planned path; we're splicing the attr
    /// into the source without burning API credit or re-deriving anything.
    Existing,
    /// Deriving the PBR companion maps (normal/MR/AO) locally.
    Deriving,
    /// All slots for this material wrote successfully.
    Done,
    /// This material's processing failed — its slots are skipped and the
    /// run continues with the next material. Surfaced so the UI can render
    /// "X failed" alongside the "Y / Z" counter without aborting the whole
    /// run.
    Failed,
}

/// One material that failed during a [`run_plan`] call. Other materials
/// continue regardless — the caller decides how to surface the partial
/// failure (CLI prints, Studio shows a banner with a Retry button).
#[derive(Debug, Clone)]
pub struct MaterialFailure {
    pub material: String,
    /// Pre-rendered error chain (`{e:#}`) so consumers don't need to depend
    /// on `anyhow` to display it.
    pub error: String,
}

/// Result of running a textures plan. Splice these `edits` to apply whatever
/// did succeed, then surface `failures` to the user. A run with zero edits
/// and N failures is still a structurally successful return — it just means
/// every material failed, and the caller decides how to present that.
#[derive(Debug, Clone, Default)]
pub struct RunReport {
    pub edits: Vec<Edit>,
    pub failures: Vec<MaterialFailure>,
}

/// True when the plan represents an action that touches the source or disk
/// (everything except `Skip`). `UseExisting` counts because it still splices
/// the `*_texture` attribute into the .mog even though it bypasses the API.
fn plan_has_work(action: &PlanAction) -> bool {
    matches!(
        action,
        PlanAction::Generate | PlanAction::Derive | PlanAction::UseExisting
    )
}

fn pbr_opts(_args: &TexturesArgs, material: &Node) -> PbrMapOptions {
    let number_attr = |key: &str| {
        material.attr(key).and_then(|v| match v {
            Value::Number(n) => Some(*n),
            _ => None,
        })
    };
    let mut opts = PbrMapOptions::default();
    if let Some(r) = number_attr("roughness") {
        opts.roughness_base = r.clamp(0.0, 1.0);
    }
    if let Some(m) = number_attr("metallic") {
        opts.metallic = m.clamp(0.0, 1.0);
    }
    if let Some(n) = number_attr("normal_strength") {
        if n.is_finite() && n > 0.0 {
            opts.normal_strength = n;
        }
    }
    if let Some(o) = number_attr("occlusion_strength") {
        if o.is_finite() && o >= 0.0 {
            opts.occlusion_strength = o.clamp(0.0, 1.0);
        }
    }
    opts
}

/// Execute the plan: generate each PNG, write it into the `.mog` directory's
/// texture folder, and return a [`RunReport`] containing both the [`Edit`]s
/// the splicer should apply and any per-material failures encountered along
/// the way. A failure on one material does NOT abort the run — every other
/// material is still attempted, and the report's `failures` field lets the
/// caller decide how to surface the partial outcome.
///
/// `progress_cb`, when supplied, is invoked once per stage transition of each
/// material (see [`TextureProgress`]).
pub fn run_plan(
    client: Option<&ImageClient>,
    model: &str,
    args: &TexturesArgs,
    ast: &[Node],
    plans: &[Plan],
    base_dir: &Path,
    progress_cb: Option<&dyn Fn(TextureProgress)>,
) -> RunReport {
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

    // Pre-compute the total once so callback receivers can render "i / N".
    // Count only materials that actually have work to do (matches the `Skip`
    // filter the GUI uses to decide whether to bail early).
    let total_materials: u32 = by_material
        .iter()
        .filter(|(_, _, ps)| ps.iter().any(|p| plan_has_work(&p.action)))
        .count() as u32;

    let mut edits = Vec::new();
    let mut failures: Vec<MaterialFailure> = Vec::new();
    let mut material_index: u32 = 0;

    for (mat_name, _span, mat_plans) in by_material {
        // Skip materials where every slot is a no-op — no need to surface them
        // in progress, they just clutter the status line.
        let has_work = mat_plans.iter().any(|p| plan_has_work(&p.action));
        if !has_work {
            continue;
        }
        material_index += 1;
        let emit = |stage: TextureStage| {
            if let Some(cb) = progress_cb {
                cb(TextureProgress {
                    current: material_index,
                    total: total_materials,
                    material: mat_name.clone(),
                    stage,
                });
            }
        };

        // Per-material work runs through this closure so any failure short-
        // circuits *just this material* — the outer loop catches the Err and
        // continues with the next material instead of aborting the whole run.
        // Edits already pushed into `edits` before the failure point stay in;
        // the DSL's `*_texture` slots are independent so a partial write is
        // valid (e.g. albedo PNG written but normals derive crashed → user
        // still gets the albedo spliced in).
        let result = process_material(
            client,
            model,
            args,
            &material_nodes,
            &mat_name,
            &mat_plans,
            base_dir,
            &emit,
            &mut edits,
        );

        match result {
            Ok(()) => emit(TextureStage::Done),
            Err(e) => {
                emit(TextureStage::Failed);
                failures.push(MaterialFailure {
                    material: mat_name.clone(),
                    error: format!("{e:#}"),
                });
            }
        }
    }
    RunReport { edits, failures }
}

/// Run every plan slot for one material. Returning `Result` lets each `?`
/// short-circuit the rest of *this* material's slots; the caller in
/// [`run_plan`] catches the error and records a [`MaterialFailure`] so the
/// run as a whole continues. Decomposed out of the loop body so the
/// per-material error boundary is obvious from the call site.
#[allow(clippy::too_many_arguments)]
fn process_material(
    client: Option<&ImageClient>,
    model: &str,
    args: &TexturesArgs,
    material_nodes: &HashMap<String, &Node>,
    mat_name: &str,
    mat_plans: &[&Plan],
    base_dir: &Path,
    emit: &dyn Fn(TextureStage),
    edits: &mut Vec<Edit>,
) -> Result<()> {
    // Process albedo first so its bytes are ready for any Derive plans.
    let albedo_plan = mat_plans.iter().find(|p| p.kind == SlotKind::Albedo).copied();
    let mut albedo_bytes: Option<Vec<u8>> = None;

    if let Some(p) = albedo_plan {
        // Decals splice as `image="…"` on the decal node; everything else
        // splices as `base_color_texture="…"` on a material declaration.
        let albedo_attr: &'static str = if p.is_decal { "image" } else { p.kind.attr() };
        match &p.action {
            PlanAction::Generate => {
                emit(TextureStage::Generating);
                let bytes = resolve_albedo_bytes(
                    client,
                    model,
                    p,
                    args.texture_size,
                    &crate::spend::CallContext::new(crate::spend::Operation::Textures)
                        .with_scene(args.input.display().to_string()),
                )?;
                // Mask-mode materials want a foliage cutout: chroma-key
                // the pure-black backdrop into alpha=0 so the leaf
                // silhouette becomes the visible shape on the leaf_card.
                // Decals already get RGBA from Gemini directly, so they
                // skip the chroma-key path even though they're transparent.
                let bytes = if p.is_mask && !p.is_decal {
                    chroma_key_black_to_alpha(&bytes).with_context(|| {
                        format!("chroma-keying mask albedo for material {}", p.material)
                    })?
                } else {
                    bytes
                };
                let abs = base_dir.join(&p.rel_path);
                write_png(&abs, &bytes)?;
                edits.push(Edit {
                    span: p.span,
                    attr: albedo_attr,
                    rel_path: to_forward_slashes(&p.rel_path),
                });
                albedo_bytes = Some(bytes);
            }
            PlanAction::UseExisting => {
                // PNG already on disk at the planned path. Splice the
                // attr into the source without an API call, and load the
                // bytes so any sibling Derive plans can still produce
                // their maps from the existing albedo.
                emit(TextureStage::Existing);
                let abs = base_dir.join(&p.rel_path);
                let bytes = fs::read(&abs)
                    .with_context(|| format!("reading existing albedo {}", abs.display()))?;
                edits.push(Edit {
                    span: p.span,
                    attr: albedo_attr,
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

    // Derived slots whose PNGs already exist on disk: just splice the
    // attrs in, no derivation needed. Done before the Derive block so
    // materials with all-existing slots still get spliced even when the
    // derive block is a no-op.
    for p in mat_plans {
        if !matches!(p.action, PlanAction::UseExisting) {
            continue;
        }
        if p.kind == SlotKind::Albedo {
            continue;
        }
        edits.push(Edit {
            span: p.span,
            attr: p.kind.attr(),
            rel_path: to_forward_slashes(&p.rel_path),
        });
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
            // than erroring this material.
            return Ok(());
        };
        emit(TextureStage::Deriving);
        let node = material_nodes
            .get(mat_name)
            .ok_or_else(|| anyhow!("internal: no AST node for material {mat_name}"))?;
        let mut opts = pbr_opts(args, node);
        opts.generate_normal = wants_normal;
        opts.generate_metallic_roughness = wants_mr;
        opts.generate_occlusion = wants_ao;

        let maps = derive_pbr_maps(&bytes, &opts)
            .with_context(|| format!("deriving PBR maps for material {mat_name}"))?;

        for p in mat_plans {
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
    Ok(())
}

fn resolve_albedo_bytes(
    client: Option<&ImageClient>,
    model: &str,
    plan: &Plan,
    max_side: u32,
    ctx: &crate::spend::CallContext,
) -> Result<Vec<u8>> {
    let client = client.ok_or_else(|| {
        anyhow!("no image client available for material {}", plan.material)
    })?;
    // Fresh per-material random seed so repeated runs over the same prompt
    // don't keep landing on the same model sample.
    let seed = Some(random_seed());
    let img = generate_with_recitation_retry_ctx(
        client,
        model,
        &plan.prompt,
        RECITATION_RETRIES,
        seed,
        ctx,
    )
    .map_err(|e: ImageError| anyhow!("{} image: {e}", client.provider_name()))?;
    resize_and_recompress_albedo(&img.png_bytes, max_side)
}

/// Downscale the albedo so its longer side is at most `max_side` and
/// re-encode with the PNG `Best` compression preset. Auto-detects the input
/// container format (PNG via the API-key surface, JPEG via the Antigravity
/// Cloud Code Assist surface) and always emits PNG so the on-disk extension
/// stays truthful. Pass-through (raw bytes) only when the input is already
/// PNG and within the size cap. RGBA inputs (foliage cutouts produced by
/// [`chroma_key_black_to_alpha`]) keep their alpha channel through the
/// resize and re-encode.
fn resize_and_recompress_albedo(bytes: &[u8], max_side: u32) -> Result<Vec<u8>> {
    let is_png = bytes.starts_with(&[0x89, b'P', b'N', b'G']);
    if max_side == 0 && is_png {
        return Ok(bytes.to_vec());
    }
    let img = image::load_from_memory(bytes)
        .context("decoding albedo image for resize")?;
    let (w, h) = (img.width(), img.height());
    if is_png && max_side > 0 && w <= max_side && h <= max_side {
        return Ok(bytes.to_vec());
    }
    let needs_resize = max_side > 0 && (w > max_side || h > max_side);
    let working = if needs_resize {
        // Lanczos3 is the slowest-but-sharpest resampler in the image crate.
        // PBR textures downscale once at generation time and are then read many
        // times, so quality wins over speed here.
        img.resize(max_side, max_side, ResampleFilter::Lanczos3)
    } else {
        img
    };
    let has_alpha = image_has_alpha(&working);
    let mut buf = Vec::new();
    if has_alpha {
        let rgba = working.to_rgba8();
        PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive)
            .write_image(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                ExtendedColorType::Rgba8,
            )
            .context("encoding RGBA albedo PNG")?;
    } else {
        let rgb = working.to_rgb8();
        PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive)
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ExtendedColorType::Rgb8,
            )
            .context("encoding RGB albedo PNG")?;
    }
    Ok(buf)
}

fn image_has_alpha(img: &DynamicImage) -> bool {
    use image::DynamicImage::*;
    matches!(
        img,
        ImageLumaA8(_) | ImageLumaA16(_) | ImageRgba8(_) | ImageRgba16(_) | ImageRgba32F(_)
    )
}

/// Convert a Gemini-generated foliage cutout (leaf cluster on uniform
/// pure-black background) into an RGBA8 PNG with alpha=0 outside the leaf
/// silhouette. The hard luminance threshold is what matters at render time —
/// glTF `alphaMode=MASK` discards on a single cutoff, so a binary alpha map
/// reads cleanly without producing the gray fringes a soft alpha would leave.
///
/// `LUMA_THRESHOLD = 0.10` (in [0, 1]) is well below any natural leaf colour
/// the model produces (even very dark green veins land near 0.15) and well
/// above the residual noise Gemini puts on a "pure black" backdrop (~0.02).
pub(crate) fn chroma_key_black_to_alpha(bytes: &[u8]) -> Result<Vec<u8>> {
    const LUMA_THRESHOLD: f32 = 0.10;
    let img = image::load_from_memory(bytes)
        .context("decoding foliage cutout image")?;
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    let mut out = RgbaImage::new(w, h);
    for (x, y, p) in rgb.enumerate_pixels() {
        let r = p[0] as f32 / 255.0;
        let g = p[1] as f32 / 255.0;
        let b = p[2] as f32 / 255.0;
        // Rec. 709 luminance — matches what the human eye reads as "dark".
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let alpha = if luma < LUMA_THRESHOLD { 0u8 } else { 255u8 };
        out.put_pixel(x, y, Rgba([p[0], p[1], p[2], alpha]));
    }
    let mut buf = Vec::new();
    PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive)
        .write_image(out.as_raw(), w, h, ExtendedColorType::Rgba8)
        .context("encoding keyed foliage RGBA PNG")?;
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

/// Call `generate_image`, retrying on `IMAGE_RECITATION` by rewriting the
/// prompt's `Subject:` line and appending a short stylistic-variation suffix.
/// Recitation rejections are sticky for the exact same prompt — re-issuing
/// as-is just burns quota — so each retry mutates the subject (the most
/// recitation-prone part) and adds a distinct hint that nudges the model
/// toward a different sample without changing what the texture is supposed
/// to depict.
///
/// `seed` is forwarded to every attempt so callers can drive sampling
/// variation across whole runs; the per-attempt subject rewrite + variation
/// hint handles intra-call variety on recitation retries.
pub fn generate_with_recitation_retry(
    client: &ImageClient,
    model: &str,
    base_prompt: &str,
    max_retries: u32,
    seed: Option<u64>,
) -> Result<GeneratedImage, ImageError> {
    generate_with_recitation_retry_ctx(
        client,
        model,
        base_prompt,
        max_retries,
        seed,
        &crate::spend::CallContext::default(),
    )
}

/// Spend-tracking variant of [`generate_with_recitation_retry`]. Each
/// successful or failed attempt lands a row in the spend DB tagged with
/// `ctx.operation` / `ctx.scene_path` so the Spending panel can show
/// per-material costs for a texture run.
pub fn generate_with_recitation_retry_ctx(
    client: &ImageClient,
    model: &str,
    base_prompt: &str,
    max_retries: u32,
    seed: Option<u64>,
    ctx: &crate::spend::CallContext,
) -> Result<GeneratedImage, ImageError> {
    let mut attempt: u32 = 0;
    loop {
        let prompt = build_attempt_prompt(base_prompt, attempt);
        match client.generate_image_with_context(model, &prompt, seed, ctx) {
            Ok(img) => return Ok(img),
            // Gemini-specific recitation rejection: the safety filter latched
            // onto literal phrasing in the subject. Rephrase + retry. Other
            // providers (Z.ai) have no equivalent — their errors fall to the
            // catch-all and propagate immediately.
            Err(ImageError::Gemini(GeminiError::InvalidResponse(ref msg)))
                if msg.contains("IMAGE_RECITATION") && attempt < max_retries =>
            {
                attempt += 1;
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Produce the prompt to send on attempt `n`. Attempt 0 is the original
/// prompt verbatim; later attempts rephrase the author-supplied
/// `Material note:` line (when present) with a per-attempt prefix and append
/// a `Variation hint:` suffix. We mutate the note rather than just appending
/// hints because Gemini's recitation filter latches on to the literal
/// phrasing of the subject text — when an author writes
/// `prompt="navy nylon ripstop weave"` and that exact phrasing trips the
/// filter, repeating it verbatim on every retry just locks in the rejection.
/// When no `Material note:` is present we fall back to suffix-only variation,
/// matching the previous behaviour for materials that don't opt in.
fn build_attempt_prompt(base: &str, attempt: u32) -> String {
    if attempt == 0 {
        return base.to_string();
    }
    let prefix = note_variation_prefix(attempt);
    let mut out = String::with_capacity(base.len() + 64);
    let mut rewrote = false;
    for line in base.split_inclusive('\n') {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if !rewrote && trimmed.starts_with(NOTE_PREFIX) {
            let lead_len = line.len() - trimmed.len();
            out.push_str(&line[..lead_len]);
            out.push_str(NOTE_PREFIX);
            let body = trimmed[NOTE_PREFIX.len()..].trim_end_matches('\n');
            out.push_str(prefix);
            out.push_str(body);
            out.push('\n');
            rewrote = true;
        } else {
            out.push_str(line);
        }
    }
    let hint = recitation_variation_hint(attempt);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("Variation hint: ");
    out.push_str(hint);
    out
}

/// Per-attempt prefix prepended to the `Material note:` line. Each variant
/// reframes the same content as a different render so the recitation filter
/// stops matching the literal phrasing without losing the user's intent.
fn note_variation_prefix(attempt: u32) -> &'static str {
    const PREFIXES: &[&str] = &[
        "alternative interpretation of ",
        "fresh take on ",
        "uncommon variant of ",
        "reimagined version of ",
    ];
    PREFIXES[(attempt as usize - 1) % PREFIXES.len()]
}

/// Mint a fresh seed for an image generation call.
///
/// Combines wall-clock nanoseconds with a process-wide atomic counter and
/// runs the result through SplitMix64, so back-to-back calls that land in the
/// same nanosecond still produce independent seeds. Avoids pulling in a `rand`
/// dependency for what's effectively one u64 per material per run.
fn random_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut x = nanos.wrapping_add(n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Synthesise a "leaf cluster on black": a centred filled disc on solid
    /// pure-black RGB. Mirrors what Gemini is being prompted to emit.
    fn synth_leaf_on_black_png(side: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(side, side);
        let cx = side as f32 * 0.5;
        let cy = side as f32 * 0.5;
        let r = side as f32 * 0.35;
        for y in 0..side {
            for x in 0..side {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let inside = (dx * dx + dy * dy) <= r * r;
                let pix = if inside {
                    image::Rgb([60, 140, 60])
                } else {
                    image::Rgb([0, 0, 0])
                };
                img.put_pixel(x, y, pix);
            }
        }
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn chroma_key_makes_black_transparent_and_keeps_leaves() {
        // The RGBA output must have:
        //  - alpha=0 on every pixel of the black backdrop,
        //  - alpha=255 inside the leaf disc,
        //  - leaf RGB preserved (chroma-key is not supposed to recolour).
        let png = synth_leaf_on_black_png(64);
        let keyed = chroma_key_black_to_alpha(&png).unwrap();
        let img = image::load_from_memory_with_format(&keyed, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        let cx = img.width() / 2;
        let cy = img.height() / 2;
        // Centre of the disc must read solid leaf colour with full alpha.
        let centre = img.get_pixel(cx, cy);
        assert_eq!(centre[3], 255, "leaf centre should be opaque");
        assert!(centre[1] > 100, "leaf colour preserved through key");
        // Corner is pure background — must have been keyed out.
        let corner = img.get_pixel(0, 0);
        assert_eq!(corner[3], 0, "black corner must key to alpha=0");
    }

    #[test]
    fn chroma_key_produces_rgba_png() {
        // The output must declare RGBA8 in the PNG header — without this,
        // glTF readers won't see an alpha channel and `alpha_mode=MASK`
        // becomes a no-op.
        let png = synth_leaf_on_black_png(32);
        let keyed = chroma_key_black_to_alpha(&png).unwrap();
        let img = image::load_from_memory_with_format(&keyed, image::ImageFormat::Png).unwrap();
        assert!(matches!(img, image::DynamicImage::ImageRgba8(_)));
    }

    /// Regression: a single material failing must not abort the run. With
    /// `client = None`, every `Generate` plan errors out at
    /// `resolve_albedo_bytes` ("no GeminiClient available"). The old
    /// implementation bailed on the first such error and dropped the rest of
    /// the plan; the new implementation collects the failure and continues.
    #[test]
    fn run_plan_isolates_per_material_failures() {
        use super::super::plan::{build_plan, TexturesArgs};
        use std::path::PathBuf;
        let src = r#"
material "wood" (color=[0.5, 0.3, 0.1])
material "metal" (color=[0.8, 0.8, 0.9])
"#;
        let ast = mogen_dsl::parse(src).expect("parse");
        let args = TexturesArgs::with_defaults(PathBuf::from("scene.mog"));
        let plans = build_plan(&ast, &args);
        let tmp = std::env::temp_dir().join(format!(
            "mogen-textures-isolate-{}",
            std::process::id()
        ));
        // run_plan with no client → every Generate plan fails at the API
        // resolve step, but the loop must keep going.
        let report = run_plan(None, "gemini-2.5-flash-image", &args, &ast, &plans, &tmp, None);
        // Both materials reported as failed (ordering matches plan order).
        let names: Vec<&str> = report.failures.iter().map(|f| f.material.as_str()).collect();
        assert!(names.contains(&"wood"), "missing 'wood' failure: {names:?}");
        assert!(names.contains(&"metal"), "missing 'metal' failure: {names:?}");
        // No edits committed because the albedo step failed before any
        // splice could be queued.
        assert!(
            report.edits.is_empty(),
            "expected no edits on full failure, got {} edits",
            report.edits.len()
        );
    }

    /// Failed stage must be emitted to the progress callback exactly once
    /// per failed material so the GUI can render "X failed" without the
    /// loop quietly skipping the event.
    #[test]
    fn run_plan_emits_failed_stage() {
        use super::super::plan::{build_plan, TexturesArgs};
        use std::path::PathBuf;
        use std::sync::Mutex;
        let src = r#"material "wood" (color=[0.5, 0.3, 0.1])"#;
        let ast = mogen_dsl::parse(src).expect("parse");
        let args = TexturesArgs::with_defaults(PathBuf::from("scene.mog"));
        let plans = build_plan(&ast, &args);
        let tmp = std::env::temp_dir().join(format!(
            "mogen-textures-stage-{}",
            std::process::id()
        ));
        let stages: Mutex<Vec<TextureStage>> = Mutex::new(Vec::new());
        let cb = |ev: TextureProgress| {
            stages.lock().unwrap().push(ev.stage);
        };
        let report = run_plan(None, "gemini-2.5-flash-image", &args, &ast, &plans, &tmp, Some(&cb));
        assert_eq!(report.failures.len(), 1);
        let stages = stages.into_inner().unwrap();
        assert!(
            stages.contains(&TextureStage::Failed),
            "expected Failed stage in {stages:?}"
        );
        // Done must NOT be emitted on a failed material — that's the whole
        // point of distinguishing the two terminal stages.
        assert!(
            !stages.contains(&TextureStage::Done),
            "Failed material should not emit Done: {stages:?}"
        );
    }

    #[test]
    fn resize_preserves_alpha_for_keyed_input() {
        // Routing an RGBA PNG through the resize path used to silently drop
        // alpha (the old code unconditionally hit `to_rgb8()`). Regression
        // guard: the post-resize PNG still carries the alpha channel.
        let keyed = chroma_key_black_to_alpha(&synth_leaf_on_black_png(256)).unwrap();
        let resized = resize_and_recompress_albedo(&keyed, 64).unwrap();
        let img = image::load_from_memory_with_format(&resized, image::ImageFormat::Png).unwrap();
        assert!(
            matches!(img, image::DynamicImage::ImageRgba8(_)),
            "resized image lost its alpha channel"
        );
        assert_eq!((img.width(), img.height()), (64, 64));
    }

    #[test]
    fn attempt_zero_returns_prompt_verbatim() {
        let base = "Material name: fabric_main\nMaterial note: navy nylon weave\n";
        assert_eq!(build_attempt_prompt(base, 0), base);
    }

    #[test]
    fn retry_rewrites_material_note_and_appends_hint() {
        // The retry helper must (a) prefix the `Material note:` body with a
        // per-attempt rephrase and (b) append a `Variation hint:` suffix.
        // Both jobs together break Gemini's recitation lock: the prefix
        // changes the literal subject phrasing, the hint pushes sampling
        // toward a different output.
        let base = "Material name: fabric_main\n\
                    Material note: navy nylon ripstop weave\n\
                    \nReminder: surface only.";
        let attempt1 = build_attempt_prompt(base, 1);
        assert!(
            attempt1.contains("Material note: alternative interpretation of navy nylon ripstop weave"),
            "missing per-attempt prefix on note: {attempt1}"
        );
        assert!(attempt1.contains("Variation hint: "));
        // Untouched lines stay intact.
        assert!(attempt1.contains("Material name: fabric_main"));
        assert!(attempt1.contains("Reminder: surface only."));
    }

    #[test]
    fn retry_rotates_through_distinct_prefixes() {
        // Repeating the same prefix on every retry would let recitation
        // re-lock immediately. Ensure attempts 1..=4 produce four different
        // rephrased note bodies.
        let base = "Material note: navy nylon weave\n";
        let mut seen = std::collections::BTreeSet::new();
        for n in 1..=4 {
            let p = build_attempt_prompt(base, n);
            let line = p.lines().find(|l| l.starts_with("Material note: ")).unwrap();
            seen.insert(line.to_string());
        }
        assert_eq!(seen.len(), 4, "expected 4 distinct rephrases, got {seen:?}");
    }

    #[test]
    fn retry_without_note_falls_back_to_hint_only() {
        // Materials that don't author a `prompt="…"` attribute have no note
        // line to rewrite. The retry should still nudge variation via the
        // suffix-only hint, matching the previous behaviour.
        let base = "Material name: stone\nStyle: photorealistic\n";
        let attempt1 = build_attempt_prompt(base, 1);
        assert!(!attempt1.contains("Material note:"));
        assert!(attempt1.contains("Variation hint: "));
    }
}
