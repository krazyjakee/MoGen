//! Derive normal / metallic-roughness / occlusion PNGs from an albedo PNG
//! without calling the LLM.
//!
//! The albedo is generated separately (`textures.rs`); these maps are pure
//! image-processing functions over its bytes. Designed so a `.mog` file gets a
//! complete PBR set even when the model can only synthesise base color.
//!
//! # Algorithm overview
//!
//! All three maps start from the albedo's per-pixel luminance treated as a
//! height field. Sampling wraps in both axes so the outputs tile alongside the
//! source.
//!
//! - **Normal** — Sobel gradient on the height field, encoded into RGB as a
//!   tangent-space normal (`0.5 + 0.5 * n`). The Z component carries
//!   `1 / strength`, so larger strength values flatten the slope before
//!   normalisation and produce more pronounced bumps.
//! - **Metallic-roughness** — packs roughness into G and the material's
//!   authored metallic into a flat B. Roughness is `base + gain * stddev`,
//!   where stddev is the local 3×3 luminance standard deviation (high-detail
//!   areas read as rougher). R is left at zero per glTF convention.
//! - **Occlusion** — cavity approximation: pixels darker than a wide
//!   box-blurred mean of luminance are darkened in R; brighter pixels stay at
//!   1.0. Cheap, tileable, and matches what a renderer would expect in
//!   `occlusionTexture.R`.

use anyhow::{anyhow, Context, Result};
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::{ExtendedColorType, ImageBuffer, ImageEncoder, ImageFormat, Luma, Rgb, RgbImage};

/// Knobs for the PBR map derivation. Sensible defaults already cover the
/// common case — only override when a particular material wants different
/// strength or smoothness.
#[derive(Debug, Clone)]
pub struct PbrMapOptions {
    pub generate_normal: bool,
    pub generate_metallic_roughness: bool,
    pub generate_occlusion: bool,
    /// Multiplier applied to the height-field gradient before normalisation.
    /// Larger = more pronounced bumps. Range ~0.5..4.0; 1.0 is neutral.
    pub normal_strength: f32,
    /// Material's authored roughness (0..1), used as the floor of the derived
    /// roughness map.
    pub roughness_base: f32,
    /// How much local detail variance pushes roughness above the base. 0.0
    /// emits a flat roughness map at `roughness_base`.
    pub roughness_variance_gain: f32,
    /// Material's authored metallic (0..1), written flat into the B channel
    /// of the metallic-roughness texture.
    pub metallic: f32,
    /// Radius (in pixels) of the box blur used to estimate the local mean for
    /// AO. Larger = softer, more global occlusion. Clamped to image size.
    pub occlusion_radius: u32,
    /// 0..1 multiplier on how dark the AO map can get. 0 disables darkening,
    /// 1 lets fully-cavity pixels reach black.
    pub occlusion_strength: f32,
}

impl Default for PbrMapOptions {
    fn default() -> Self {
        Self {
            generate_normal: true,
            generate_metallic_roughness: true,
            generate_occlusion: true,
            normal_strength: 1.5,
            roughness_base: 0.85,
            roughness_variance_gain: 0.6,
            metallic: 0.0,
            occlusion_radius: 16,
            occlusion_strength: 0.7,
        }
    }
}

/// PNG byte buffers for whichever maps were requested. Slots are `None` when
/// the matching `generate_*` flag was off.
#[derive(Debug, Clone, Default)]
pub struct GeneratedPbrMaps {
    pub normal_png: Option<Vec<u8>>,
    pub metallic_roughness_png: Option<Vec<u8>>,
    pub occlusion_png: Option<Vec<u8>>,
}

/// Decode the albedo, derive the requested maps, and re-encode each as PNG.
pub fn derive_pbr_maps(albedo_png: &[u8], opts: &PbrMapOptions) -> Result<GeneratedPbrMaps> {
    let img = image::load_from_memory_with_format(albedo_png, ImageFormat::Png)
        .context("decoding albedo PNG")?
        .to_rgb8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(anyhow!("albedo image is empty"));
    }

    let height = luminance_field(&img);

    let mut out = GeneratedPbrMaps::default();
    if opts.generate_normal {
        let n = sobel_normal_map(&height, w, h, opts.normal_strength);
        out.normal_png = Some(encode_png_rgb(&n)?);
    }
    if opts.generate_metallic_roughness {
        let mr = metallic_roughness_map(
            &height,
            w,
            h,
            opts.roughness_base,
            opts.roughness_variance_gain,
            opts.metallic,
        );
        out.metallic_roughness_png = Some(encode_png_rgb(&mr)?);
    }
    if opts.generate_occlusion {
        let ao = occlusion_map(&height, w, h, opts.occlusion_radius, opts.occlusion_strength);
        out.occlusion_png = Some(encode_png_luma(&ao)?);
    }
    Ok(out)
}

