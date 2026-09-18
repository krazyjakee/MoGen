//! A hand-built glTF exercising what MoGen's own GLBs never contain: an
//! embedded PNG texture, a triangle strip, non-indexed geometry, a point
//! primitive, a matrix transform, a camera, an unsupported extension, and a
//! mesh placed twice.

use std::fs;

use base64::Engine as _;
use mogen_import::{import_gltf, Options};

/// A valid 2×2 RGBA PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x72, 0xb6, 0x0d,
    0x24, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x0a, 0x21, 0x14, 0x00, 0x45, 0xcc, 0x07, 0xf9, 0x66, 0xcb, 0xff, 0x39, 0x00, 0x00, 0x00,
    0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn fixture() -> String {
    let mut bin = Vec::new();
    // 0..48: quad positions (strip order).
    bin.extend(f32s(&[0., 0., 0., 1., 0., 0., 0., 1., 0., 1., 1., 0.]));
    // 48..80: quad UVs.
    bin.extend(f32s(&[0., 1., 1., 1., 0., 0., 1., 0.]));
    // 80..88: strip indices, u16.
    bin.extend([0u16, 1, 2, 3].iter().flat_map(|i| i.to_le_bytes()));
    // 88..124: one non-indexed triangle.
    bin.extend(f32s(&[0., 0., 0., 1., 0., 0., 0., 0., -1.]));
    assert_eq!(bin.len(), 124);
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);

    format!(
        r#"{{
  "asset": {{"version": "2.0"}},
  "extensionsUsed": ["KHR_texture_transform"],
  "scene": 0,
  "scenes": [{{"nodes": [0, 3]}}],
  "nodes": [
    {{"name": "Root Node", "matrix": [2,0,0,0, 0,2,0,0, 0,0,2,0, 1,2,3,1], "children": [1, 2]}},
    {{"name": "Panel", "mesh": 0}},
    {{"name": "Panel", "mesh": 0, "translation": [0.5, 0, 0]}},
    {{"name": "Cam", "camera": 0}}
  ],
  "cameras": [{{"type": "perspective", "perspective": {{"yfov": 1.0, "znear": 0.1}}}}],
  "meshes": [{{"name": "panel mesh", "primitives": [
    {{"attributes": {{"POSITION": 0, "TEXCOORD_0": 1}}, "indices": 2, "mode": 5, "material": 0}},
    {{"attributes": {{"POSITION": 3}}, "material": 1}},
    {{"attributes": {{"POSITION": 3}}, "mode": 0}}
  ]}}],
  "materials": [
    {{"name": "Painted Metal", "pbrMetallicRoughness": {{"baseColorFactor": [1, 0.5, 0.25, 1], "metallicFactor": 0.8, "roughnessFactor": 0.3, "baseColorTexture": {{"index": 0}}}}}},
    {{"name": "Glass", "alphaMode": "BLEND", "doubleSided": true, "pbrMetallicRoughness": {{"baseColorFactor": [0.9, 0.9, 1, 0.25]}}}}
  ],
  "textures": [{{"source": 0}}],
  "images": [{{"name": "paint albedo", "uri": "data:image/png;base64,{png}"}}],
  "buffers": [{{"byteLength": 124, "uri": "data:application/octet-stream;base64,{bin}"}}],
  "bufferViews": [
    {{"buffer": 0, "byteOffset": 0, "byteLength": 48}},
    {{"buffer": 0, "byteOffset": 48, "byteLength": 32}},
    {{"buffer": 0, "byteOffset": 80, "byteLength": 8}},
    {{"buffer": 0, "byteOffset": 88, "byteLength": 36}}
  ],
  "accessors": [
    {{"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]}},
    {{"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC2"}},
    {{"bufferView": 2, "componentType": 5123, "count": 4, "type": "SCALAR"}},
    {{"bufferView": 3, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,-1], "max": [1,0,0]}}
  ]
}}"#,
        png = b64(PNG),
        bin = b64(&bin),
    )
}

