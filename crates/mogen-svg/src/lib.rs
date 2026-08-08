//! Rasterize `.svg` texture references to PNG.
//!
//! glTF 2.0 core defines exactly two embeddable image MIME types —
//! `image/png` and `image/jpeg` — so an SVG can never ship as an SVG. Support
//! therefore means rasterizing at build time, which suits `mogen` fine: it is
//! an offline compiler and rasterization is a pure function of
//! `(bytes, size, wrap)`.
//!
//! # Why this is a crate and not a module of `mogen-export`
//!
//! Rasterization began life inside the exporter, as a pre-export pass. That is
//! still where the *pass* belongs — rewriting a [`SceneGraph`]'s texture paths
//! and layering the bytes over a `TextureSource` is an export-shaped problem,
//! and `mogen_export::svg` keeps it.
//!
//! What does not belong there is the renderer itself. Every consumer that
//! holds a `Material` and a path — MoGen Studio's live viewport, and any
//! downstream engine that reads the lowered [`SceneGraph`] and decodes
//! textures itself rather than going through glTF — sits *beside* the
//! exporter, not after it. Studio already reached back into `mogen-export`
//! for exactly these three functions; an engine consuming `mogen-core` +
//! `mogen-dsl` could not reach them at all without taking a dependency on the
//! whole exporter, `oxipng`, `fbxcel` and `meshopt` included.
//!
//! So the policy lives at the bottom, where anything can call it, and the pass
//! stays at the top, where only the exporter needs it. There is still exactly
//! one implementation of "what pixels does this SVG produce" — the point of
//! the original decision — it is simply reachable now.
//!
//! # Dependencies
//!
//! `resvg` and `usvg` only, both pure Rust, so this crate cross-compiles to
//! `wasm32-unknown-unknown` where `image`/`oxipng` cannot. `usvg`'s default
//! features are off deliberately: they pull in `fontdb`, which enumerates
//! system fonts at render time and would make output depend on which machine
//! ran the build. Text in an SVG must be converted to paths first.

use std::path::Path;

use anyhow::{bail, Context, Result};

use mogen_core::{Material, DEFAULT_SVG_SIZE};

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

/// Resolve a material's `texture_size` to the pixel edge length its `.svg`
/// slots rasterize to, applying [`DEFAULT_SVG_SIZE`] when unset and rejecting
/// anything outside `1..=MAX_SVG_SIZE`.
///
/// Every consumer of a raw `Material` must resolve the size through here, or
/// the same SVG rasterizes at two different resolutions depending on which
/// component asked.
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
/// The nine draws composite source-over, which makes the result exactly what
/// painting the art across an infinite plane of adjacent tiles would give.
/// That is the right model, and it is why the join is continuous — but it does
/// mean a *translucent* shape wider than the whole tile overlaps itself, and
/// so reads darker in the overhang band at each edge (measurably: a 50%-opaque
/// band drawn from -10 to 110 in a 0..100 viewBox comes out 75% opaque in the
/// outer 10%). The two bands abut across the join, so the tiling stays
/// seamless in the strict sense; they are still a visible frame. Art that
/// stays inside the viewBox is unaffected — `wrap_is_a_no_op_for_contained_art`
/// pins that — and overhang narrower than the tile only ever lands on the
/// opposite edge, where the centre cell drew nothing.
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

    /// The overlap the source-over lattice implies, pinned so it can't drift
    /// into an asymmetry: a translucent shape wider than the tile darkens by
    /// the same amount at *both* edges, which is what keeps the join
    /// continuous even though the band is visible.
    #[test]
    fn wrap_overlap_is_symmetric_across_the_join() {
        const BAND: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
            <rect x="-10" y="0" width="120" height="100" fill="#ff0000" fill-opacity="0.5"/>
        </svg>"##;
        let img = decode(&render_svg(BAND, 64, true).unwrap());
        let alpha = |x| img.get_pixel(x, 32).0[3];
        assert_eq!(
            alpha(0),
            alpha(63),
            "overhang bands must match across the join or the tile is not seamless"
        );
        assert!(
            alpha(0) > alpha(32),
            "self-overlap in the overhang band is expected; see render_svg's docs"
        );
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
