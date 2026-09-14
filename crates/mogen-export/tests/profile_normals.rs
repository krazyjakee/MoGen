//! End-to-end regression: repaired constructor normals survive the GLB accessor.
use serde_json::Value;

#[test]
fn profile_normals_survive_glb_export() {
    let source = include_str!("../../../examples/features/profile_normals.mog");
    let scene = mogen_dsl::lower(&mogen_dsl::parse(source).unwrap()).unwrap();
    let bytes = mogen_export::build_glb_with_options(
        &scene,
        &mogen_export::ExportOptions::default(),
        |_| {},
    )
    .unwrap();
    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json: Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
    let bin = &bytes[28 + json_len..];
    for name in ["arm", "rail", "post"] {
        let original = scene.nodes.iter().find(|n| n.name == name).unwrap();
        let original = original.mesh.as_ref().unwrap();
        let node = json["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["name"] == name)
            .unwrap();
        let mesh = &json["meshes"][node["mesh"].as_u64().unwrap() as usize];
        let accessor_id = mesh["primitives"][0]["attributes"]["NORMAL"]
            .as_u64()
            .unwrap() as usize;
        let accessor = &json["accessors"][accessor_id];
        assert_eq!(accessor["componentType"], 5126);
        assert_eq!(accessor["type"], "VEC3");
        assert_eq!(
            accessor["count"].as_u64().unwrap() as usize,
            original.normals.len()
        );
        let view = &json["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
        let offset = view["byteOffset"].as_u64().unwrap_or(0) as usize
            + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
        let stride = view["byteStride"].as_u64().unwrap_or(12) as usize;
        for (i, expected) in original.normals.iter().enumerate() {
            let normal: [f32; 3] = std::array::from_fn(|k| {
                let start = offset + i * stride + k * 4;
                f32::from_le_bytes(bin[start..start + 4].try_into().unwrap())
            });
            assert_eq!(&normal, expected, "{name}: exported normal changed");
            let length = normal.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!(
                length.is_finite() && (length - 1.0).abs() < 1e-5,
                "{name}: {normal:?}"
            );
        }
    }
}
