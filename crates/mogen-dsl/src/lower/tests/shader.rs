use super::*;
use crate::lower::*;
use crate::parser::parse;

#[test]
fn shader_declaration_and_material_reference_lower_cleanly() {
    let g = lower_src(
        r#"
        shader "ripple" (source="shaders/ripple.glsl") {
          param "speed" (type=float, default=2.0)
          param "tint" (type=color, default=[0.0, 0.3, 0.5])
        }
        material "pond" (color=[0.1, 0.2, 0.3], shader="ripple") {
          shader_params (speed=3.5, tint=[0.0, 0.4, 0.6])
        }
        scene { box "b" (size=[1,1,1], mat="pond") }
        "#,
    );
    let decl = g.find_shader_scoped("ripple", None).expect("ripple shader declared");
    assert_eq!(decl.params.len(), 2);

    let mid = g.find_material("pond").expect("pond material");
    let mat = &g.materials[mid.0 as usize];
    assert_eq!(mat.shader_name.as_deref(), Some("ripple"));
    assert_eq!(
        mat.shader_params.get("speed"),
        Some(&mogen_core::ShaderParamValue::Float(3.5))
    );
    assert_eq!(
        mat.shader_params.get("tint"),
        Some(&mogen_core::ShaderParamValue::Vec3([0.0, 0.4, 0.6]))
    );
}

#[test]
fn standard_and_pbr_shader_values_clear_shader_name() {
    let g = lower_src(
        r#"
        material "a" (color=[0.1, 0.2, 0.3], shader="standard")
        scene { box "b" (size=[1,1,1], mat="a") }
        "#,
    );
    let mid = g.find_material("a").expect("material a");
    assert_eq!(g.materials[mid.0 as usize].shader_name, None);
}

#[test]
fn builtin_water_shader_is_seeded_without_declaration() {
    let g = lower_src(r#"scene { box "b" (size=[1,1,1]) }"#);
    assert!(g.has_shader(mogen_core::shader::WATER));
    assert!(g.find_shader_scoped(mogen_core::shader::WATER, None).is_some());
}

/// The built-in preset declares `absorption`, so a material can carry *how*
/// absorbing its water is rather than only that it is water. Resolution goes
/// through the ordinary `resolve_param` path — no special case for the
/// built-in.
#[test]
fn builtin_water_declares_absorption_and_resolves_an_override() {
    use mogen_core::shader::{WATER, WATER_ABSORPTION, WATER_ABSORPTION_DEFAULT};
    use mogen_core::ShaderParamValue;

    let g = lower_src(
        r#"
        material "pond" (color=[0.15, 0.4, 0.45], shader="water") {
          shader_params (absorption=2.5)
        }
        material "sea" (color=[0.1, 0.3, 0.4], shader="water")
        scene { box "b" (size=[1,1,1], mat="pond") }
        "#,
    );
    let decl = g.find_shader_scoped(WATER, None).expect("water shader");
    assert_eq!(
        decl.params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        [WATER_ABSORPTION],
        "the preset declares exactly its one parameter"
    );

    let pond = &g.materials[g.find_material("pond").expect("pond").0 as usize];
    assert_eq!(
        decl.resolve_param(WATER_ABSORPTION, &pond.shader_params),
        Some(ShaderParamValue::Float(2.5)),
        "an authored override must win"
    );

    // The control: a material that says nothing gets the declared default, and
    // that default is not the disabled value — otherwise `shader=\"water\"`
    // would be a no-op reporting success.
    let sea = &g.materials[g.find_material("sea").expect("sea").0 as usize];
    assert_eq!(
        decl.resolve_param(WATER_ABSORPTION, &sea.shader_params),
        Some(ShaderParamValue::Float(WATER_ABSORPTION_DEFAULT))
    );
    assert!(WATER_ABSORPTION_DEFAULT > 0.0);
}

#[test]
fn user_declared_water_shader_shadows_the_builtin() {
    let g = lower_src(
        r#"
        shader "water" (source="my_water.glsl")
        scene { box "b" (size=[1,1,1]) }
        "#,
    );
    let decl = g
        .find_shader_scoped(mogen_core::shader::WATER, None)
        .expect("water shader");
    assert_eq!(decl.source, std::path::PathBuf::from("my_water.glsl"));
}

#[test]
fn duplicate_shader_declaration_in_same_file_dedupes_to_first() {
    let g = lower_src(
        r#"
        shader "ripple" (source="first.glsl") { param "speed" (type=float, default=1.0) }
        shader "ripple" (source="second.glsl")
        scene { box "b" (size=[1,1,1]) }
        "#,
    );
    let matches: Vec<_> = g.shaders.iter().filter(|s| s.name == "ripple").collect();
    assert_eq!(matches.len(), 1, "second declaration of the same name should be dropped");
    assert_eq!(matches[0].source, std::path::PathBuf::from("first.glsl"));
}

#[test]
fn shader_without_source_errors() {
    let ast = parse(r#"shader "ripple" { param "speed" (type=float) }"#).unwrap();
    assert!(lower(&ast).is_err(), "shader declaration requires a `source=`");
}

#[test]
fn shader_param_without_type_errors() {
    let ast = parse(r#"shader "ripple" (source="x.glsl") { param "speed" }"#).unwrap();
    assert!(lower(&ast).is_err(), "shader param requires a `type=`");
}

#[test]
fn shader_param_unknown_type_errors() {
    let ast =
        parse(r#"shader "ripple" (source="x.glsl") { param "speed" (type=matrix4) }"#).unwrap();
    assert!(lower(&ast).is_err(), "unknown param type must error");
}

#[test]
fn shader_param_default_type_mismatch_errors() {
    let ast = parse(
        r#"shader "ripple" (source="x.glsl") { param "speed" (type=float, default=[1,2,3]) }"#,
    )
    .unwrap();
    assert!(
        lower(&ast).is_err(),
        "a vec3 default on a float param must error"
    );
}