// --- intermediate fields ---------------------------------------------------

/// Rec.601 luminance of every pixel as `f32` in `0..1`. Stored in row-major
/// order — `luma[y * w + x]`.
fn luminance_field(img: &RgbImage) -> Vec<f32> {
    let (w, h) = img.dimensions();
    let mut out = Vec::with_capacity((w * h) as usize);
    for p in img.pixels() {
        let [r, g, b] = p.0;
        let l = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        out.push(l / 255.0);
    }
    out
}

#[inline]
fn wrap(i: i32, n: i32) -> usize {
    // Euclidean modulo so negative indices wrap correctly.
    let r = i.rem_euclid(n);
    r as usize
}

#[inline]
fn at(field: &[f32], w: u32, x: i32, y: i32) -> f32 {
    let h = (field.len() as u32 / w) as i32;
    let xi = wrap(x, w as i32);
    let yi = wrap(y, h);
    field[yi * w as usize + xi]
}

// --- normal map ------------------------------------------------------------

fn sobel_normal_map(height: &[f32], w: u32, h: u32, strength: f32) -> RgbImage {
    let mut img = ImageBuffer::new(w, h);
    // Guard against degenerate strength values; treat <= 0 as "flat".
    let z = if strength.is_finite() && strength > 1e-4 {
        1.0 / strength
    } else {
        1.0
    };
    for y in 0..h {
        for x in 0..w {
            let xi = x as i32;
            let yi = y as i32;
            // Sobel kernels on the height field, sampled with wrap so the
            // resulting normal map tiles seamlessly with the albedo.
            let tl = at(height, w, xi - 1, yi - 1);
            let t  = at(height, w, xi,     yi - 1);
            let tr = at(height, w, xi + 1, yi - 1);
            let l  = at(height, w, xi - 1, yi);
            let r  = at(height, w, xi + 1, yi);
            let bl = at(height, w, xi - 1, yi + 1);
            let b  = at(height, w, xi,     yi + 1);
            let br = at(height, w, xi + 1, yi + 1);

            let dx = (tr + 2.0 * r + br) - (tl + 2.0 * l + bl);
            let dy = (bl + 2.0 * b + br) - (tl + 2.0 * t + tr);

            // Negate gradients so brighter (taller) regions push the normal
            // toward the viewer's hemisphere — standard "white = up" mapping.
            let nx = -dx;
            let ny = -dy;
            let nz = z;
            let inv_len = 1.0 / (nx * nx + ny * ny + nz * nz).sqrt();
            let nx = nx * inv_len;
            let ny = ny * inv_len;
            let nz = nz * inv_len;

            img.put_pixel(
                x,
                y,
                Rgb([encode_unit(nx), encode_unit(ny), encode_unit(nz)]),
            );
        }
    }
    img
}

#[inline]
fn encode_unit(n: f32) -> u8 {
    let v = (n * 0.5 + 0.5).clamp(0.0, 1.0);
    (v * 255.0).round() as u8
}

// --- metallic-roughness ----------------------------------------------------

fn metallic_roughness_map(
    height: &[f32],
    w: u32,
    h: u32,
    base: f32,
    variance_gain: f32,
    metallic: f32,
) -> RgbImage {
    let mut img = ImageBuffer::new(w, h);
    let metallic_byte = (metallic.clamp(0.0, 1.0) * 255.0).round() as u8;
    for y in 0..h {
        for x in 0..w {
            let stddev = local_stddev_3x3(height, w, x as i32, y as i32);
            let r = (base + variance_gain * stddev).clamp(0.0, 1.0);
            // glTF convention: R is unused (or AO), G = roughness, B = metallic.
            img.put_pixel(x, y, Rgb([0, (r * 255.0).round() as u8, metallic_byte]));
        }
    }
    img
}

