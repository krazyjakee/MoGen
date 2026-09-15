use super::super::*;
use super::support::*;
use crate::{GenerateConfig, ImageInput};
use serde_json::json;

#[test]
fn scripted_tool_sequence_inspects_edits_compiles_renders_and_corrects_again() {
    let next = ORIGINAL.replace("size=[0.1,0.1,1]", "size=[0.2,0.1,1]");
    let last = next.replace("size=[0.2,0.1,1]", "size=[0.3,0.1,1]");
    let sequence = vec![
        json!({"tool":"inspect","revision":rev(ORIGINAL),"name":"arm"}),
        json!({"tool":"apply","revision":rev(ORIGINAL),"edits":next}),
        json!({"tool":"compile","revision":rev(&next)}),
        json!({"tool":"render","revision":rev(&next),"view":"back"}),
        json!({"tool":"apply","revision":rev(&next),"edits":last}),
        json!({"tool":"finish","revision":rev(&last),"findings":"arm thickness corrected"}),
    ];
    let mut calls = sequence.into_iter();
    let mut model = |cfg: &GenerateConfig| {
        let initial_prompt = cfg.history.first().map_or(&cfg.user_prompt, |t| &t.text);
        assert!(initial_prompt.contains("Original target: carved chair"));
        assert!(initial_prompt.contains("Dimensions/units: one metre wide"));
        assert!(initial_prompt.contains("thicken arm"));
        assert_eq!(cfg.user_images[0].data, vec![1, 2, 3]);
        Ok(response(calls.next().unwrap().to_string()))
    };
    let mut w = ModelingWorkspace::new(ORIGINAL.into(), None, vec![], None).unwrap();
    let mut renderer = Renderer {
        seen: vec![],
        fail: false,
    };
    let mut cfg = GenerateConfig::new("chair");
    let mut brief = ModelingBrief {
        prompt: "carved chair".into(),
        dimensions: "one metre wide".into(),
        ..Default::default()
    };
    brief.add_reference(
        "target".into(),
        ImageInput {
            mime_type: "image/png".into(),
            data: vec![1, 2, 3],
        },
    );
    brief.attach(&mut cfg).unwrap();
    tool_session(&mut w, &cfg, "thicken arm", &mut model, &mut renderer).unwrap();
    assert_eq!(w.source, last);
    assert_eq!(renderer.seen, vec![(rev(&next), View::Back)]);
}

#[test]
fn tool_errors_return_to_model_without_mutating_source() {
    let mut count = 0;
    let mut model = |cfg: &GenerateConfig| {
        count += 1;
        Ok(response(if count == 1 {
            "{\"tool\":\"apply\",\"revision\":\"stale\",\"edits\":\"invalid\"}".into()
        } else {
            assert!(cfg.user_prompt.contains("Stale"));
            json!({"tool":"finish","revision":rev(ORIGINAL),"findings":"could not improve"})
                .to_string()
        }))
    };
    let mut w = ModelingWorkspace::new(ORIGINAL.into(), None, vec![], None).unwrap();
    tool_session(
        &mut w,
        &GenerateConfig::new("x"),
        "x",
        &mut model,
        &mut Renderer {
            seen: vec![],
            fail: false,
        },
    )
    .unwrap();
    assert_eq!(w.source, ORIGINAL);
}

#[test]
fn tool_step_limit_follows_configured_call_budget_not_a_fixed_constant() {
    // Regression: the correction loop used to run a hardcoded 24 iterations
    // regardless of the session's configured `--calls` budget. A model that
    // never calls `finish` must now be stopped exactly at the configured
    // call limit, whether that's smaller or larger than the old constant.
    for calls in [2u32, 30] {
        let mut count = 0u32;
        let mut model = |_: &GenerateConfig| {
            count += 1;
            Ok(response(
                json!({"tool":"inspect","revision":rev(ORIGINAL),"name":"seat"}).to_string(),
            ))
        };
        let mut w = ModelingWorkspace::new(ORIGINAL.into(), None, vec![], None).unwrap();
        let mut cfg = GenerateConfig::new("x");
        cfg.session_control = Some(SessionControl::new(SessionLimits {
            calls,
            ..Default::default()
        }));
        let err = tool_session(
            &mut w,
            &cfg,
            "x",
            &mut model,
            &mut Renderer {
                seen: vec![],
                fail: false,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("Modeling tool step limit reached"),
            "err = {err}"
        );
        assert_eq!(count, calls, "calls budget = {calls}");
        assert_eq!(w.source, ORIGINAL);
    }
}

#[test]
fn six_quality_targets_compile_and_techniques_are_retrievable() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benches/quality");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("tasks.json")).unwrap()).unwrap();
    assert_eq!(manifest["tasks"].as_array().unwrap().len(), 7);
    for task in manifest["tasks"].as_array().unwrap() {
        let path = root.join(task["source"].as_str().unwrap());
        compile(&std::fs::read_to_string(&path).unwrap(), path.parent())
            .unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
    }
    for topic in [
        "upholstery",
        "hollow vessel",
        "organic",
        "curved frame",
        "shaped surface",
    ] {
        assert!(documentation(topic).unwrap().contains("scene {"));
    }
}

