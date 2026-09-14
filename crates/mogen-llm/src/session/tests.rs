use super::*;
use crate::{GenerateConfig, GenerateResponse, ImageInput, Usage};
use serde_json::json;
use std::time::Duration;
const ORIGINAL:&str="material \"wood\" (color=[0.5,0.3,0.1])\nscene { box \"seat\" (size=[1,0.2,1],mat=\"wood\") box \"arm\" (size=[0.1,0.1,1],pos=[0.45,0.1,0],mat=\"wood\") }";
fn rev(source: &str) -> String {
    revision(source, &Default::default())
}
fn response(text: impl Into<String>) -> GenerateResponse {
    GenerateResponse {
        text: text.into(),
        usage: Usage {
            prompt_tokens: 10,
            response_tokens: 10,
            total_tokens: 20,
            cached_tokens: 0,
        },
    }
}
struct Renderer {
    seen: Vec<(String, View)>,
    fail: bool,
}
impl SessionRenderer for Renderer {
    fn render(&mut self, _source: &str, revision: &str, view: View) -> anyhow::Result<ImageInput> {
        if self.fail {
            anyhow::bail!("fixture render failure");
        }
        self.seen.push((revision.into(), view));
        Ok(ImageInput {
            mime_type: "image/png".into(),
            data: format!("{revision}-{}", view.label()).into_bytes(),
        })
    }
}
#[test]
fn limits_cancel_deadline_calls_and_unknown_spend() {
    let cfg = GenerateConfig::new("chair");
    let c = SessionControl::new(SessionLimits {
        calls: 1,
        ..Default::default()
    });
    c.before_call(&cfg, None).unwrap();
    assert!(c.before_call(&cfg, None).is_err());
    assert_eq!(c.meter().calls, 1);
    let c = SessionControl::new(Default::default());
    c.cancel();
    assert!(c.before_call(&cfg, None).is_err());
    assert_eq!(c.meter().calls, 0);
    let c = SessionControl::new(SessionLimits {
        seconds: 5,
        ..Default::default()
    });
    assert!(c.check_at(Duration::from_secs(5)).is_err());
    assert!(c.before_call(&cfg, None).is_err());
    let c = SessionControl::new(SessionLimits {
        spend_usd: Some(0.01),
        ..Default::default()
    });
    assert!(c.before_call(&cfg, None).is_err());
    assert_eq!(c.meter().calls, 0);
    let c = SessionControl::new(SessionLimits {
        spend_usd: Some(0.01),
        ..Default::default()
    });
    assert!(c
        .before_call(
            &cfg,
            Some(crate::spend::pricing::TextPricing::flat(
                100.0, 100.0, 100.0
            ))
        )
        .is_err());
}
#[test]
fn atomic_edits_preserve_locks_and_unrelated_text() {
    let lock = PartLock {
        name: "seat".into(),
        kind: LockKind::Subtree,
    };
    let mut w =
        ModelingWorkspace::new(ORIGINAL.into(), None, vec![lock], Some("arm".into())).unwrap();
    let old = w.revision().unwrap();
    let next = ORIGINAL.replace("size=[0.1,0.1,1]", "size=[0.2,0.1,1]");
    assert!(w.apply("stale", &next).is_err());
    assert_eq!(w.source, ORIGINAL);
    assert!(w
        .apply(&old, &ORIGINAL.replace("size=[1,0.2,1]", "size=[2,0.2,1]"))
        .is_err());
    assert!(w
        .apply(
            &old,
            &ORIGINAL.replace("color=[0.5,0.3,0.1]", "color=[1,0,0]")
        )
        .is_err());
    assert!(w
        .apply(
            &old,
            "<<<<<<< SEARCH\nnot present\n=======\nx\n>>>>>>> REPLACE"
        )
        .is_err());
    assert_eq!(w.source, ORIGINAL);
    w.apply(&old, &next).unwrap();
    assert_eq!(w.source, next);
    assert!(w.apply(&old, ORIGINAL).is_err());
}
#[test]
fn shared_material_and_parent_transform_cannot_bypass_locks() {
    let locks = vec![PartLock {
        name: "seat".into(),
        kind: LockKind::Subtree,
    }];
    assert!(enforce_locks(
        ORIGINAL,
        &ORIGINAL.replace("color=[0.5,0.3,0.1]", "color=[1,0,0]"),
        &locks,
        None
    )
    .is_err());
    let src = "scene { group \"frame\" { box \"seat\" (size=[1,1,1]) } }";
    assert!(enforce_locks(
        src,
        &src.replace("group \"frame\"", "group \"frame\" (pos=[0,1,0])"),
        &locks,
        None
    )
    .is_err());
}
#[test]
fn imported_dependencies_are_captured_and_stale_changes_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let module = "module \"chair\" () { box \"seat\" (size=[1,1,1]) }";
    std::fs::write(dir.path().join("chair.mog"), module).unwrap();
    let source = "import \"chair.mog\"\nscene { use \"chair\" () }";
    let w = ModelingWorkspace::new(source.into(), Some(dir.path().into()), vec![], None).unwrap();
    let old = w.revision().unwrap();
    assert_eq!(dependencies(source, Some(dir.path())).unwrap().len(), 1);
    std::fs::write(
        dir.path().join("chair.mog"),
        module.replace("1,1,1", "2,1,1"),
    )
    .unwrap();
    assert!(w.check_revision(&old).is_err());
    assert!(dependencies("import \"missing.mog\"\nscene {}", Some(dir.path())).is_err());
}
#[test]
fn project_roundtrip_preserves_reference_bytes_and_brief() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chair.mog");
    let mut p = ModelingProject::default();
    p.brief.prompt = "carved chair".into();
    p.brief.dimensions = "0.9m tall".into();
    p.brief.corrections.push("thicker arms".into());
    p.brief.add_reference(
        "front target".into(),
        ImageInput {
            mime_type: "image/png".into(),
            data: vec![1, 2, 3],
        },
    );
    p.save(&path).unwrap();
    let q = ModelingProject::load(&path).unwrap();
    assert_eq!(q.brief.references[0].image.data, vec![1, 2, 3]);
    assert_eq!(q.brief.corrections, p.brief.corrections);
    assert_eq!(q.brief.dimensions, p.brief.dimensions);
    let mut cfg = GenerateConfig::new("modify");
    q.brief.attach(&mut cfg).unwrap();
    assert!(cfg.user_prompt.contains("carved chair"));
    assert_eq!(cfg.user_images[0].data, vec![1, 2, 3]);
}
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
fn refinement_rejects_dependency_changes_during_capture_or_review() {
    struct ChangingRenderer {
        path: std::path::PathBuf,
        change_during_render: bool,
        renders: usize,
    }
    impl SessionRenderer for ChangingRenderer {
        fn render(&mut self, _: &str, _: &str, _: View) -> anyhow::Result<ImageInput> {
            self.renders += 1;
            if self.change_during_render && self.renders == 3 {
                std::fs::write(
                    &self.path,
                    "module \"chair\" () { sphere \"seat\" (radius=1) }",
                )?;
            }
            Ok(ImageInput {
                mime_type: "image/png".into(),
                data: vec![1],
            })
        }
    }
    for during_render in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chair.mog");
        let module = "module \"chair\" () { box \"seat\" (size=[1,1,1]) }";
        std::fs::write(&path, module).unwrap();
        let source = "import \"chair.mog\"\nscene { use \"chair\" () }";
        let mut project = ModelingProject::default();
        let mut reviews = 0;
        let mut model = |_: &GenerateConfig| {
            reviews += 1;
            std::fs::write(&path, "module \"chair\" () { sphere \"seat\" (radius=1) }")?;
            Ok(response(
                json!({"findings":"complete","complete":true,"improved":true,"correction":""})
                    .to_string(),
            ))
        };
        let result = refine_session(
            &mut project,
            source,
            Some(dir.path()),
            &GenerateConfig::new("chair"),
            "fixture",
            &mut model,
            &mut ChangingRenderer {
                path: path.clone(),
                change_during_render: during_render,
                renders: 0,
            },
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(result, source);
        assert_eq!(reviews, if during_render { 0 } else { 1 });
        assert!(project
            .stop_reason
            .contains("Stale source/dependency revision"));
        let candidate = &project.candidates[0];
        assert_eq!(
            candidate.dependencies[std::path::Path::new("chair.mog")],
            module.as_bytes()
        );
        assert_eq!(candidate.views.len(), if during_render { 0 } else { 5 });
        assert!(candidate.findings.contains("awaiting visual review"));
        project.save(&dir.path().join("scene.mog")).unwrap();
        ModelingProject::load(&dir.path().join("scene.mog")).unwrap();
    }
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
fn refinement_rejects_regression_and_keeps_multiview_candidates() {
    let next = ORIGINAL.replace("size=[0.1,0.1,1]", "size=[0.2,0.1,1]");
    let mut p = ModelingProject::default();
    p.limits.iterations = 1;
    let mut count = 0;
    let mut model = |cfg: &GenerateConfig| {
        count += 1;
        let text = match count {
            1 => {
                assert_eq!(cfg.user_images.len(), 5);
                assert!(String::from_utf8_lossy(&cfg.user_images[2].data).contains("back"));
                json!({"findings":"back arm too thin","complete":false,"improved":false,"correction":"thicken arm"})
            }
            2 => json!({"tool":"apply","revision":rev(ORIGINAL),"edits":next}),
            3 => json!({"tool":"finish","revision":rev(&next),"findings":"thicker"}),
            4 => {
                assert_eq!(cfg.user_images.len(), 10);
                json!({"findings":"arm now too thick","complete":false,"improved":false,"correction":"revert"})
            }
            _ => panic!("unexpected call"),
        };
        Ok(response(text.to_string()))
    };
    let mut renderer = Renderer {
        seen: vec![],
        fail: false,
    };
    let result = refine_session(
        &mut p,
        ORIGINAL,
        None,
        &GenerateConfig::new("chair"),
        "fixture",
        &mut model,
        &mut renderer,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result, ORIGINAL);
    assert_eq!(p.candidates.len(), 2);
    assert_eq!(p.candidates[1].views.len(), 5);
    assert!(p.stop_reason.contains("No useful"));
}
#[test]
fn render_failure_and_iteration_exhaustion_keep_valid_work() {
    for fail in [true, false] {
        let mut p = ModelingProject::default();
        p.limits.iterations = 0;
        let mut model = |_: &GenerateConfig| {
            Ok(response(json!({"findings":"missing detail","complete":false,"improved":false,"correction":"add detail"}).to_string()))
        };
        let result = refine_session(
            &mut p,
            ORIGINAL,
            None,
            &GenerateConfig::new("chair"),
            "fixture",
            &mut model,
            &mut Renderer { seen: vec![], fail },
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(result, ORIGINAL);
        assert_eq!(p.candidates.len(), 1);
        assert!(p.stop_reason.contains(if fail {
            "render failure"
        } else {
            "Iteration limit"
        }));
    }
}
#[test]
fn planner_coder_and_reviewer_requests_retain_labeled_reference_bytes() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let address = format!("http://{}", server.server_addr());
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        for text in ["A chair with an arm", ORIGINAL, ORIGINAL] {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .expect("mock request within 5 seconds");
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            tx.send(body).unwrap();
            request.respond(tiny_http::Response::from_string(json!({"choices":[{"message":{"content":text}}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}).to_string()).with_header(tiny_http::Header::from_bytes("Content-Type","application/json").unwrap())).unwrap();
        }
    });
    let client = crate::LlmClient::with_base_url(crate::Provider::OpenAI, "fixture", &address);
    let mut cfg = GenerateConfig::new("");
    cfg.model = "gpt-4.1".into();
    cfg.user_images.push(ImageInput {
        mime_type: "image/png".into(),
        data: vec![1, 2, 3],
    });
    cfg.spend_context =
        crate::CallContext::new(crate::Operation::Generate).with_session("fixture-session");
    let plan = crate::generate_plan(&client, &cfg, "").unwrap();
    cfg.user_prompt = crate::compose_coder_prompt("", &plan.plan);
    client.generate(&cfg).unwrap();
    crate::visual_refine(
        &client,
        &cfg,
        &crate::RepairConfig::default(),
        mogen_dsl::stdlib_registry(),
        "chair",
        ORIGINAL,
        ImageInput {
            mime_type: "image/png".into(),
            data: vec![4, 5, 6],
        },
    )
    .unwrap();
    worker.join().unwrap();
    let bodies: Vec<_> = rx.try_iter().collect();
    assert_eq!(bodies.len(), 3);
    for body in &bodies {
        assert!(body.contains("AQID"));
    }
    assert!(bodies[0].contains("original target references"));
    assert!(bodies[2].contains("BAUG"));
    assert!(bodies[2].contains("Image roles"));
}

