use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
#[cfg(feature = "textures")]
use serde_json::json;

use mogen_core::{Material, TextureRef};

#[cfg(feature = "textures")]
use crate::align_up;
use crate::BufferView;

/// Where the exporter pulls texture bytes from. Desktop builds use
/// [`FsTextureSource`] (reads from the filesystem); wasm builds plug in an
/// in-memory map populated by the JS caller. The split keeps `mogen-export`
/// portable to environments without `std::fs` access.
pub trait TextureSource {
    fn read(&self, path: &Path) -> Result<Vec<u8>>;
}

/// Reads textures off the filesystem via `std::fs::read`. Always available so
/// the desktop CLI / Studio path stays a one-liner; on wasm32 it compiles
/// fine but every read returns an `Unsupported` error — wasm callers
/// construct a [`MapTextureSource`] (or their own impl) instead.
pub struct FsTextureSource;

impl TextureSource for FsTextureSource {
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        std::fs::read(path)
            .with_context(|| format!("reading texture file {}", path.display()))
    }
}

/// In-memory `path → bytes` map. The wasm bridge populates this from the
/// `Map<string, Uint8Array>` of binary assets the JS caller supplies, so
/// every `texture = "albedo.png"` reference in the `.mog` source resolves
/// to bytes the host already has in scope.
pub struct MapTextureSource {
    pub assets: HashMap<PathBuf, Vec<u8>>,
}

impl MapTextureSource {
    pub fn new(assets: HashMap<PathBuf, Vec<u8>>) -> Self {
        Self { assets }
    }
}

impl TextureSource for MapTextureSource {
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        self.assets.get(path).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "texture \"{}\" not present in the asset map (wasm callers \
                 must include every texture path referenced from `.mog` \
                 source under the same key)",
                path.display()
            )
        })
    }
}

/// Whether a texture carries colour data (displayed to a human and safe to
/// store as lossy JPEG) or linear numeric data packed into RGB channels
/// (normals, metallic/roughness, occlusion — JPEG artefacts would corrupt the
/// shaded result, so these must stay lossless PNG).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SlotKind {
    Color,
    Linear,
}

/// Packed image + texture metadata. Keyed by (path, slot kind) so the same
/// file used in two different roles (e.g. albedo and AO — rare but legal)
/// gets two embeds with the encoding appropriate to each role.
#[derive(Default)]
pub(crate) struct TextureTable {
    pub(crate) images: Vec<Value>,
    pub(crate) textures: Vec<Value>,
    pub(crate) samplers: Vec<Value>,
    by_key: HashMap<(PathBuf, SlotKind), usize>,
}

impl TextureTable {
    pub(crate) fn index_of(&self, tex: &Option<TextureRef>, kind: SlotKind) -> Option<usize> {
        let t = tex.as_ref()?;
        self.by_key.get(&(t.path.clone(), kind)).copied()
    }

    #[cfg(test)]
    pub(crate) fn insert_for_test(&mut self, path: PathBuf, kind: SlotKind, idx: usize) {
        self.by_key.insert((path, kind), idx);
    }
}

/// Stub used when the `textures` feature is disabled entirely. Returns an
/// empty table; downstream `emit_material` then omits every `*Texture` slot
/// and the materials export as pure PBR factors.
#[cfg(not(feature = "textures"))]
pub(crate) fn pack_textures(
    _materials: &[Material],
    _bin: &mut Vec<u8>,
    _buffer_views: &mut Vec<BufferView>,
    _source: &dyn TextureSource,
) -> Result<TextureTable> {
    Ok(TextureTable::default())
}

/// Read every unique texture file referenced by materials, encode it for its
/// slot kind, embed the resulting bytes into the BIN chunk, and fill out the
/// glTF image / texture / sampler tables.
///
/// The encoding policy depends on which sub-features are enabled:
///
/// - `textures-optimize` on (desktop default): colour slots transcode to
///   JPEG q=90 when alpha permits, linear slots run through oxipng for
///   lossless shrink. Matches the historic desktop behaviour bit-for-bit.
/// - `textures-optimize` off (wasm): bytes are embedded as-is and the MIME
///   type is inferred from the file extension. No `image` / `oxipng`
///   linkage — keeps the wasm artifact small and avoids C deps that don't
///   cross-compile.
#[cfg(feature = "textures")]
pub(crate) fn pack_textures(
    materials: &[Material],
    bin: &mut Vec<u8>,
    buffer_views: &mut Vec<BufferView>,
    source: &dyn TextureSource,
) -> Result<TextureTable> {
    let mut table = TextureTable::default();

    // Collect every (ref, slot kind) in authored order so indices are stable
    // across rebuilds.
    let mut slots: Vec<(&TextureRef, SlotKind)> = Vec::new();
    for m in materials {
        for (slot, kind) in [
            (&m.base_color_texture, SlotKind::Color),
            (&m.metallic_roughness_texture, SlotKind::Linear),
            (&m.normal_texture, SlotKind::Linear),
            (&m.occlusion_texture, SlotKind::Linear),
            (&m.emissive_texture, SlotKind::Color),
        ] {
            if let Some(t) = slot {
                slots.push((t, kind));
            }
        }
    }
    if slots.is_empty() {
        return Ok(table);
    }

    // One shared sampler: linear mipmapping, linear mag, repeat on both axes.
    // Matches Godot's default texture import and is the right choice for
    // tileable PBR maps. Per-texture sampler overrides can come later.
    table.samplers.push(json!({
        "magFilter": 9729,        // LINEAR
        "minFilter": 9987,        // LINEAR_MIPMAP_LINEAR
        "wrapS": 10497,           // REPEAT
        "wrapT": 10497,
    }));

    for (t, kind) in slots {
        let key = (t.path.clone(), kind);
        if table.by_key.contains_key(&key) {
            continue;
        }
        let raw = source
            .read(&t.path)
            .with_context(|| format!("reading texture {}", t.path.display()))?;
        let (bytes, mime) = encode_for_slot(&raw, &t.path, kind)
            .with_context(|| format!("encoding texture {}", t.path.display()))?;

        let offset = align_up(bin, 4);
        let byte_length = bytes.len();
        bin.extend_from_slice(&bytes);
        buffer_views.push(BufferView {
            buffer: 0,
            byte_offset: offset,
            byte_length,
            target: None,
        });

        let image_idx = table.images.len();
        table.images.push(json!({
            "bufferView": buffer_views.len() - 1,
            "mimeType": mime,
            "name": t.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        }));

        let texture_idx = table.textures.len();
        table.textures.push(json!({
            "source": image_idx,
            "sampler": 0,
        }));
        table.by_key.insert(key, texture_idx);
    }

    Ok(table)
}

