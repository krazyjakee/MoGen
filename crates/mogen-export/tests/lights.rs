//! Round-trip coverage for `KHR_lights_punctual`: scene graphs with a
//! `Light` on a node should produce a GLB whose JSON chunk has the right
//! top-level extension, the right `extensionsUsed` entry, and per-node
//! `extensions.KHR_lights_punctual.light` indices wired to the lights array.

use serde_json::Value;

use mogen_core::{Light, LightKind, SceneGraph, Transform};

fn parse_glb_json(bytes: &[u8]) -> Value {
    // Header is 12 bytes; the first chunk header is 8 bytes; then JSON payload.
    let json_len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    let json_start = 20;
    let json_bytes = &bytes[json_start..json_start + json_len];
    serde_json::from_slice(json_bytes).expect("valid JSON chunk")
}

fn build_glb(scene: &SceneGraph) -> Vec<u8> {
    mogen_export::build_glb_with_options(
        scene,
        &mogen_export::ExportOptions::default(),
        |_| {},
    )
    .expect("export ok")
}

#[test]
fn directional_light_emits_extension_and_node_link() {
    let mut scene = SceneGraph::new();
    let id = scene.add_root("sun", "light", Transform::default());
    scene.set_light(
        id,
        Light {
            kind: LightKind::Directional,
            color: [1.0, 0.95, 0.85],
            intensity: 3.0,
            range: None,
            inner_cone_rad: 0.0,
            outer_cone_rad: std::f32::consts::FRAC_PI_4,
        },
    );

    let bytes = build_glb(&scene);
    let json = parse_glb_json(&bytes);

    let used = json["extensionsUsed"].as_array().expect("extensionsUsed");
    assert!(
        used.iter().any(|v| v == "KHR_lights_punctual"),
        "extensionsUsed should mention KHR_lights_punctual, got {used:?}",
    );

    let lights = &json["extensions"]["KHR_lights_punctual"]["lights"];
    let lights = lights.as_array().expect("lights array");
    assert_eq!(lights.len(), 1);
    assert_eq!(lights[0]["type"], "directional");
    let color = lights[0]["color"].as_array().expect("color array");
    let expected = [1.0_f32, 0.95, 0.85];
    for (i, v) in color.iter().enumerate() {
        let got = v.as_f64().unwrap() as f32;
        assert!(
            (got - expected[i]).abs() < 1e-6,
            "color[{i}] = {got}, expected {}",
            expected[i],
        );
    }
    assert_eq!(lights[0]["intensity"], serde_json::json!(3.0));

    let node_ext = &json["nodes"][0]["extensions"]["KHR_lights_punctual"]["light"];
    assert_eq!(node_ext, &serde_json::json!(0));
}

#[test]
fn point_light_emits_range_but_no_spot_block() {
    let mut scene = SceneGraph::new();
    let id = scene.add_root("lamp", "light", Transform::default());
    scene.set_light(
        id,
        Light {
            kind: LightKind::Point,
            color: [1.0, 1.0, 1.0],
            intensity: 10.0,
            range: Some(8.0),
            ..Default::default()
        },
    );

    let bytes = build_glb(&scene);
    let json = parse_glb_json(&bytes);
    let l = &json["extensions"]["KHR_lights_punctual"]["lights"][0];
    assert_eq!(l["type"], "point");
    assert_eq!(l["range"], serde_json::json!(8.0));
    assert!(l.get("spot").is_none(), "point light should not have spot block");
}

#[test]
fn spot_light_emits_radian_cone_angles() {
    let mut scene = SceneGraph::new();
    let id = scene.add_root("spot", "light", Transform::default());
    scene.set_light(
        id,
        Light {
            kind: LightKind::Spot,
            color: [1.0, 1.0, 1.0],
            intensity: 20.0,
            range: Some(10.0),
            inner_cone_rad: 20f32.to_radians(),
            outer_cone_rad: 35f32.to_radians(),
        },
    );

    let bytes = build_glb(&scene);
    let json = parse_glb_json(&bytes);
    let l = &json["extensions"]["KHR_lights_punctual"]["lights"][0];
    assert_eq!(l["type"], "spot");
    let inner = l["spot"]["innerConeAngle"].as_f64().unwrap() as f32;
    let outer = l["spot"]["outerConeAngle"].as_f64().unwrap() as f32;
    assert!((inner - 20f32.to_radians()).abs() < 1e-5);
    assert!((outer - 35f32.to_radians()).abs() < 1e-5);
}

#[test]
fn scene_without_lights_omits_extension() {
    let mut scene = SceneGraph::new();
    scene.add_root("group", "group", Transform::default());
    let bytes = build_glb(&scene);
    let json = parse_glb_json(&bytes);
    assert!(
        json.get("extensions").is_none(),
        "no lights → no extensions block, got {:?}",
        json.get("extensions"),
    );
    let used = json
        .get("extensionsUsed")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !used.iter().any(|v| v == "KHR_lights_punctual"),
        "no lights → no KHR_lights_punctual in extensionsUsed",
    );
}
