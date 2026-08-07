//! Rasterize `.svg` texture references into PNG before anything downstream
//! looks at them.
//!
//! glTF 2.0 core defines exactly two embeddable image MIME types —
//! `image/png` and `image/jpeg` — so an SVG can never ship as an SVG. Support
//! therefore means rasterizing at build time, which suits `mogen` fine: it is
//! an offline compiler and rasterization is a pure function of
//! `(bytes, size, wrap)`.
//!
//! # Why this is a pass and not a decode branch
//!
//! The obvious place to put this is [`crate::texture::encode_for_slot`], next
//! to the JPEG/oxipng policy. That would be wrong. Five separate consumers
//! read a [`TextureRef`] path, and only one of them goes through that
//! function:
//!
//! | consumer | reads |
//! |---|---|
//! | GLB embed | `encode_for_slot` |
//! | FBX export | the **raw path** (`fbx/material.rs`) — a `.svg` would leak into the FBX |
//! | Studio preview | its own loaders under `viewer/` |
//! | PBR map derivation | `mogen-llm::pbr_maps`, which hard-codes `ImageFormat::Png` |
//! | imposter bake | already-baked pixels; unaffected |
//!
//! Branching at decode time would need four separate fixes. Rewriting the
//! path *before* export means every one of them only ever sees a PNG, and the
//! derived-PBR pipeline picks up SVG albedo support for free.
//!
//! # No cache
//!
//! Rasterized bytes live in memory for the duration of the build and are
//! handed to the exporter through [`OverlayTextureSource`]. Nothing is written
//! to `MOGEN_CACHE_DIR`. That costs a re-render per build (milliseconds for
//! the tile-sized art this is for) and buys portability — the pass works
//! unchanged on wasm, where there is no filesystem to cache into — plus
//! freedom from a whole category of staleness bugs. It matches the precedent
//! set by the generated-texture pipeline in `mogen-llm`, which also declines
//! to cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use mogen_core::{Material, SceneGraph, TextureRef, DEFAULT_SVG_SIZE};

use crate::texture::TextureSource;

/// Upper bound on the rasterized edge length. 8192² RGBA is 256 MB before PNG
/// compression, which is already well past anything a texture slot should be
/// carrying; beyond it a typo in `texture_size` turns into an OOM rather than
/// an error message.
pub const MAX_SVG_SIZE: u32 = 8192;

/// Does this path look like an SVG? Extension-based, matching how the rest of
/// the exporter dispatches on format (`texture::mime_from_extension`).
pub fn is_svg(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("svg"))
}

/// The synthetic path a rasterized SVG is republished under. Keeps a `.png`
/// extension so extension-based MIME inference — the wasm embed path, which
/// runs without the `image` crate — keeps working untouched. The size and wrap
/// mode are in the stem so one SVG used at two sizes doesn't collide in the
/// exporter's `(path, SlotKind)` dedup map.
///
/// The `.svg` extension is dropped rather than appended to. The exporter names
/// each glTF image after its file stem, so keeping it would leave a
/// `"name": "tile.svg.1024.raster"` in the output — a vector path in a file
/// that provably contains none, which anything grepping the glTF would
/// reasonably misread.
fn raster_path(src: &Path, size: u32, wrap: bool) -> PathBuf {
    let suffix = if wrap { "w" } else { "" };
    let stem = src.with_extension("");
    PathBuf::from(format!("{}.raster{size}{suffix}.png", stem.display()))
}

/// A scene with every `.svg` texture path rewritten, plus the PNG bytes those
/// new paths resolve to.
pub struct SvgRaster {
    /// Copy of the input scene with SVG texture paths repointed at their
    /// rasterized equivalents.
    pub scene: SceneGraph,
    /// `synthetic path -> PNG bytes`, to be layered over the caller's
    /// [`TextureSource`] via [`OverlayTextureSource`].
    pub images: HashMap<PathBuf, Vec<u8>>,
}

/// A [`TextureSource`] that answers from an in-memory map first and falls
/// through to a base source otherwise. Lets rasterized SVGs and on-disk
/// rasters coexist without the exporter knowing which is which.
pub struct OverlayTextureSource<'a> {
    base: &'a dyn TextureSource,
    overlay: &'a HashMap<PathBuf, Vec<u8>>,
}

impl<'a> OverlayTextureSource<'a> {
    pub fn new(base: &'a dyn TextureSource, overlay: &'a HashMap<PathBuf, Vec<u8>>) -> Self {
        Self { base, overlay }
    }
}

impl TextureSource for OverlayTextureSource<'_> {
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        match self.overlay.get(path) {
            Some(bytes) => Ok(bytes.clone()),
            None => self.base.read(path),
        }
    }
}