fn local_stddev_3x3(height: &[f32], w: u32, x: i32, y: i32) -> f32 {
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let v = at(height, w, x + dx, y + dy);
            sum += v;
            sum_sq += v * v;
        }
    }
    let n = 9.0;
    let mean = sum / n;
    let var = (sum_sq / n - mean * mean).max(0.0);
    var.sqrt()
}

// --- occlusion -------------------------------------------------------------

fn occlusion_map(height: &[f32], w: u32, h: u32, radius: u32, strength: f32) -> ImageBuffer<Luma<u8>, Vec<u8>> {
    let radius = radius.min(w.saturating_sub(1) / 2).min(h.saturating_sub(1) / 2);
    let blurred = box_blur(height, w, h, radius);

    let mut img = ImageBuffer::new(w, h);
    let strength = strength.clamp(0.0, 1.0);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let local = height[i];
            let mean = blurred[i];
            // "How much darker than the local neighbourhood am I?" Negative
            // values (brighter than mean) clamp to 0 — those are highlights,
            // not cavities, and shouldn't darken the AO output.
            let cavity = (mean - local).max(0.0);
            // mean - local is bounded by the [0, 1] luminance range, but in
            // practice it's small. Multiplying by 4 turns subtle differences
            // into a usable AO range without losing tileability.
            let darken = (cavity * 4.0 * strength).min(1.0);
            let ao = (1.0 - darken).clamp(0.0, 1.0);
            img.put_pixel(x, y, Luma([(ao * 255.0).round() as u8]));
        }
    }
    img
}

/// Separable box blur with wrap-around sampling. O(w*h*radius) per pass — at
/// the radii we actually use (≤32) this is well under a millisecond for
/// 1k-square textures, so a fancier summed-area approach isn't worth the
/// arithmetic.
fn box_blur(field: &[f32], w: u32, h: u32, radius: u32) -> Vec<f32> {
    if radius == 0 {
        return field.to_vec();
    }
    let wi = w as i32;
    let hi = h as i32;
    let r = radius as i32;
    let window = (2 * r + 1) as f32;
    let mut tmp = vec![0.0f32; field.len()];

    // Horizontal pass.
    for y in 0..hi {
        let row = (y as usize) * w as usize;
        for x in 0..wi {
            let mut sum = 0.0;
            for dx in -r..=r {
                sum += field[row + wrap(x + dx, wi)];
            }
            tmp[row + x as usize] = sum / window;
        }
    }

    // Vertical pass over the horizontally-blurred buffer.
    let mut out = vec![0.0f32; field.len()];
    for x in 0..wi {
        for y in 0..hi {
            let mut sum = 0.0;
            for dy in -r..=r {
                let yi = wrap(y + dy, hi);
                sum += tmp[yi * w as usize + x as usize];
            }
            out[(y as usize) * w as usize + x as usize] = sum / window;
        }
    }
    out
}

// --- encoding --------------------------------------------------------------

/// Encode an 8-bit RGB image as PNG with the highest zlib level and adaptive
/// filtering. ~20–30% smaller than the `image` crate's default `Fast` preset
/// at the cost of a slower encode — worth it because the PNG bytes are
/// embedded into the GLB.
fn encode_png_rgb(img: &RgbImage) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive)
        .write_image(img.as_raw(), img.width(), img.height(), ExtendedColorType::Rgb8)
        .context("encoding RGB PNG")?;
    Ok(buf)
}

