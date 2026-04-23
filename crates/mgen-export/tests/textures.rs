//! End-to-end coverage of the texture export path: DSL-lowered materials with
//! `base_color_texture` pointing at an on-disk PNG should round-trip into a
//! valid GLB with populated `images[]` / `textures[]` / `samplers[]` tables
//! and material references wired to the right indices. Also verifies the
//! per-slot encoding policy: colour slots transcode to JPEG, linear slots
//! stay PNG, and colour-with-alpha stays PNG.

use std::fs;
use std::path::PathBuf;
use std::process::id;

use image::{ImageBuffer, Rgb, Rgba};
use serde_json::Value;

use mgen_core::{Material, Mesh, MaterialId, SceneGraph, TextureRef, Transform};
use mgen_geom::box_mesh;

/// Build a solid-colour RGB PNG at the requested size. Re-encoded fresh by
/// `image` so tests don't depend on a hand-crafted byte blob with correct
/// CRCs — the exporter now decodes every texture before embedding.
fn rgb_png(size: u32, color: [u8; 3]) -> Vec<u8> {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(size, size, Rgb(color));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encoding rgb png fixture");
    out
}

/// Build an RGBA PNG where at least one pixel has alpha < 255, forcing the
/// "meaningful alpha" branch in the exporter.
fn rgba_png_with_transparency(size: u32) -> Vec<u8> {
    let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(size, size, Rgba([255, 255, 255, 255]));
    // One translucent pixel is enough.
    img.put_pixel(0, 0, Rgba([255, 255, 255, 128]));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encoding rgba png fixture");
    out
}

fn unique_tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("mgen-texture-export-{}-{name}", id()))
}

fn read_glb(bytes: &[u8]) -> (Value, Vec<u8>) {
    assert_eq!(&bytes[0..4], b"glTF", "not a GLB");
    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json_bytes = &bytes[20..20 + json_len];
    let js: Value = serde_json::from_slice(json_bytes).expect("invalid GLB JSON chunk");

    // BIN chunk follows: [length: u32][type: u32][data…]
    let bin_start = 20 + json_len;
    let bin_len = u32::from_le_bytes(bytes[bin_start..bin_start + 4].try_into().unwrap()) as usize;
    let bin = bytes[bin_start + 8..bin_start + 8 + bin_len].to_vec();
    (js, bin)
}

fn read_gltf_json(glb: &[u8]) -> Value {
    read_glb(glb).0
}

/// Extract the embedded bytes for an image at `images[image_idx]` by
/// following its bufferView offset/length into the BIN chunk.
fn image_bytes(js: &Value, bin: &[u8], image_idx: usize) -> Vec<u8> {
    let bv_idx = js["images"][image_idx]["bufferView"].as_u64().unwrap() as usize;
    let bv = &js["bufferViews"][bv_idx];
    let offset = bv["byteOffset"].as_u64().unwrap_or(0) as usize;
    let length = bv["byteLength"].as_u64().unwrap() as usize;
    bin[offset..offset + length].to_vec()
}

#[test]
fn base_color_texture_writes_image_texture_sampler_and_material_ref() {
    let png_path = unique_tmp("albedo.png");
    fs::write(&png_path, rgb_png(4, [200, 180, 140])).expect("writing fixture png");

    let mut scene = SceneGraph::new();
    let mut mat = Material::new("painted");
    mat.base_color_texture = Some(TextureRef::new(png_path.clone()));
    let mat_id = scene.add_material(mat);

    let id = scene.add_root("box", "box", Transform::IDENTITY);
    scene.set_mesh(id, box_mesh([1.0, 1.0, 1.0]));
    scene.set_material(id, mat_id);

    let out = unique_tmp("out.glb");
    mgen_export::write_glb(&scene, &out).expect("write_glb");
    let bytes = fs::read(&out).expect("reading produced glb");
    let js = read_gltf_json(&bytes);

    // Texture scaffolding.
    let images = js["images"].as_array().expect("images[] should be present");
    assert_eq!(images.len(), 1, "one image per distinct texture path");
    assert_eq!(images[0]["mimeType"], "image/jpeg",
        "opaque colour sources transcode to JPEG for size");
    assert!(images[0].get("bufferView").is_some(), "image must reference a bufferView");

    let textures = js["textures"].as_array().expect("textures[] should be present");
    assert_eq!(textures.len(), 1);
    assert_eq!(textures[0]["source"], 0);
    assert_eq!(textures[0]["sampler"], 0);

    let samplers = js["samplers"].as_array().expect("samplers[] should be present");
    assert_eq!(samplers.len(), 1);
    assert_eq!(samplers[0]["wrapS"], 10497, "repeat wrap is the default");

    // Material wiring.
    let materials = js["materials"].as_array().expect("materials[]");
    let pbr = &materials[0]["pbrMetallicRoughness"];
    assert_eq!(pbr["baseColorTexture"]["index"], 0);

    // TEXCOORD_0 attribute on the primitive.
    let attrs = &js["meshes"][0]["primitives"][0]["attributes"];
    assert!(attrs.get("TEXCOORD_0").is_some(), "primitive must expose TEXCOORD_0");

    let _ = fs::remove_file(&png_path);
    let _ = fs::remove_file(&out);
    let _: Option<MaterialId> = None; // suppress unused-import on MaterialId for some toolchains
}