#[test]
fn a_feature_rich_gltf_imports_builds_and_reports_what_it_dropped() {
    let json = fixture();
    let out = import_gltf(
        json.as_bytes(),
        None,
        "panel.gltf",
        "panel",
        &Options::default(),
    )
    .expect("imports");
    let src = &out.source;

    // Header: the source, the tally, and every skipped feature.
    assert!(
        src.starts_with("// Imported from glTF \"panel.gltf\" as \"panel\".\n"),
        "{src}"
    );
    assert!(
        src.contains("// 3 nodes · 1 meshes · 2 polys · 3 triangles · 2 materials · 1 textures"),
        "{src}"
    );
    assert!(src.contains("cameras (1)"), "{src}");
    assert!(src.contains("point / line primitives (1)"), "{src}");
    assert!(src.contains("nodes without geometry (1)"), "{src}");
    assert!(
        src.contains("extension KHR_texture_transform is not supported"),
        "{src}"
    );

    // The texture comes out as its original bytes, and the material points at it.
    assert_eq!(out.assets.len(), 1);
    assert_eq!(out.assets[0].path, "panel_textures/paint_albedo.png");
    assert_eq!(out.assets[0].bytes, PNG);
    assert!(src.contains(
        "material \"Painted_Metal\" (color=[1, 0.5, 0.25], metallic=0.8, roughness=0.3, base_color_texture=\"panel_textures/paint_albedo.png\")"
    ), "{src}");
    assert!(src.contains(
        "material \"Glass\" (color=[0.9, 0.9, 1], metallic=1, roughness=1, alpha_mode=\"blend\", alpha=0.25, double_sided=1)"
    ), "{src}");

    // The matrix decomposes to pos + uniform scale; the shared mesh becomes a
    // module placed twice; names are sanitised and made unique.
    assert!(
        src.contains("group \"Root_Node\" (pos=[1, 2, 3], scale=2) {"),
        "{src}"
    );
    assert!(src.contains("module \"panel_mesh\" {"), "{src}");
    assert_eq!(src.matches("use \"panel_mesh\"").count(), 2, "{src}");
    assert!(
        src.contains("group \"Panel\" {") && src.contains("group \"Panel_2\" (pos=[0.5, 0, 0]) {"),
        "{src}"
    );
    assert!(src.contains("poly \"panel_mesh_Painted_Metal\""), "{src}");
    assert!(src.contains("poly \"panel_mesh_Glass\""), "{src}");
    // The strip became two triangles with consistent winding.
    assert!(src.contains("indices=[0, 1, 2, 2, 1, 3]"), "{src}");

    // And the result builds.
    let dir = std::env::temp_dir().join(format!("mogen-import-features-{}", std::process::id()));
    fs::create_dir_all(dir.join("panel_textures")).unwrap();
    fs::write(dir.join(&out.assets[0].path), &out.assets[0].bytes).unwrap();
    let ast = mogen_dsl::parse(src).expect("parses");
    let diags = mogen_validate::validate_ast_with_source(&ast, Some(&dir));
    assert!(
        !mogen_core::has_errors(&diags),
        "{}",
        mogen_validate::render_json("panel", &diags)
    );
    let mut graph = mogen_dsl::lower_with_source(&ast, Some(&dir)).expect("lowers");
    graph.resolve_texture_paths(&dir);
    let diags = mogen_validate::validate_graph(&graph);
    assert!(
        !mogen_core::has_errors(&diags),
        "{}",
        mogen_validate::render_json("panel", &diags)
    );
    let glb = dir.join("panel.glb");
    mogen_export::write_glb(&graph, &glb).expect("exports");
    let (doc, _, images) = gltf::import(&glb).expect("rebuilt GLB reads");
    assert_eq!(images.len(), 1, "the texture made it into the rebuilt GLB");
    assert!(doc
        .materials()
        .any(|m| m.alpha_mode() == gltf::material::AlphaMode::Blend));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn obj_and_fbx_are_recognised_but_not_yet_read() {
    for ext in ["obj", "fbx"] {
        let path = std::path::PathBuf::from(format!("model.{ext}"));
        assert!(mogen_import::Format::from_path(&path).is_some());
        let err = mogen_import::import_file(&path, "m", &Options::default()).unwrap_err();
        assert!(format!("{err:#}").contains("not supported yet"), "{err:#}");
    }
    assert!(mogen_import::Format::from_path(std::path::Path::new("scene.json")).is_none());
    assert_eq!(
        mogen_import::Format::from_path(std::path::Path::new("A.GLB")),
        Some(mogen_import::Format::Gltf)
    );
}
