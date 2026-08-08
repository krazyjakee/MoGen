//! Rewrite `.svg` texture references to rasterized PNG before anything
//! downstream looks at them.
//!
//! The renderer itself lives in [`mogen_svg`]; this module is the *pass* that
//! applies it to a whole [`SceneGraph`] on the way into an exporter. The two
//! were one module until the renderer had a second caller that was not
//! downstream of export at all — see that crate's docs for why the policy
//! moved down and the pass stayed here.
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
//! | PBR map derivation | `mogen-llm::pbr_maps`, which hard-codes `ImageFormat::Png` |
//! | imposter bake | already-baked pixels; unaffected |
//!
//! Branching at decode time would need three separate fixes. Rewriting the
//! path *before* export means every one of them only ever sees a PNG, and the
//! derived-PBR pipeline picks up SVG albedo support for free.
//!
//! # The consumers this pass cannot reach
//!
//! Studio's live viewport is not downstream of the exporter at all — it
//! flattens the `SceneGraph` straight out of the compile pipeline and uploads
//! each material's texture paths to GL itself. The rewritten paths this pass
//! produces are synthetic and resolve only through [`OverlayTextureSource`],
//! so handing them to a loader that reads from disk would be worse than
//! useless. The same is true of any engine that consumes the lowered
//! `SceneGraph` and decodes textures itself.
//!
//! Both therefore call [`mogen_svg::render_svg`] and
//! [`mogen_svg::resolve_svg_size`] directly, on the real `.svg` path — same
//! functions, same pixels, no second implementation of the policy.
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

use anyhow::{Context, Result};

use mogen_core::{Material, SceneGraph, TextureRef};

use crate::texture::TextureSource;

pub use mogen_svg::{is_svg, render_svg, resolve_svg_size, MAX_SVG_SIZE};

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
        // `texture_size` is documented as ignored by raster slots, so its range
        // check belongs *behind* this test rather than in front of it. Ahead of
        // it, whether an out-of-range value on a texture-less material failed
        // the build depended on whether some entirely unrelated material in the
        // same scene happened to reference an SVG.
        if !material_references_svg(mat) {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A 100x100 viewBox with a red square covering the left half.
    const HALF: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
        <rect x="0" y="0" width="50" height="100" fill="#ff0000"/>
    </svg>"##;

    /// `texture_size` is documented as ignored by raster slots. It must stay
    /// ignored even when a *different* material in the same scene pulls the
    /// SVG pass into play — that coupling is invisible from the source.
    #[test]
    fn texture_size_is_unchecked_on_materials_without_an_svg() {
        let mut scene = SceneGraph::new();
        let mut raster = Material::new("raster");
        raster.base_color_texture = Some(TextureRef::new(PathBuf::from("flat.png")));
        raster.texture_size = Some(MAX_SVG_SIZE + 1);
        let mut vector = Material::new("vector");
        vector.base_color_texture = Some(TextureRef::new(PathBuf::from("tile.svg")));
        scene.materials.push(raster);
        scene.materials.push(vector);

        let source = crate::texture::MapTextureSource::new(
            [(PathBuf::from("tile.svg"), HALF.to_vec())].into_iter().collect(),
        );
        let out = rasterize_svg_textures(&scene, &source)
            .expect("raster-only sizes are not checked")
            .expect("the scene does reference an SVG");
        assert_eq!(
            out.scene.materials[0].base_color_texture.as_ref().unwrap().path,
            PathBuf::from("flat.png"),
            "the raster slot must be left exactly as it was"
        );
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
}