/// Rasterize every `.svg` texture referenced by `scene`'s materials.
///
/// Returns `Ok(None)` when the scene references no SVG at all — the common
/// case, and worth detecting so an all-raster scene doesn't pay for a
/// [`SceneGraph`] clone it has no use for.
///
/// Each unique `(path, size, wrap)` triple is rendered once even if several
/// materials or slots share it.
pub fn rasterize_svg_textures(
    scene: &SceneGraph,
    source: &dyn TextureSource,
) -> Result<Option<SvgRaster>> {
    if !scene.materials.iter().any(material_references_svg) {
        return Ok(None);
    }

    let mut scene = scene.clone();
    let mut images: HashMap<PathBuf, Vec<u8>> = HashMap::new();

    for mat in &mut scene.materials {
        let size = resolve_svg_size(mat)?;
        let wrap = mat.texture_wrap;
        for slot in mat.texture_slots_mut() {
            let Some(tex) = slot.as_ref() else { continue };
            if !is_svg(&tex.path) {
                continue;
            }
            let out = raster_path(&tex.path, size, wrap);
            // Several materials can legitimately share one SVG; render once.
            if !images.contains_key(&out) {
                let svg = source.read(&tex.path)?;
                let png = render_svg(&svg, size, wrap).with_context(|| {
                    format!("rasterizing SVG texture {}", tex.path.display())
                })?;
                images.insert(out.clone(), png);
            }
            *slot = Some(TextureRef::new(out));
        }
    }

    Ok(Some(SvgRaster { scene, images }))
}

fn material_references_svg(mat: &Material) -> bool {
    // Mirrors `texture_slots_mut`'s slot list, read-only.
    [
        &mat.base_color_texture,
        &mat.metallic_roughness_texture,
        &mat.normal_texture,
        &mat.occlusion_texture,
        &mat.emissive_texture,
    ]
    .into_iter()
    .flatten()
    .any(|t| is_svg(&t.path))
}

/// Resolve a material's `texture_size` to the pixel edge length its `.svg`
/// slots rasterize to, applying [`DEFAULT_SVG_SIZE`] when unset and rejecting
/// anything outside `1..=MAX_SVG_SIZE`. Exposed so other consumers of a raw
/// `Material` — e.g. MoGen Studio's live viewport, which rasterizes textures
/// outside the export pipeline — resolve the same size the exporter would.
pub fn resolve_svg_size(mat: &Material) -> Result<u32> {
    let size = mat.texture_size.unwrap_or(DEFAULT_SVG_SIZE);
    if size == 0 || size > MAX_SVG_SIZE {
        bail!(
            "material \"{}\" sets texture_size = {size}, which is outside the \
             supported range 1..={MAX_SVG_SIZE}",
            mat.name
        );
    }
    Ok(size)
}