#[test]
fn recover_dependency_snapshot_without_overwriting_current_project() {
    let dir = tempfile::tempdir().unwrap();
    let module = "module \"chair\" () { box \"seat\" (size=[1,1,1]) }";
    std::fs::write(dir.path().join("chair.mog"), module).unwrap();
    let source = "import \"chair.mog\"\nscene { use \"chair\" () }";
    let mut p = ModelingProject::default();
    let i = p
        .record(
            source.into(),
            Some(dir.path()),
            &GenerateConfig::new("chair"),
            "fixture",
            "valid".into(),
            vec![],
        )
        .unwrap();
    std::fs::write(dir.path().join("chair.mog"), "new external edit").unwrap();
    let restored = p.candidates[i]
        .restore_copy(&dir.path().join("copy"))
        .unwrap();
    compile(
        &std::fs::read_to_string(restored).unwrap(),
        Some(&dir.path().join("copy")),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("chair.mog")).unwrap(),
        "new external edit"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("copy/chair.mog")).unwrap(),
        module
    );
}

#[test]
fn recovery_preserves_dependencies_named_like_the_entry_point() {
    let dir = tempfile::tempdir().unwrap();
    let module = "module \"chair\" () { box \"seat\" (size=[1,1,1]) }";
    std::fs::write(dir.path().join("restored.mog"), module).unwrap();
    std::fs::create_dir(dir.path().join("restored-1.mog")).unwrap();
    std::fs::write(
        dir.path().join("restored-1.mog/other.mog"),
        "module \"other\" () {}",
    )
    .unwrap();
    let source =
        "import \"restored.mog\"\nimport \"restored-1.mog/other.mog\"\nscene { use \"chair\" () }";
    let mut project = ModelingProject::default();
    let i = project
        .record(
            source.into(),
            Some(dir.path()),
            &GenerateConfig::new("chair"),
            "fixture",
            "valid".into(),
            vec![],
        )
        .unwrap();
    let destination = dir.path().join("copy");
    let entry = project.candidates[i].restore_copy(&destination).unwrap();
    assert_eq!(entry, destination.join("restored-2.mog"));
    assert_eq!(
        std::fs::read_to_string(destination.join("restored.mog")).unwrap(),
        module
    );
    let recovered = std::fs::read_to_string(entry).unwrap();
    assert_eq!(
        dependencies(&recovered, Some(&destination)).unwrap(),
        project.candidates[i].dependencies
    );
    compile(&recovered, Some(&destination)).unwrap();
}

