//! A material's preview shader must ride to glTF `node.extras.shader` as
//! `{name, params}` metadata — mogen never compiles GLSL, but a downstream
//! consumer (or MoGen Studio) needs the name + resolved params to bind the
//! real shader. Compiles a small `.mog` through the full pipeline and
//! inspects the JSON chunk.

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
fn declared_shader_params_merge_defaults_with_overrides() {
    let src = r#"
        shader "ripple" (source="shaders/ripple.glsl") {
          param "speed" (type=float, default=2.0)
          param "tint" (type=color, default=[0.0, 0.3, 0.5])
        }
        material "pond" (color=[0.1, 0.2, 0.3], shader="ripple") {
          shader_params (speed=3.5)
        }
        scene { box "b" (size=[1,1,1], mat="pond") }
    "#;
    let json = parse_glb_json(&compile(src));
    let sh = &find_node(&json, "b")["extras"]["shader"];

    assert_eq!(sh["name"], "ripple");
    // Override wins for speed; tint falls back to the declared default.
    assert!((sh["params"]["speed"].as_f64().unwrap() - 3.5).abs() < 1e-6);
    let tint = sh["params"]["tint"].as_array().expect("tint array");
    assert!((tint[2].as_f64().unwrap() - 0.5).abs() < 1e-6);
}

#[test]
fn builtin_water_shader_projects_name_with_no_declaration() {
    let src = r#"
        material "sea" (color=[0.0, 0.3, 0.5], shader="water")
        scene { box "b" (size=[1,1,1], mat="sea") }
    "#;
    let json = parse_glb_json(&compile(src));
    let sh = &find_node(&json, "b")["extras"]["shader"];
    assert_eq!(sh["name"], "water");
}

#[test]
fn standard_material_has_no_shader_extras() {
    let src = r#"scene { box "plain" (size=[1,1,1]) }"#;
    let json = parse_glb_json(&compile(src));
    let node = find_node(&json, "plain");
    if let Some(extras) = node.get("extras") {
        assert!(extras.get("shader").is_none(), "unexpected shader extras: {extras:?}");
    }
}