fn encode_png_luma(img: &ImageBuffer<Luma<u8>, Vec<u8>>) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive)
        .write_image(img.as_raw(), img.width(), img.height(), ExtendedColorType::L8)
        .context("encoding luma PNG")?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbImage};
    use std::io::Cursor;

    fn flat_albedo(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut img = RgbImage::new(w, h);
        for p in img.pixels_mut() {
            *p = Rgb(rgb);
        }
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .unwrap();
        buf
    }

    fn checker_albedo(w: u32, h: u32) -> Vec<u8> {
        let mut img = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let on = ((x / 4) + (y / 4)) % 2 == 0;
                let v = if on { 230 } else { 25 };
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .unwrap();
        buf
    }

    fn decode_rgb(png: &[u8]) -> RgbImage {
        image::load_from_memory_with_format(png, ImageFormat::Png)
            .unwrap()
            .to_rgb8()
    }

    fn decode_luma(png: &[u8]) -> ImageBuffer<Luma<u8>, Vec<u8>> {
        image::load_from_memory_with_format(png, ImageFormat::Png)
            .unwrap()
            .to_luma8()
    }

    #[test]
    fn flat_input_yields_flat_normals() {
        let png = flat_albedo(16, 16, [128, 128, 128]);
        let maps = derive_pbr_maps(&png, &PbrMapOptions::default()).unwrap();
        let n = decode_rgb(maps.normal_png.as_ref().unwrap());
        // A flat height field has zero gradient — the normal everywhere should
        // be (0,0,1), encoded as (128,128,255) within rounding.
        for p in n.pixels() {
            assert!((p.0[0] as i32 - 128).abs() <= 2);
            assert!((p.0[1] as i32 - 128).abs() <= 2);
            assert!(p.0[2] >= 250);
        }
    }

    #[test]
    fn checker_input_produces_nonflat_normals() {
        let png = checker_albedo(16, 16);
        let maps = derive_pbr_maps(&png, &PbrMapOptions::default()).unwrap();
        let n = decode_rgb(maps.normal_png.as_ref().unwrap());
        let mut nonflat = 0usize;
        for p in n.pixels() {
            if (p.0[0] as i32 - 128).abs() > 8 || (p.0[1] as i32 - 128).abs() > 8 {
                nonflat += 1;
            }
        }
        assert!(nonflat > 0, "expected at least some sloped pixels along the checker edges");
    }

    #[test]
    fn metallic_roughness_packs_metallic_into_blue() {
        let png = flat_albedo(8, 8, [200, 200, 200]);
        let opts = PbrMapOptions {
            metallic: 1.0,
            ..PbrMapOptions::default()
        };
        let maps = derive_pbr_maps(&png, &opts).unwrap();
        let mr = decode_rgb(maps.metallic_roughness_png.as_ref().unwrap());
        for p in mr.pixels() {
            assert_eq!(p.0[0], 0, "R should be unused (zero)");
            assert_eq!(p.0[2], 255, "B should encode metallic = 1.0");
        }
    }

    #[test]
    fn roughness_floor_respected_on_flat_input() {
        let png = flat_albedo(8, 8, [100, 100, 100]);
        let opts = PbrMapOptions {
            roughness_base: 0.5,
            roughness_variance_gain: 1.0,
            ..PbrMapOptions::default()
        };
        let maps = derive_pbr_maps(&png, &opts).unwrap();
        let mr = decode_rgb(maps.metallic_roughness_png.as_ref().unwrap());
        // Flat input → zero variance → roughness == base.
        for p in mr.pixels() {
            let g = p.0[1] as i32;
            assert!((g - 128).abs() <= 2, "expected ~128 for base=0.5, got {g}");
        }
    }

    #[test]
    fn occlusion_is_white_on_flat_input() {
        let png = flat_albedo(16, 16, [180, 180, 180]);
        let maps = derive_pbr_maps(&png, &PbrMapOptions::default()).unwrap();
        let ao = decode_luma(maps.occlusion_png.as_ref().unwrap());
        for p in ao.pixels() {
            assert!(p.0[0] >= 250, "flat input should leave AO un-darkened");
        }
    }

    #[test]
    fn flags_disable_individual_outputs() {
        let png = flat_albedo(4, 4, [128, 128, 128]);
        let opts = PbrMapOptions {
            generate_normal: false,
            generate_metallic_roughness: true,
            generate_occlusion: false,
            ..PbrMapOptions::default()
        };
        let maps = derive_pbr_maps(&png, &opts).unwrap();
        assert!(maps.normal_png.is_none());
        assert!(maps.metallic_roughness_png.is_some());
        assert!(maps.occlusion_png.is_none());
    }

    #[test]
    fn empty_input_errors_cleanly() {
        // 1x1 PNG of one pixel — smallest legal input — should still succeed.
        let png = flat_albedo(1, 1, [128, 128, 128]);
        let maps = derive_pbr_maps(&png, &PbrMapOptions::default()).unwrap();
        assert!(maps.normal_png.is_some());

        // Outright garbage bytes should not panic; they should be reported.
        let bad = derive_pbr_maps(b"not a png at all", &PbrMapOptions::default());
        assert!(bad.is_err());
    }
}