#[test]
fn binary_mesh_dependencies_are_scoped_snapshotted_and_revision_checked() {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join("project");
    std::fs::create_dir(&project_dir).unwrap();
    std::fs::write(project_dir.join("part.glb"), b"original mesh bytes").unwrap();
    std::fs::write(project_dir.join("albedo"), b"texture bytes").unwrap();
    let source = "material \"surface\" (base_color_texture=albedo)\nscene { mesh \"part\" (src=\"part.glb\") }";
    let workspace =
        ModelingWorkspace::new(source.into(), Some(project_dir.clone()), vec![], None).unwrap();
    let expected = workspace.revision().unwrap();
    let mut project = ModelingProject::default();
    let i = project
        .record(
            source.into(),
            Some(&project_dir),
            &GenerateConfig::new("part"),
            "fixture",
            "snapshot".into(),
            vec![],
        )
        .unwrap();
    let candidate = &project.candidates[i];
    assert_eq!(candidate.dependencies.len(), 2);
    assert_eq!(
        candidate.dependencies[std::path::Path::new("part.glb")],
        b"original mesh bytes"
    );
    std::fs::write(project_dir.join("part.glb"), b"external edit").unwrap();
    assert!(workspace.check_revision(&expected).is_err());
    let destination = dir.path().join("recovered");
    candidate.restore_copy(&destination).unwrap();
    assert_eq!(
        std::fs::read(destination.join("part.glb")).unwrap(),
        b"original mesh bytes"
    );
    std::fs::write(dir.path().join("outside.glb"), b"outside project").unwrap();
    let error = compile(
        "scene { mesh \"part\" (src=\"../outside.glb\") }",
        Some(&project_dir),
    )
    .unwrap_err();
    assert!(error.to_string().contains("outside the modeling project"));
}
#[test]
fn six_quality_targets_compile_and_techniques_are_retrievable() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benches/quality");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("tasks.json")).unwrap()).unwrap();
    assert_eq!(manifest["tasks"].as_array().unwrap().len(), 6);
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
fn unsupported_images_fail_before_call_admission() {
    let client = crate::LlmClient::new(crate::Provider::Ollama, "");
    let mut cfg = GenerateConfig::new("");
    cfg.user_images.push(ImageInput {
        mime_type: "image/png".into(),
        data: vec![1],
    });
    let c = SessionControl::new(Default::default());
    cfg.session_control = Some(c.clone());
    assert!(matches!(
        client.generate(&cfg),
        Err(crate::ProviderError::Unsupported { .. })
    ));
    assert_eq!(c.meter().calls, 0);
}
#[test]
fn cancellation_during_request_records_usage_and_prevents_repairs() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let address = format!("http://{}", server.server_addr());
    let c = SessionControl::new(Default::default());
    let cancel = c.clone();
    let thread = std::thread::spawn(move || {
        let mut request = server
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body).unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["max_completion_tokens"], 512);
        cancel.cancel();
        request.respond(tiny_http::Response::from_string(json!({"choices":[{"message":{"content":"invalid DSL"}}],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}).to_string())).unwrap();
    });
    let client = crate::LlmClient::with_base_url(crate::Provider::OpenAI, "fixture", &address);
    let mut cfg = GenerateConfig::new("chair");
    cfg.model = "gpt-4.1".into();
    cfg.session_control = Some(c.clone());
    cfg.max_output_tokens = Some(512);
    assert!(crate::generate_with_repair(&client, cfg, &Default::default()).is_err());
    thread.join().unwrap();
    assert_eq!(c.meter().calls, 1);
    assert_eq!(c.meter().usage.total_tokens, 30);
}

