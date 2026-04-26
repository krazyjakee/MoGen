use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::imageops::FilterType as ResampleFilter;
use image::{DynamicImage, ExtendedColorType, ImageEncoder, ImageFormat, Rgba, RgbaImage};
use mogen_core::Span;
use mogen_dsl::ast::{Node, Value};

use crate::gemini::{GeminiClient, GeminiError};
use crate::image::GeneratedImage;
use crate::pbr_maps::{derive_pbr_maps, PbrMapOptions};

use super::plan::{Plan, PlanAction, SlotKind, TexturesArgs};
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
/// texture folder, and return the [`Edit`]s the splicer should apply.
/// `progress_cb`, when supplied, is invoked once per stage transition of each
/// material (see [`TextureProgress`]).
pub fn run_plan(
    client: Option<&GeminiClient>,
    model: &str,
    args: &TexturesArgs,
    ast: &[Node],
    plans: &[Plan],
    base_dir: &Path,
    progress_cb: Option<&dyn Fn(TextureProgress)>,
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

    // Pre-compute the total once so callback receivers can render "i / N".
    // Count only materials that actually have work to do (matches the `Skip`
    // filter the GUI uses to decide whether to bail early).
    let total_materials: u32 = by_material
        .iter()
        .filter(|(_, _, ps)| ps.iter().any(|p| plan_has_work(&p.action)))
        .count() as u32;

    let mut edits = Vec::new();
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
        // Process albedo first so its bytes are ready for any Derive plans.
        let albedo_plan = mat_plans.iter().find(|p| p.kind == SlotKind::Albedo).copied();
        let mut albedo_bytes: Option<Vec<u8>> = None;

        if let Some(p) = albedo_plan {
            match &p.action {
                PlanAction::Generate => {
                    emit(TextureStage::Generating);
                    let bytes = resolve_albedo_bytes(client, model, p, args.texture_size)?;
                    // Mask-mode materials want a foliage cutout: chroma-key
                    // the pure-black backdrop into alpha=0 so the leaf
                    // silhouette becomes the visible shape on the leaf_card.
                    let bytes = if p.is_mask {
                        chroma_key_black_to_alpha(&bytes)
                            .with_context(|| {
                                format!("chroma-keying mask albedo for material {}", p.material)
                            })?
                    } else {
                        bytes
                    };
                    let abs = base_dir.join(&p.rel_path);
                    write_png(&abs, &bytes)?;
                    edits.push(Edit {
                        span: p.span,
                        attr: p.kind.attr(),
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
                    let bytes = fs::read(&abs).with_context(|| {
                        format!("reading existing albedo {}", abs.display())
                    })?;
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

        // Derived slots whose PNGs already exist on disk: just splice the
        // attrs in, no derivation needed. Done before the Derive block so
        // materials with all-existing slots still get spliced even when the
        // derive block is a no-op.
        for p in &mat_plans {
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
                // than erroring the whole run.
                emit(TextureStage::Done);
                continue;
            };
            emit(TextureStage::Deriving);
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
        emit(TextureStage::Done);
    }
    Ok(edits)
}

fn resolve_albedo_bytes(
    client: Option<&GeminiClient>,
    model: &str,
    plan: &Plan,
    max_side: u32,
) -> Result<Vec<u8>> {
    let client = client.ok_or_else(|| {
        anyhow!("no GeminiClient available for material {}", plan.material)
    })?;
    // Fresh per-material random seed so repeated runs over the same prompt
    // don't keep landing on the same Gemini sample.
    let seed = Some(random_seed());
    let img = generate_with_recitation_retry(
        client,
        model,
        &plan.prompt,
        RECITATION_RETRIES,
        seed,
    )
    .map_err(|e: GeminiError| anyhow!("gemini image: {e}"))?;
    resize_and_recompress_albedo(&img.png_bytes, max_side)
}

/// Downscale the albedo so its longer side is at most `max_side` and
/// re-encode with the PNG `Best` compression preset. Returns the original
/// bytes when `max_side == 0` or the image is already within the cap — no
/// point re-encoding if we have nothing to change. RGBA inputs (foliage
/// cutouts produced by [`chroma_key_black_to_alpha`]) keep their alpha
/// channel through the resize and re-encode.
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
    let has_alpha = image_has_alpha(&img);
    // Lanczos3 is the slowest-but-sharpest resampler in the image crate.
    // PBR textures downscale once at generation time and are then read many
    // times, so quality wins over speed here.
    let resized = img.resize(max_side, max_side, ResampleFilter::Lanczos3);
    let mut buf = Vec::new();
    if has_alpha {
        let rgba = resized.to_rgba8();
        PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive)
            .write_image(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                ExtendedColorType::Rgba8,
            )
            .context("encoding resized RGBA albedo PNG")?;
    } else {
        let rgb = resized.to_rgb8();
        PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive)
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ExtendedColorType::Rgb8,
            )
            .context("encoding resized RGB albedo PNG")?;
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
pub(crate) fn chroma_key_black_to_alpha(png: &[u8]) -> Result<Vec<u8>> {
    const LUMA_THRESHOLD: f32 = 0.10;
    let img = image::load_from_memory_with_format(png, ImageFormat::Png)
        .context("decoding foliage cutout PNG")?;
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

/// Call `generate_image`, retrying on `IMAGE_RECITATION` with a short
/// stylistic-variation suffix. Recitation rejections are sticky for the exact
/// same prompt — re-issuing as-is just burns quota — so each retry appends a
/// distinct hint that nudges the model toward a different sample without
/// changing what the texture is supposed to depict.
///
/// `seed` is forwarded to every attempt so callers can drive sampling
/// variation across whole runs; the variation hint handles intra-call variety
/// on recitation retries.
pub fn generate_with_recitation_retry(
    client: &GeminiClient,
    model: &str,
    base_prompt: &str,
    max_retries: u32,
    seed: Option<u64>,
) -> Result<GeneratedImage, GeminiError> {
    let mut attempt: u32 = 0;
    loop {
        let prompt = if attempt == 0 {
            base_prompt.to_string()
        } else {
            let hint = recitation_variation_hint(attempt);
            format!("{base_prompt}\nVariation hint: {hint}")
        };
        match client.generate_image(model, &prompt, seed) {
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
}
