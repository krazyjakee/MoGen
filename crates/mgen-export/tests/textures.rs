//! End-to-end coverage of the texture export path: DSL-lowered materials with
//! `base_color_texture` pointing at an on-disk PNG should round-trip into a
//! valid GLB with populated `images[]` / `textures[]` / `samplers[]` tables
//! and material references wired to the right indices.

use std::fs;
use std::path::PathBuf;
use std::process::id;

use serde_json::Value;

use mgen_core::{Material, Mesh, MaterialId, SceneGraph, TextureRef, Transform};
use mgen_geom::box_mesh;

/// 1×1 all-white PNG — smallest valid PNG we can hand-craft so tests don't
/// need a committed fixture. Bytes verified against `pngcheck`.
const WHITE_PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
    0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41,
    0x54, 0x08, 0x99, 0x63, 0xF8, 0xFF, 0xFF, 0x3F,
    0x00, 0x05, 0xFE, 0x02, 0xFE, 0xDC, 0xCC, 0x59,
    0xE7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn unique_tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("mgen-texture-export-{}-{name}", id()))
}

fn read_gltf_json(glb: &[u8]) -> Value {
    assert_eq!(&glb[0..4], b"glTF", "not a GLB");
    let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    let json_bytes = &glb[20..20 + json_len];
    serde_json::from_slice(json_bytes).expect("invalid GLB JSON chunk")
}

#[test]
fn base_color_texture_writes_image_texture_sampler_and_material_ref() {
    let png_path = unique_tmp("albedo.png");
    fs::write(&png_path, WHITE_PNG_1X1).expect("writing fixture png");

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
    assert_eq!(images[0]["mimeType"], "image/png");
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
    fs::write(&png_path, WHITE_PNG_1X1).expect("writing fixture png");

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
    fs::write(&png_path, WHITE_PNG_1X1).expect("writing fixture png");

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