#[cfg(unix)]
#[test]
fn cancellation_stops_cli_descendants_even_after_launcher_exits() {
    use std::process::{Command, Stdio};
    for script in ["sleep 3 & wait", "sleep 3 &"] {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_child(&mut command);
        let child = command.spawn().unwrap();
        let control = SessionControl::new(Default::default());
        let cancel = control.clone();
        let started = std::time::Instant::now();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            cancel.cancel();
        });
        wait_for_child(child, Some(&control)).unwrap();
        worker.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "CLI descendant kept its output pipes open after cancellation"
        );
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
fn relational_driver_changes_cannot_bypass_dependent_geometry_locks() {
    let source = "scene { box \"seat\" (pos=[0,0.6,0],size=[1,0.1,1]) spline_tube \"leg\" (points=[[0,0,0],[0,0.3,0]],radius=0.025) } relate (child=\"leg\",target=\"seat\",mode=\"endpoint\",socket=\"bottom\",insertion=0.01)";
    let changed = source.replace("pos=[0,0.6,0]", "pos=[0,0.9,0]");
    let locks = vec![PartLock { name:"leg".into(),kind:LockKind::Geometry }];
    let error = enforce_locks(source,&changed,&locks,None).unwrap_err();
    assert!(error.to_string().contains("locked"));
}