/// Render SVG bytes to a square RGBA PNG of `size`².
///
/// The SVG's own viewBox is scaled to fill the target exactly — a
/// non-square viewBox is stretched rather than letterboxed, because these are
/// texture tiles addressed in UV space, where the aspect correction belongs to
/// `uv_scale` and not to the image.
///
/// With `wrap` on, the tree is drawn nine times on a 3×3 lattice and the
/// centre cell is kept. Artwork that overflows the viewBox therefore reappears
/// on the opposite edge instead of being clipped, so a pattern tiles seamlessly
/// without the author having to hand-split the shapes that straddle the
/// boundary. This is the one thing a vector source buys that a supplied PNG
/// cannot: the renderer is ours, so the wrap can be synthesised.
///
/// Public so a consumer that already has SVG bytes and a resolved size/wrap —
/// e.g. Studio's live viewport, which rasterizes on the fly outside the
/// export pipeline — can call the same renderer [`rasterize_svg_textures`] uses
/// rather than re-implementing it.
pub fn render_svg(svg: &[u8], size: u32, wrap: bool) -> Result<Vec<u8>> {
    // `usvg::Options` default carries no fontdb (the `text` feature is off in
    // Cargo.toml), so this cannot depend on host-installed fonts.
    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg, &opts).context("parsing SVG")?;

    let vb = tree.size();
    if vb.width() <= 0.0 || vb.height() <= 0.0 {
        bail!("SVG has a zero-sized viewBox; nothing to rasterize");
    }
    let (sx, sy) = (size as f32 / vb.width(), size as f32 / vb.height());

    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)
        .with_context(|| format!("allocating a {size}x{size} pixmap"))?;

    let offsets: &[(f32, f32)] = if wrap {
        &[
            (-1.0, -1.0), (0.0, -1.0), (1.0, -1.0),
            (-1.0, 0.0),  (0.0, 0.0),  (1.0, 0.0),
            (-1.0, 1.0),  (0.0, 1.0),  (1.0, 1.0),
        ]
    } else {
        &[(0.0, 0.0)]
    };

    for (dx, dy) in offsets {
        let t = resvg::tiny_skia::Transform::from_translate(dx * size as f32, dy * size as f32)
            .pre_scale(sx, sy);
        resvg::render(&tree, t, &mut pixmap.as_mut());
    }

    pixmap.encode_png().context("encoding rasterized SVG to PNG")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 100x100 viewBox with a red square covering the left half. Deliberately
    /// asymmetric so a horizontal flip would be detectable.
    const HALF: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
        <rect x="0" y="0" width="50" height="100" fill="#ff0000"/>
    </svg>"##;

    /// A circle centred on the left edge, so half of it falls outside the
    /// viewBox. Under wrap it must reappear on the right edge.
    const OVERHANG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
        <circle cx="0" cy="50" r="20" fill="#00ff00"/>
    </svg>"##;

    fn decode(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .expect("decoding rasterized PNG")
            .to_rgba8()
    }

    #[test]
    fn renders_at_requested_size() {
        let png = render_svg(HALF, 64, false).unwrap();
        let img = decode(&png);
        assert_eq!(img.dimensions(), (64, 64));
    }

    #[test]
    fn scales_viewbox_to_fill_target() {
        // The red half should occupy the left half of the output regardless of
        // the ratio between viewBox units and pixels.
        let img = decode(&render_svg(HALF, 64, false).unwrap());
        assert_eq!(img.get_pixel(10, 32).0[0], 255, "left half should be red");
        assert_eq!(img.get_pixel(54, 32).0[3], 0, "right half should be empty");
    }

    /// The determinism guarantee the whole build reproducibility story rests
    /// on: same input, byte-identical output.
    #[test]
    fn rasterization_is_deterministic() {
        let a = render_svg(HALF, 128, false).unwrap();
        let b = render_svg(HALF, 128, false).unwrap();
        assert_eq!(a, b, "same SVG + size must produce identical bytes");
    }

    #[test]
    fn wrap_brings_overhanging_art_back_on_the_far_edge() {
        let plain = decode(&render_svg(OVERHANG, 64, false).unwrap());
        let wrapped = decode(&render_svg(OVERHANG, 64, true).unwrap());

        // Without wrap the right edge is empty; with wrap the clipped half of
        // the circle reappears there.
        assert_eq!(plain.get_pixel(63, 32).0[3], 0, "no wrap => right edge empty");
        assert!(
            wrapped.get_pixel(63, 32).0[3] > 0,
            "wrap => clipped art reappears on the opposite edge"
        );
        // The left edge is unchanged either way.
        assert!(plain.get_pixel(0, 32).0[1] > 0);
        assert!(wrapped.get_pixel(0, 32).0[1] > 0);
    }

    /// Wrapping must be a no-op for art that stays inside the viewBox, so it
    /// is safe to enable on any tile without changing its appearance.
    #[test]
    fn wrap_is_a_no_op_for_contained_art() {
        let plain = render_svg(HALF, 64, false).unwrap();
        let wrapped = render_svg(HALF, 64, true).unwrap();
        assert_eq!(plain, wrapped);
    }

    #[test]
    fn rejects_a_malformed_svg() {
        assert!(render_svg(b"not an svg at all", 64, false).is_err());
    }

    #[test]
    fn is_svg_is_case_insensitive_and_extension_based() {
        assert!(is_svg(Path::new("a/b/tile.svg")));
        assert!(is_svg(Path::new("TILE.SVG")));
        assert!(!is_svg(Path::new("tile.png")));
        assert!(!is_svg(Path::new("svg")));
    }

    #[test]
    fn raster_paths_separate_sizes_and_wrap_modes() {
        let p = Path::new("tile.svg");
        let a = raster_path(p, 512, false);
        let b = raster_path(p, 1024, false);
        let c = raster_path(p, 512, true);
        assert_ne!(a, b, "different sizes must not share a dedup key");
        assert_ne!(a, c, "different wrap modes must not share a dedup key");
        // Extension-based MIME inference must still see a PNG.
        assert_eq!(a.extension().unwrap(), "png");
        // No vector extension may survive into the name the exporter derives
        // from this path.
        assert!(
            !a.to_string_lossy().contains(".svg"),
            "raster path must not carry the source's .svg extension: {}",
            a.display()
        );
    }

    #[test]
    fn resolve_svg_size_uses_the_default_when_unset() {
        let mat = Material::new("m");
        assert_eq!(resolve_svg_size(&mat).unwrap(), DEFAULT_SVG_SIZE);
    }

    #[test]
    fn resolve_svg_size_rejects_zero() {
        let mut mat = Material::new("m");
        mat.texture_size = Some(0);
        let err = resolve_svg_size(&mat).unwrap_err();
        assert!(format!("{err:#}").contains("texture_size"));
    }

    #[test]
    fn resolve_svg_size_rejects_above_the_max() {
        let mut mat = Material::new("blowup");
        mat.texture_size = Some(MAX_SVG_SIZE + 1);
        let err = resolve_svg_size(&mat).unwrap_err();
        let msg = format!("{err:#}");
        // Names the offending material so a multi-material scene's error is
        // actionable, and the bound itself so the fix is obvious.
        assert!(msg.contains("blowup"), "should name the material: {msg}");
        assert!(
            msg.contains(&MAX_SVG_SIZE.to_string()),
            "should name the bound: {msg}"
        );
    }

    #[test]
    fn resolve_svg_size_accepts_the_max_exactly() {
        let mut mat = Material::new("m");
        mat.texture_size = Some(MAX_SVG_SIZE);
        assert_eq!(resolve_svg_size(&mat).unwrap(), MAX_SVG_SIZE);
    }
}