#[test]
fn duplicate_texture_paths_dedupe_to_one_image() {
    let png_path = unique_tmp("shared.png");
    fs::write(&png_path, rgb_png(4, [128, 128, 128])).expect("writing fixture png");

    let mut scene = SceneGraph::new();
    let mut a = Material::new("a");
    a.base_color_texture = Some(TextureRef::new(png_path.clone()));
    let mut b = Material::new("b");
    b.base_color_texture = Some(TextureRef::new(png_path.clone()));
    let a_id = scene.add_material(a);
    let b_id = scene.add_material(b);

    let na = scene.add_root("a", "box", Transform::IDENTITY);
    scene.set_mesh(na, box_mesh([1.0, 1.0, 1.0]));
    scene.set_material(na, a_id);
    let nb = scene.add_root("b", "box", Transform::IDENTITY);
    scene.set_mesh(nb, box_mesh([1.0, 1.0, 1.0]));
    scene.set_material(nb, b_id);

    let out = unique_tmp("dedup.glb");
    mgen_export::write_glb(&scene, &out).expect("write_glb");
    let bytes = fs::read(&out).expect("reading produced glb");
    let js = read_gltf_json(&bytes);

    assert_eq!(js["images"].as_array().unwrap().len(), 1);
    assert_eq!(js["textures"].as_array().unwrap().len(), 1);
    // Both materials should reference texture index 0.
    let mats = js["materials"].as_array().unwrap();
    assert_eq!(mats[0]["pbrMetallicRoughness"]["baseColorTexture"]["index"], 0);
    assert_eq!(mats[1]["pbrMetallicRoughness"]["baseColorTexture"]["index"], 0);

    let _ = fs::remove_file(&png_path);
    let _ = fs::remove_file(&out);
}

#[test]
fn missing_texture_file_returns_error() {
    let mut scene = SceneGraph::new();
    let mut mat = Material::new("ghost");
    mat.base_color_texture = Some(TextureRef::new(
        unique_tmp("does-not-exist.png"),
    ));
    let mat_id = scene.add_material(mat);
    let id = scene.add_root("box", "box", Transform::IDENTITY);
    scene.set_mesh(id, box_mesh([1.0, 1.0, 1.0]));
    scene.set_material(id, mat_id);

    let out = unique_tmp("bad.glb");
    let err = mgen_export::write_glb(&scene, &out)
        .expect_err("writing a GLB with a missing texture must error");
    let msg = format!("{err:#}");
    assert!(msg.contains("reading texture file"), "unhelpful error: {msg}");
}

#[test]
fn dsl_material_with_texture_lowers_and_exports() {
    // Full DSL surface: author `base_color_texture="..."` on a material, lower,
    // and verify the exporter emits the image + material reference. Catches
    // regressions where lowering silently drops the attribute or the exporter
    // stops honouring `base_color_texture` on materials built via the DSL.
    let png_path = unique_tmp("dsl-albedo.png");
    fs::write(&png_path, rgb_png(4, [255, 255, 255])).expect("writing fixture png");

    let src = format!(
        r#"
        material "painted" (
          color=[1, 1, 1],
          base_color_texture="{}"
        )
        scene {{
          box "b" (size=[1, 1, 1], mat="painted")
        }}
        "#,
        png_path.display()
    );
    let ast = mgen_dsl::parse(&src).expect("parse");
    let scene = mgen_dsl::lower(&ast).expect("lower");
    assert_eq!(scene.materials.len(), 1);
    assert!(scene.materials[0].base_color_texture.is_some(),
        "lower() must populate TextureRef from base_color_texture attr");

    let out = unique_tmp("dsl.glb");
    mgen_export::write_glb(&scene, &out).expect("write_glb");
    let bytes = fs::read(&out).expect("reading produced glb");
    let js = read_gltf_json(&bytes);
    assert_eq!(js["images"].as_array().unwrap().len(), 1);
    let attrs = &js["meshes"][0]["primitives"][0]["attributes"];
    assert!(attrs.get("TEXCOORD_0").is_some());

    let _ = fs::remove_file(&png_path);
    let _ = fs::remove_file(&out);
}