#[test]
fn guided_details_respect_dependent_geometry_locks() {
    let source = include_str!("../../../../examples/furniture/guided_cushion.mog");
    let changed = source.replace("use \"cushion\" ()", "use \"cushion\" (w=1.2)");
    let locks = vec![PartLock { name: "welt".into(), kind: LockKind::Geometry }];
    assert!(enforce_locks(source, &changed, &locks, None).unwrap_err().to_string().contains("locked"));
}

#[test]
fn measurements_report_broken_joints_and_serialize_revision() {
    let source = "scene { box \"foot\" (size=[0.2,0.04,0.2],pos=[0,0.02,0]) box \"leg\" (size=[0.06,0.4,0.06],pos=[0,0.25,0]) }";
    let mut w=ModelingWorkspace::new(source.into(),None,vec![],None).unwrap();
    let r=w.revision().unwrap();
    let m=w.measure(&r,"foot","leg",Some(0.002),None).unwrap();
    assert_eq!(m["units"],"m");
    assert_eq!(m["surface"]["status"],"separated");
    assert!((m["surface"]["distance"].as_f64().unwrap()-0.01).abs()<1e-6);
    let restored:serde_json::Value=serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(restored["revision"],r);
    assert!(w.inspect(None).unwrap()["diagnostics"].as_array().unwrap().iter().any(|d|d["code"]=="E1101"));
    w.source=source.replace("pos=[0,0.25,0]","pos=[0,0.24,0]");
    assert!(w.measure(&r,"foot","leg",None,None).is_err());
    let m=w.measure(&w.revision().unwrap(),"foot","leg",None,None).unwrap();
    assert_eq!(m["surface"]["status"],"within_tolerance");
}

