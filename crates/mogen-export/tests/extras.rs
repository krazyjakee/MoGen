//! Author-supplied `extras="{…}"` must reach glTF `node.extras` verbatim,
//! merged over the entries mogen derives itself. This is the escape hatch
//! converters use to round-trip metadata the DSL has no dedicated attribute
//! for. Compiles a small `.mog` through the full pipeline and inspects the
//! JSON chunk. Well-formedness is a validator concern and is covered by
//! `mogen-validate`'s `extras_attr_tests`.

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
fn extras_object_is_written_verbatim() {
    let src = r#"
        scene {
          box "thing" (size=[1,1,1],
                       extras="{\"hp\": 42, \"loot\": [\"gold\", \"gem\"], \"boss\": true}")
        }
    "#;
    let json = parse_glb_json(&compile(src));
    let extras = &find_node(&json, "thing")["extras"];

    assert_eq!(extras["hp"], 42);
    assert_eq!(extras["loot"][0], "gold");
    assert_eq!(extras["loot"][1], "gem");
    assert_eq!(extras["boss"], true);
}

#[test]
fn extras_coexists_with_derived_entries() {
    let src = r#"
        scene {
          box "item" (size=[1,1,1], role="prop", tags="wood,old",
                      extras="{\"custom\": 1}")
        }
    "#;
    let json = parse_glb_json(&compile(src));
    let extras = &find_node(&json, "item")["extras"];

    assert_eq!(extras["role"], "prop");
    assert_eq!(extras["tags"][0], "wood");
    assert_eq!(extras["tags"][1], "old");
    assert_eq!(extras["custom"], 1);
}

#[test]
fn explicit_extras_key_overrides_the_derived_one() {
    // Deliberate: a converter must be able to set a key mogen also derives,
    // without us having to anticipate which keys that might be.
    let src = r#"
        scene {
          box "item" (size=[1,1,1], role="prop", extras="{\"role\": \"override\"}")
        }
    "#;
    let json = parse_glb_json(&compile(src));
    let extras = &find_node(&json, "item")["extras"];

    assert_eq!(extras["role"], "override");
}

#[test]
fn nested_objects_survive_the_round_trip() {
    // The shape converters actually need — e.g. an interactivity descriptor
    // or a saved camera hanging off a node. Note the `r##` delimiter: the
    // colour literal contains `"#`, which would end an `r#"…"#` string.
    let src = r##"
        scene {
          box "lamp" (size=[1,1,1],
                      extras="{\"interactive\": {\"controls\": [{\"kind\": \"toggle\"}], \"effects\": [{\"kind\": \"light\", \"color\": \"#fff\"}]}}")
        }
    "##;
    let json = parse_glb_json(&compile(src));
    let extras = &find_node(&json, "lamp")["extras"];

    assert_eq!(extras["interactive"]["controls"][0]["kind"], "toggle");
    assert_eq!(extras["interactive"]["effects"][0]["color"], "#fff");
}

#[test]
fn node_without_extras_gains_no_key() {
    let src = r#"scene { box "plain" (size=[1,1,1]) }"#;
    let json = parse_glb_json(&compile(src));
    let node = find_node(&json, "plain");

    assert!(node["extras"].get("hp").is_none());
}