#[test]
fn mesh_without_uvs_omits_texcoord_0() {
    // Directly-constructed Mesh with no UVs (simulating a legacy pipeline
    // that never populated the field). Exporter should not write TEXCOORD_0
    // and the GLB should still be valid.
    let mut scene = SceneGraph::new();
    let id = scene.add_root("bare", "box", Transform::IDENTITY);
    let mesh = Mesh::new(
        vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0.0, 0.0, 1.0]; 3],
        vec![0, 1, 2],
    );
    scene.set_mesh(id, mesh);

    let out = unique_tmp("bare.glb");
    mgen_export::write_glb(&scene, &out).expect("write_glb");
    let bytes = fs::read(&out).expect("reading produced glb");
    let js = read_gltf_json(&bytes);
    let attrs = &js["meshes"][0]["primitives"][0]["attributes"];
    assert!(attrs.get("TEXCOORD_0").is_none());
    assert!(js.get("images").is_none(), "no images table when no textures authored");

    let _ = fs::remove_file(&out);
}

#[test]
fn normal_texture_stays_png_and_is_embedded_as_png_bytes() {
    // Normal maps carry numeric data per channel — JPEG would corrupt the
    // decoded normal vectors. Verify the exporter preserves PNG for this
    // slot and that the embedded bytes still decode as a PNG.
    let png_path = unique_tmp("normal.png");
    // A non-trivial pattern so oxipng has filter choices to make — all-solid
    // buffers compress the same way under any filter.
    let mut img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(8, 8);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = Rgb([(x * 32) as u8, (y * 32) as u8, 255]);
    }
    let mut png_bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .unwrap();
    fs::write(&png_path, &png_bytes).unwrap();

    let mut scene = SceneGraph::new();
    let mut mat = Material::new("bumpy");
    mat.normal_texture = Some(TextureRef::new(png_path.clone()));
    let mat_id = scene.add_material(mat);
    let id = scene.add_root("b", "box", Transform::IDENTITY);
    scene.set_mesh(id, box_mesh([1.0, 1.0, 1.0]));
    scene.set_material(id, mat_id);

    let out = unique_tmp("normal.glb");
    mgen_export::write_glb(&scene, &out).unwrap();
    let bytes = fs::read(&out).unwrap();
    let (js, bin) = read_glb(&bytes);
    assert_eq!(js["images"][0]["mimeType"], "image/png");

    let embedded = image_bytes(&js, &bin, 0);
    // PNG magic.
    assert_eq!(&embedded[0..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    // And `image` can round-trip it.
    image::load_from_memory_with_format(&embedded, image::ImageFormat::Png)
        .expect("embedded normal map should be a valid PNG");

    let _ = fs::remove_file(&png_path);
    let _ = fs::remove_file(&out);
}

#[test]
fn base_color_with_transparency_stays_png() {
    // JPEG has no alpha channel, so a base-colour texture whose alpha is
    // actually used (e.g. a pierced leaf or cut-out decal) must stay PNG.
    let png_path = unique_tmp("leaf.png");
    fs::write(&png_path, rgba_png_with_transparency(8)).unwrap();

    let mut scene = SceneGraph::new();
    let mut mat = Material::new("leaf");
    mat.base_color_texture = Some(TextureRef::new(png_path.clone()));
    let mat_id = scene.add_material(mat);
    let id = scene.add_root("b", "box", Transform::IDENTITY);
    scene.set_mesh(id, box_mesh([1.0, 1.0, 1.0]));
    scene.set_material(id, mat_id);

    let out = unique_tmp("leaf.glb");
    mgen_export::write_glb(&scene, &out).unwrap();
    let bytes = fs::read(&out).unwrap();
    let js = read_gltf_json(&bytes);
    assert_eq!(js["images"][0]["mimeType"], "image/png",
        "alpha-bearing colour texture must stay PNG — JPEG has no alpha channel");

    let _ = fs::remove_file(&png_path);
    let _ = fs::remove_file(&out);
}

#[test]
fn emissive_texture_transcodes_to_jpeg() {
    let png_path = unique_tmp("emissive.png");
    fs::write(&png_path, rgb_png(8, [255, 80, 20])).unwrap();

    let mut scene = SceneGraph::new();
    let mut mat = Material::new("glow");
    mat.emissive_texture = Some(TextureRef::new(png_path.clone()));
    let mat_id = scene.add_material(mat);
    let id = scene.add_root("b", "box", Transform::IDENTITY);
    scene.set_mesh(id, box_mesh([1.0, 1.0, 1.0]));
    scene.set_material(id, mat_id);

    let out = unique_tmp("emissive.glb");
    mgen_export::write_glb(&scene, &out).unwrap();
    let bytes = fs::read(&out).unwrap();
    let js = read_gltf_json(&bytes);
    assert_eq!(js["images"][0]["mimeType"], "image/jpeg");

    let _ = fs::remove_file(&png_path);
    let _ = fs::remove_file(&out);
}

#[test]
fn same_path_used_as_color_and_linear_embeds_twice() {
    // Pathological but legal: the same file is referenced as both base_color
    // (colour, becomes JPEG) and occlusion (linear, stays PNG). The exporter
    // must embed it twice so each slot gets a format appropriate to its role.
    let png_path = unique_tmp("dual.png");
    fs::write(&png_path, rgb_png(4, [120, 120, 120])).unwrap();

    let mut scene = SceneGraph::new();
    let mut mat = Material::new("dual");
    mat.base_color_texture = Some(TextureRef::new(png_path.clone()));
    mat.occlusion_texture = Some(TextureRef::new(png_path.clone()));
    let mat_id = scene.add_material(mat);
    let id = scene.add_root("b", "box", Transform::IDENTITY);
    scene.set_mesh(id, box_mesh([1.0, 1.0, 1.0]));
    scene.set_material(id, mat_id);

    let out = unique_tmp("dual.glb");
    mgen_export::write_glb(&scene, &out).unwrap();
    let bytes = fs::read(&out).unwrap();
    let js = read_gltf_json(&bytes);

    let images = js["images"].as_array().unwrap();
    assert_eq!(images.len(), 2, "same path in two slot kinds embeds twice");
    let mimes: Vec<&str> = images.iter().map(|i| i["mimeType"].as_str().unwrap()).collect();
    assert!(mimes.contains(&"image/jpeg"), "colour side should be JPEG: {mimes:?}");
    assert!(mimes.contains(&"image/png"), "linear side should be PNG: {mimes:?}");

    let _ = fs::remove_file(&png_path);
    let _ = fs::remove_file(&out);
}

#[test]
fn jpeg_source_for_color_slot_passes_through_as_jpeg() {
    // A user-supplied JPEG stays JPEG — we decode and re-encode, but the
    // output is still JPEG q=90. The important bit is the mime type.
    let jpeg_path = unique_tmp("source.jpg");
    let rgb: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(8, 8, Rgb([200, 100, 50]));
    let mut jpeg_bytes = Vec::new();
    rgb.write_to(&mut std::io::Cursor::new(&mut jpeg_bytes), image::ImageFormat::Jpeg)
        .unwrap();
    fs::write(&jpeg_path, &jpeg_bytes).unwrap();

    let mut scene = SceneGraph::new();
    let mut mat = Material::new("m");
    mat.base_color_texture = Some(TextureRef::new(jpeg_path.clone()));
    let mat_id = scene.add_material(mat);
    let id = scene.add_root("b", "box", Transform::IDENTITY);
    scene.set_mesh(id, box_mesh([1.0, 1.0, 1.0]));
    scene.set_material(id, mat_id);

    let out = unique_tmp("jpgsrc.glb");
    mgen_export::write_glb(&scene, &out).unwrap();
    let bytes = fs::read(&out).unwrap();
    let js = read_gltf_json(&bytes);
    assert_eq!(js["images"][0]["mimeType"], "image/jpeg");

    let _ = fs::remove_file(&jpeg_path);
    let _ = fs::remove_file(&out);
}
