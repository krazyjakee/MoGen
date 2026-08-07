//! End-to-end coverage of the `.svg` texture path.
//!
//! glTF cannot carry SVG, so support means rasterizing at build time. These
//! tests pin the observable contract: a scene referencing an `.svg` produces a
//! GLB whose embedded image is real raster of the requested size, the
//! rasterization is reproducible, `texture_size` is honoured per material, and
//! nothing about the raster path regressed.

use std::fs;
use std::path::PathBuf;
use std::process::id;

use serde_json::Value;

use mogen_core::{Material, SceneGraph, TextureRef, Transform, UvMode};
use mogen_geom::box_mesh;

/// A 100x100 tile: red left half. Asymmetric so orientation errors show up.
const TILE_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
    <rect x="0" y="0" width="50" height="100" fill="#ff0000"/>
    <rect x="50" y="0" width="50" height="100" fill="#0000ff"/>
</svg>"##;

fn unique_tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("mogen-svg-export-{}-{name}", id()))
}

fn read_glb(bytes: &[u8]) -> (Value, Vec<u8>) {
    assert_eq!(&bytes[0..4], b"glTF", "not a GLB");
    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let js: Value =
        serde_json::from_slice(&bytes[20..20 + json_len]).expect("invalid GLB JSON chunk");
    let bin_start = 20 + json_len;
    let bin_len = u32::from_le_bytes(bytes[bin_start..bin_start + 4].try_into().unwrap()) as usize;
    let bin = bytes[bin_start + 8..bin_start + 8 + bin_len].to_vec();
    (js, bin)
}

fn image_bytes(js: &Value, bin: &[u8], image_idx: usize) -> Vec<u8> {
    let bv_idx = js["images"][image_idx]["bufferView"].as_u64().unwrap() as usize;
    let bv = &js["bufferViews"][bv_idx];
    let offset = bv["byteOffset"].as_u64().unwrap_or(0) as usize;
    let length = bv["byteLength"].as_u64().unwrap() as usize;
    bin[offset..offset + length].to_vec()
}

/// Build a one-box scene whose material carries `mat`, export it, return the GLB.
fn export_with(mat: Material, out_name: &str) -> Vec<u8> {
    let mut scene = SceneGraph::new();
    let mat_id = scene.add_material(mat);
    let node = scene.add_root("box", "box", Transform::IDENTITY);
    scene.set_mesh(node, box_mesh([1.0, 1.0, 1.0], UvMode::default()));
    scene.set_material(node, mat_id);

    let out = unique_tmp(out_name);
    mogen_export::write_glb(&scene, &out).expect("write_glb");
    let bytes = fs::read(&out).expect("reading produced glb");
    let _ = fs::remove_file(&out);
    bytes
}