#[test]
fn measurements_recheck_module_relationships_and_world_transforms() {
    let source = "module \"joint\" () { box \"rail\" (size=[0.3,0.1,0.2],pos=[0,0.5,0]) box \"tenon\" (size=[0.08,0.3,0.08]) relate (child=\"tenon\",target=\"rail\",mode=\"align\",plug=\"top\",socket=\"bottom\",insertion=0.02) } scene { use \"joint\" (pos=[2,0,0],rot=[0,30,0]) }";
    for input in [source.to_owned(),source.replace("pos=[0,0.5,0]","pos=[0,0.8,0]")] {
        let w=ModelingWorkspace::new(input.clone(),None,vec![],None).unwrap();
        let m=w.measure(&w.revision().unwrap(),"tenon","rail",None,None).unwrap();
        assert_eq!(m["relationship_checks"][0]["intent"],"intentional_insertion");
        assert_eq!(m["relationship_checks"][0]["satisfied"],true);
        let mut scene=compile(&input,None).unwrap();
        let child=scene.relationships[0].child;
        scene.nodes[child.0 as usize].transform.translation.y+=0.1;
        assert!(!mogen_core::relationship_measurements(&scene)[0].satisfied);
    }
}

#[test]
fn fit_fixture_covers_curved_separation_and_grounding_edits() {
    let source=include_str!("../../../../examples/features/joint_measurements.mog");
    let w=ModelingWorkspace::new(source.into(),None,vec![],None).unwrap();
    let m=w.measure(&w.revision().unwrap(),"curved_a","curved_b",None,None).unwrap();
    assert_eq!(m["surface"]["status"],"separated");
    let source=include_str!("../../../../examples/furniture/relational_chair.mog");
    let mut scene=compile(source,None).unwrap();
    let r=scene.relationships.iter().find(|r|r.mode=="ground").unwrap().clone();
    assert!(mogen_core::relationship_measurements(&scene).iter().filter(|r|r.mode=="ground").all(|r|r.satisfied));
    scene.nodes[r.child.0 as usize].transform.translation.y+=0.01;
    let child_name=scene.get(r.child).name.clone();
    assert!(mogen_core::relationship_measurements(&scene).iter().any(|r|r.child==child_name && !r.satisfied));
}

#[test]
fn measurements_reject_changed_imports() {
    let dir=tempfile::tempdir().unwrap();
    let module="module \"joint\" () { box \"a\" () box \"b\" (pos=[0,1,0]) }";
    std::fs::write(dir.path().join("joint.mog"),module).unwrap();
    let source="import \"joint.mog\"\nscene { use \"joint\" () }";
    let w=ModelingWorkspace::new(source.into(),Some(dir.path().into()),vec![],None).unwrap();
    let old=w.revision().unwrap();
    let result=w.measure(&old,"a","b",None,None).unwrap();
    assert_eq!(result["revision"],old);
    std::fs::write(dir.path().join("joint.mog"),module.replace("0,1,0","0,2,0")).unwrap();
    assert!(w.measure(&old,"a","b",None,None).is_err());
}
