//! GL texture cache shared across batches. PNGs are decoded once and
//! re-uploaded only when the source file's mtime advances — regenerating
//! a texture (`Generate Textures` etc.) becomes visible without a manual
//! refresh button.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use glow::HasContext;

use super::super::gl_util::try_load_texture;
use super::Renderer;

/// One entry in [`Renderer::texture_cache`].
pub(super) struct CachedTexture {
    /// File mtime captured at load time. Used to detect on-disk changes —
    /// regenerated PNGs will have a newer mtime, which forces a reload.
    pub(super) mtime: Option<SystemTime>,
    /// The uploaded GL texture, or `None` if loading failed.
    pub(super) texture: Option<glow::Texture>,
}

impl Renderer {
    /// Look up a texture in the cache, decoding (or, for `.svg`, rasterizing)
    /// and uploading on the first miss. `srgb` selects the GPU storage
    /// format: albedo and emissive maps need sRGB so the GL pipeline
    /// linearises them on read; metallic-roughness, normal, and AO maps
    /// store data, not colour, and must be linear. Re-loads when the file's
    /// mtime changes so regenerated PNGs (e.g. after `Generate Textures`)
    /// become visible without restarting. `svg_size`/`svg_wrap` are the
    /// source material's `texture_size`/`texture_wrap` and only affect
    /// `.svg` paths — included in the cache key so the same SVG referenced
    /// at two resolutions (e.g. two materials sharing one file) doesn't
    /// serve one material's raster to the other. Returns `None` for paths
    /// that fail to load — the caller falls back to the corresponding
    /// scalar uniform.
    pub(super) fn ensure_texture(
        &mut self,
        gl: &glow::Context,
        path: &Path,
        srgb: bool,
        svg_size: Option<u32>,
        svg_wrap: bool,
    ) -> Option<glow::Texture> {
        let mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok();
        let key = (path.to_path_buf(), srgb, svg_size, svg_wrap);

        if let Some(cached) = self.texture_cache.get(&key) {
            if cached.mtime == mtime {
                return cached.texture;
            }
            // mtime moved — drop the stale GL texture before reloading.
            if let Some(old) = cached.texture {
                unsafe { gl.delete_texture(old) };
            }
        }

        let texture = match unsafe { try_load_texture(gl, path, srgb, svg_size, svg_wrap) } {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("viewer: texture load failed for {}: {e}", path.display());
                None
            }
        };
        self.texture_cache
            .insert(key, CachedTexture { mtime, texture });
        texture
    }

    /// Drop GL textures whose source paths are no longer referenced by any
    /// active batch. Keeps memory usage bounded across scene reloads where
    /// the user has swapped texture files.
    pub(in crate::viewer) fn evict_unused_textures(&mut self, gl: &glow::Context) {
        let mut alive: HashSet<&PathBuf> = HashSet::new();
        for b in &self.batches {
            for slot in [
                &b.base_color_texture,
                &b.metallic_roughness_texture,
                &b.normal_texture,
                &b.occlusion_texture,
                &b.emissive_texture,
            ] {
                if let Some(p) = slot {
                    alive.insert(p);
                }
            }
        }
        let stale: Vec<TextureCacheKey> = self
            .texture_cache
            .keys()
            .filter(|(p, ..)| !alive.contains(p))
            .cloned()
            .collect();
        for k in stale {
            if let Some(entry) = self.texture_cache.remove(&k) {
                if let Some(tex) = entry.texture {
                    unsafe { gl.delete_texture(tex) };
                }
            }
        }
    }
}

/// `(path, srgb, svg_size, svg_wrap)`. The last two are inert for raster
/// textures but must be part of the key so one `.svg` shared by two
/// materials at different `texture_size`s gets two cached uploads, not a
/// clobbered one.
pub(super) type TextureCacheKey = (PathBuf, bool, Option<u32>, bool);
pub(super) type TextureCache = HashMap<TextureCacheKey, CachedTexture>;