#[test]
fn svg_albedo_rasterizes_and_embeds_as_raster() {
    let svg_path = unique_tmp("tile.svg");
    fs::write(&svg_path, TILE_SVG).expect("writing svg fixture");

    let mut mat = Material::new("vector");
    mat.base_color_texture = Some(TextureRef::new(svg_path.clone()));
    mat.texture_size = Some(64);
    let bytes = export_with(mat, "svg_albedo.glb");
    let (js, bin) = read_glb(&bytes);

    let images = js["images"].as_array().expect("images[]");
    assert_eq!(images.len(), 1, "one image for the one SVG");

    // The GLB must carry a spec-legal raster MIME — never anything SVG-ish.
    let mime = images[0]["mimeType"].as_str().unwrap();
    assert!(
        mime == "image/png" || mime == "image/jpeg",
        "embedded MIME must be a glTF core image type, got {mime}"
    );

    // And it must be genuinely decodable raster at the size we asked for.
    let embedded = image_bytes(&js, &bin, 0);
    let img = image::load_from_memory(&embedded).expect("embedded image must decode");
    assert_eq!(img.width(), 64, "rasterized to the requested texture_size");
    assert_eq!(img.height(), 64);

    // Material wiring survives the path rewrite.
    assert_eq!(
        js["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["index"],
        0
    );

    // The output must be self-contained: every image resolves to a bufferView,
    // and no trace of the vector source is left anywhere in the glTF JSON —
    // including the image `name`, which the exporter derives from the stem.
    assert!(images[0].get("bufferView").is_some());
    assert!(
        !serde_json::to_string(&js).unwrap().contains(".svg"),
        "no .svg reference may survive into the exported glTF"
    );

    let _ = fs::remove_file(&svg_path);
}

#[test]
fn texture_size_controls_the_raster_resolution() {
    let svg_path = unique_tmp("sized.svg");
    fs::write(&svg_path, TILE_SVG).expect("writing svg fixture");

    for size in [32u32, 256] {
        let mut mat = Material::new("vector");
        mat.base_color_texture = Some(TextureRef::new(svg_path.clone()));
        mat.texture_size = Some(size);
        let bytes = export_with(mat, &format!("sized{size}.glb"));
        let (js, bin) = read_glb(&bytes);
        let img = image::load_from_memory(&image_bytes(&js, &bin, 0)).expect("decodes");
        assert_eq!(img.width(), size, "texture_size = {size} should be honoured");
    }

    let _ = fs::remove_file(&svg_path);
}

/// The default must apply when the material says nothing — and it must be the
/// documented constant, not an accident of the renderer.
#[test]
fn omitting_texture_size_uses_the_documented_default() {
    let svg_path = unique_tmp("default.svg");
    fs::write(&svg_path, TILE_SVG).expect("writing svg fixture");

    let mut mat = Material::new("vector");
    mat.base_color_texture = Some(TextureRef::new(svg_path.clone()));
    let bytes = export_with(mat, "default.glb");
    let (js, bin) = read_glb(&bytes);
    let img = image::load_from_memory(&image_bytes(&js, &bin, 0)).expect("decodes");
    assert_eq!(img.width(), mogen_core::DEFAULT_SVG_SIZE);

    let _ = fs::remove_file(&svg_path);
}

/// Build reproducibility: the same scene must export byte-identically. This is
/// the guarantee that lets rasterization stay uncached.
#[test]
fn svg_export_is_reproducible() {
    let svg_path = unique_tmp("repro.svg");
    fs::write(&svg_path, TILE_SVG).expect("writing svg fixture");

    let build = || {
        let mut mat = Material::new("vector");
        mat.base_color_texture = Some(TextureRef::new(svg_path.clone()));
        mat.texture_size = Some(64);
        export_with(mat, "repro.glb")
    };
    assert_eq!(build(), build(), "same scene must export byte-identically");

    let _ = fs::remove_file(&svg_path);
}

/// Two materials sharing one SVG at one size should render it once and dedupe
/// to a single embedded image, exactly as two materials sharing a PNG do.
#[test]
fn shared_svg_dedupes_to_one_image() {
    let svg_path = unique_tmp("shared.svg");
    fs::write(&svg_path, TILE_SVG).expect("writing svg fixture");

    let mut scene = SceneGraph::new();
    for name in ["a", "b"] {
        let mut m = Material::new(name);
        m.base_color_texture = Some(TextureRef::new(svg_path.clone()));
        m.texture_size = Some(32);
        let id = scene.add_material(m);
        let n = scene.add_root(name, "box", Transform::IDENTITY);
        scene.set_mesh(n, box_mesh([1.0, 1.0, 1.0], UvMode::default()));
        scene.set_material(n, id);
    }

    let out = unique_tmp("shared.glb");
    mogen_export::write_glb(&scene, &out).expect("write_glb");
    let bytes = fs::read(&out).expect("reading glb");
    let (js, _) = read_glb(&bytes);
    assert_eq!(
        js["images"].as_array().unwrap().len(),
        1,
        "one SVG at one size should rasterize and embed once"
    );

    let _ = fs::remove_file(&svg_path);
    let _ = fs::remove_file(&out);
}

/// The same SVG at two different sizes must *not* collide in the exporter's
/// dedup map — otherwise one material silently gets the other's resolution.
#[test]
fn same_svg_at_two_sizes_produces_two_images() {
    let svg_path = unique_tmp("twosize.svg");
    fs::write(&svg_path, TILE_SVG).expect("writing svg fixture");

    let mut scene = SceneGraph::new();
    for (name, size) in [("small", 32u32), ("large", 128)] {
        let mut m = Material::new(name);
        m.base_color_texture = Some(TextureRef::new(svg_path.clone()));
        m.texture_size = Some(size);
        let id = scene.add_material(m);
        let n = scene.add_root(name, "box", Transform::IDENTITY);
        scene.set_mesh(n, box_mesh([1.0, 1.0, 1.0], UvMode::default()));
        scene.set_material(n, id);
    }

    let out = unique_tmp("twosize.glb");
    mogen_export::write_glb(&scene, &out).expect("write_glb");
    let bytes = fs::read(&out).expect("reading glb");
    let (js, bin) = read_glb(&bytes);

    let images = js["images"].as_array().unwrap();
    assert_eq!(images.len(), 2, "distinct sizes must not share one embed");
    let mut widths: Vec<u32> = (0..2)
        .map(|i| image::load_from_memory(&image_bytes(&js, &bin, i)).unwrap().width())
        .collect();
    widths.sort();
    assert_eq!(widths, vec![32, 128]);

    let _ = fs::remove_file(&svg_path);
    let _ = fs::remove_file(&out);
}

/// A scene with no SVG at all must be completely unaffected by the pass.
#[test]
fn raster_only_scenes_are_untouched() {
    let png_path = unique_tmp("plain.png");
    let img: image::RgbImage = image::ImageBuffer::from_pixel(4, 4, image::Rgb([10, 20, 30]));
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encoding png fixture");
    fs::write(&png_path, &png).expect("writing png fixture");

    let mut mat = Material::new("raster");
    mat.base_color_texture = Some(TextureRef::new(png_path.clone()));
    let bytes = export_with(mat, "raster.glb");
    let (js, _) = read_glb(&bytes);
    assert_eq!(js["images"].as_array().unwrap().len(), 1);
    assert_eq!(
        js["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["index"],
        0
    );

    let _ = fs::remove_file(&png_path);
}

/// A `texture_size` past the exporter's cap must fail the build with a
/// message naming the material and the bound, not silently clamp or OOM
/// trying to allocate the requested pixmap.
#[test]
fn oversized_texture_size_fails_the_build_with_a_useful_message() {
    let svg_path = unique_tmp("oversized.svg");
    fs::write(&svg_path, TILE_SVG).expect("writing svg fixture");

    let mut mat = Material::new("blowup");
    mat.base_color_texture = Some(TextureRef::new(svg_path.clone()));
    mat.texture_size = Some(mogen_export::MAX_SVG_SIZE + 1);

    let mut scene = SceneGraph::new();
    let mat_id = scene.add_material(mat);
    let n = scene.add_root("box", "box", Transform::IDENTITY);
    scene.set_mesh(n, box_mesh([1.0, 1.0, 1.0], UvMode::default()));
    scene.set_material(n, mat_id);

    let out = unique_tmp("oversized.glb");
    let err = mogen_export::write_glb(&scene, &out).expect_err("oversized texture_size must fail");
    let msg = format!("{err:#}");
    assert!(msg.contains("blowup"), "error should name the material, got: {msg}");
    assert!(
        msg.contains("texture_size"),
        "error should name the attribute, got: {msg}"
    );

    let _ = fs::remove_file(&svg_path);
    let _ = fs::remove_file(&out);
}

/// `bundle_lods_and_imposter`'s bake reads texture files straight off disk
/// through a separate GL renderer that never sees the SVG pre-pass's
/// in-memory raster — combining the two must fail fast with an explanatory
/// message instead of a confusing "file not found" once the bake reaches the
/// synthetic raster path. No display required: the guard fires before the GL
/// bake would even start.
#[test]
#[cfg(feature = "imposter")]
fn svg_with_bundled_imposter_fails_with_a_clear_message() {
    let svg_path = unique_tmp("imposter.svg");
    fs::write(&svg_path, TILE_SVG).expect("writing svg fixture");

    let mut mat = Material::new("vector");
    mat.base_color_texture = Some(TextureRef::new(svg_path.clone()));

    let mut scene = SceneGraph::new();
    let mat_id = scene.add_material(mat);
    let n = scene.add_root("box", "box", Transform::IDENTITY);
    scene.set_mesh(n, box_mesh([1.0, 1.0, 1.0], UvMode::default()));
    scene.set_material(n, mat_id);

    let opts = mogen_export::ExportOptions {
        bundle_lods_and_imposter: true,
        ..Default::default()
    };
    let err = mogen_export::build_glb_with_options(&scene, &opts, |_| {})
        .expect_err("svg + bundle_lods_and_imposter must fail, not silently mis-bake");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("bundle_lods_and_imposter") && msg.contains("svg"),
        "error should name both the option and the format, got: {msg}"
    );

    let _ = fs::remove_file(&svg_path);
}

/// A broken SVG must fail the build with a message naming the file, not
/// silently export a material with no texture.
#[test]
fn malformed_svg_fails_the_build_with_a_useful_message() {
    let svg_path = unique_tmp("broken.svg");
    fs::write(&svg_path, b"<not-an-svg").expect("writing broken fixture");

    let mut scene = SceneGraph::new();
    let mut mat = Material::new("bad");
    mat.base_color_texture = Some(TextureRef::new(svg_path.clone()));
    let mat_id = scene.add_material(mat);
    let n = scene.add_root("box", "box", Transform::IDENTITY);
    scene.set_mesh(n, box_mesh([1.0, 1.0, 1.0], UvMode::default()));
    scene.set_material(n, mat_id);

    let out = unique_tmp("broken.glb");
    let err = mogen_export::write_glb(&scene, &out).expect_err("malformed SVG must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("broken.svg"),
        "error should name the offending file, got: {msg}"
    );

    let _ = fs::remove_file(&svg_path);
    let _ = fs::remove_file(&out);
}