#[test]
fn closeup_framing_uses_parent_world_transform_and_selected_subtree() {
    let scene = compile(
        "scene { group \"frame\" (pos=[3,0,0]) { box \"part\" (size=[0.2,0.2,0.2]) } }",
        None,
    )
    .unwrap();
    let (center, radius) = part_framing(&scene, "part").unwrap();
    assert!((center[0] - 3.0).abs() < 1e-5);
    assert!(radius < 0.2);
    assert!(part_framing(&scene, "missing").is_err());
}

#[test]
fn measurements_report_broken_joints_and_serialize_revision() {
    let source = "scene { box \"foot\" (size=[0.2,0.04,0.2],pos=[0,0.02,0]) box \"leg\" (size=[0.06,0.4,0.06],pos=[0,0.25,0]) }";
    let mut w = ModelingWorkspace::new(source.into(), None, vec![], None).unwrap();
    let r = w.revision().unwrap();
    let m = w.measure(&r, "foot", "leg", Some(0.002), None).unwrap();
    assert_eq!(m["units"], "m");
    assert_eq!(m["surface"]["status"], "separated");
    assert!((m["surface"]["distance"].as_f64().unwrap() - 0.01).abs() < 1e-6);
    let restored: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(restored["revision"], r);
    assert!(w.inspect(None).unwrap()["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == "E1101"));
    w.source = source.replace("pos=[0,0.25,0]", "pos=[0,0.24,0]");
    assert!(w.measure(&r, "foot", "leg", None, None).is_err());
    let m = w
        .measure(&w.revision().unwrap(), "foot", "leg", None, None)
        .unwrap();
    assert_eq!(m["surface"]["status"], "within_tolerance");
}

#[test]
fn measurements_recheck_module_relationships_and_world_transforms() {
    let source = "module \"joint\" () { box \"rail\" (size=[0.3,0.1,0.2],pos=[0,0.5,0]) box \"tenon\" (size=[0.08,0.3,0.08]) relate (child=\"tenon\",target=\"rail\",mode=\"align\",plug=\"top\",socket=\"bottom\",insertion=0.02) } scene { use \"joint\" (pos=[2,0,0],rot=[0,30,0]) }";
    for input in [
        source.to_owned(),
        source.replace("pos=[0,0.5,0]", "pos=[0,0.8,0]"),
    ] {
        let w = ModelingWorkspace::new(input.clone(), None, vec![], None).unwrap();
        let m = w
            .measure(&w.revision().unwrap(), "tenon", "rail", None, None)
            .unwrap();
        assert_eq!(
            m["relationship_checks"][0]["intent"],
            "intentional_insertion"
        );
        assert_eq!(m["relationship_checks"][0]["satisfied"], true);
        let mut scene = compile(&input, None).unwrap();
        let child = scene.relationships[0].child;
        scene.nodes[child.0 as usize].transform.translation.y += 0.1;
        assert!(!mogen_core::relationship_measurements(&scene)[0].satisfied);
    }
}

#[test]
fn fit_fixture_covers_curved_separation_and_grounding_edits() {
    let source = include_str!("../../../../../examples/features/joint_measurements.mog");
    let w = ModelingWorkspace::new(source.into(), None, vec![], None).unwrap();
    let m = w
        .measure(&w.revision().unwrap(), "curved_a", "curved_b", None, None)
        .unwrap();
    assert_eq!(m["surface"]["status"], "separated");
    let source = include_str!("../../../../../examples/furniture/relational_chair.mog");
    let mut scene = compile(source, None).unwrap();
    let r = scene
        .relationships
        .iter()
        .find(|r| r.mode == "ground")
        .unwrap()
        .clone();
    assert!(mogen_core::relationship_measurements(&scene)
        .iter()
        .filter(|r| r.mode == "ground")
        .all(|r| r.satisfied));
    scene.nodes[r.child.0 as usize].transform.translation.y += 0.01;
    let child_name = scene.get(r.child).name.clone();
    assert!(mogen_core::relationship_measurements(&scene)
        .iter()
        .any(|r| r.child == child_name && !r.satisfied));
}
