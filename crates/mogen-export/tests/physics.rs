//! Physics data must ride to glTF `node.extras.physics` so a downstream
//! importer can rebuild a RigidBody + PhysicsMaterial. Compiles a small `.mog`
//! through the full pipeline and inspects the JSON chunk.

use serde_json::Value;

fn parse_glb_json(bytes: &[u8]) -> Value {
    let json_len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    let json_bytes = &bytes[20..20 + json_len];
    serde_json::from_slice(json_bytes).expect("valid JSON chunk")
}

fn compile(src: &str) -> Vec<u8> {
    let ast = mogen_dsl::parse(src).expect("parse");
    let scene = mogen_dsl::lower(&ast).expect("lower");
    mogen_export::build_glb_with_options(&scene, &mogen_export::ExportOptions::default(), |_| {})
        .expect("export")
}

fn find_node<'a>(json: &'a Value, name: &str) -> &'a Value {
    json["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["name"] == name)
        .unwrap_or_else(|| panic!("no node {name}"))
}

#[test]
fn physics_extras_are_written_with_computed_weight() {
    let src = r#"
        physics "oak" (weight=700kg/m3, friction=0.6, bounce=0.2)
        scene { box "crate" (size=[1,1,1], phys="oak") }
    "#;
    let json = parse_glb_json(&compile(src));
    let phys = &find_node(&json, "crate")["extras"]["physics"];

    assert_eq!(phys["material"], "oak");
    assert!((phys["weight_per_m3"].as_f64().unwrap() - 700.0).abs() < 1e-2);
    assert!((phys["friction"].as_f64().unwrap() - 0.6).abs() < 1e-3);
    assert!((phys["bounce"].as_f64().unwrap() - 0.2).abs() < 1e-3);
    // 1 m³ of oak weighs ~700 kg — auto-computed from the real mesh volume.
    assert!((phys["weight"].as_f64().unwrap() - 700.0).abs() < 1.0);
    // Centred box → centre of gravity at the local origin.
    let cog = phys["center_of_gravity"].as_array().expect("cog array");
    assert!(cog.iter().all(|c| c.as_f64().unwrap().abs() < 1e-3));
}

#[test]
fn nodes_without_physics_have_no_physics_extras() {
    let src = r#"scene { box "plain" (size=[1,1,1]) }"#;
    let json = parse_glb_json(&compile(src));
    let node = find_node(&json, "plain");
    // Either no extras at all, or extras without a `physics` key.
    if let Some(extras) = node.get("extras") {
        assert!(extras.get("physics").is_none(), "unexpected physics extras: {extras:?}");
    }
}