/// Pick the embed bytes + MIME for a texture given its source bytes, path,
/// and slot kind. With `textures-optimize` on this re-encodes; without it
/// the bytes pass through and only the extension determines the MIME.
#[cfg(all(feature = "textures", feature = "textures-optimize"))]
fn encode_for_slot(
    source: &[u8],
    _path: &Path,
    kind: SlotKind,
) -> Result<(Vec<u8>, &'static str)> {
    let fmt = image::guess_format(source).context("detecting texture image format")?;
    match kind {
        SlotKind::Color => encode_color_slot(source, fmt),
        SlotKind::Linear => encode_linear_slot(source, fmt),
    }
}

/// Pass-through path: embed the source bytes as-is and pick the MIME type
/// from the file extension. PNG, JPEG, and WebP are recognised; anything
/// else is rejected so the GLB stays valid (glTF only specifies these
/// three image MIME types).
#[cfg(all(feature = "textures", not(feature = "textures-optimize")))]
fn encode_for_slot(
    source: &[u8],
    path: &Path,
    _kind: SlotKind,
) -> Result<(Vec<u8>, &'static str)> {
    let mime = mime_from_extension(path)?;
    Ok((source.to_vec(), mime))
}

#[cfg(all(feature = "textures", not(feature = "textures-optimize")))]
fn mime_from_extension(path: &Path) -> Result<&'static str> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") => Ok("image/png"),
        Some("jpg") | Some("jpeg") => Ok("image/jpeg"),
        Some("webp") => Ok("image/webp"),
        _ => anyhow::bail!(
            "unsupported texture extension on {} \
             (only .png/.jpg/.jpeg/.webp are embeddable without re-encode)",
            path.display()
        ),
    }
}

#[cfg(all(feature = "textures", feature = "textures-optimize"))]
fn encode_color_slot(source: &[u8], fmt: image::ImageFormat) -> Result<(Vec<u8>, &'static str)> {
    let img = image::load_from_memory_with_format(source, fmt)
        .context("decoding colour texture")?;

    // A source with an alpha channel only forces PNG if alpha is actually
    // used — a fully-opaque RGBA image transcodes to JPEG just as happily as
    // an RGB one. Scanning the buffer is O(pixels) but this runs once per
    // unique texture at build time, not per frame.
    let keep_alpha = img.color().has_alpha()
        && img.to_rgba8().pixels().any(|p| p.0[3] < 255);

    if keep_alpha {
        // Re-encode to PNG so oxipng sees a canonical source, then optimize.
        let mut canonical = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut canonical), image::ImageFormat::Png)
            .context("re-encoding alpha PNG")?;
        Ok(optimize_png_bytes(&canonical))
    } else {
        let rgb = img.to_rgb8();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
            .context("encoding JPEG")?;
        Ok((out, "image/jpeg"))
    }
}

#[cfg(all(feature = "textures", feature = "textures-optimize"))]
fn encode_linear_slot(source: &[u8], fmt: image::ImageFormat) -> Result<(Vec<u8>, &'static str)> {
    match fmt {
        image::ImageFormat::Png => Ok(optimize_png_bytes(source)),
        image::ImageFormat::Jpeg => Ok((source.to_vec(), "image/jpeg")),
        other => anyhow::bail!(
            "unsupported texture format {:?} for linear slot (expected PNG or JPEG)",
            other
        ),
    }
}

/// Run oxipng over a PNG byte buffer. Preset 2 is a good balance of speed vs
/// size — it re-picks per-row filters and re-deflates, typically 10–30%
/// smaller on un-optimized generator output. If oxipng fails or produces a
/// larger file, keep the original.
#[cfg(all(feature = "textures", feature = "textures-optimize"))]
fn optimize_png_bytes(bytes: &[u8]) -> (Vec<u8>, &'static str) {
    let opts = oxipng::Options::from_preset(2);
    match oxipng::optimize_from_memory(bytes, &opts) {
        Ok(opt) if opt.len() < bytes.len() => (opt, "image/png"),
        _ => (bytes.to_vec(), "image/png"),
    }
}
