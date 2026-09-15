use super::super::*;
use super::support::*;
use crate::{GenerateConfig, GenerateResponse, ImageInput};
use serde_json::json;

#[test]
fn new_refinement_captures_its_own_candidates_and_resumes_without_calls() {
    let next = ORIGINAL.replace("0.1,0.1,1", "0.12,0.1,1");
    let mut project = ModelingProject::default();
    let cfg = GenerateConfig::new("chair");
    let mut renderer = Renderer {
        seen: vec![],
        fail: false,
    };
    for run in 0..2 {
        // The same edit can recur after restoring an earlier source, with a
        // changed brief or camera framing. Its captures belong to this run.
        project.brief.corrections.push(format!("Refinement {run}"));
        let mut responses = vec![
            r#"{"findings":"Thin arm","complete":false,"improved":false,"correction":"Thicken arm"}"#.to_string(),
            next.clone(),
            json!({"tool":"finish","revision":rev(&next),"findings":"Thickened"}).to_string(),
            r#"{"findings":"Thicker arm","complete":true,"improved":true,"correction":""}"#.to_string(),
        ].into_iter();
        let result = refine_session(
            &mut project,
            ORIGINAL,
            None,
            &cfg,
            "fixture",
            &mut |_| {
                Ok(response(
                    responses.next().expect("unexpected provider call"),
                ))
            },
            &mut renderer,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(result, next);
        assert!(responses.next().is_none());
        assert_eq!(project.session_initial, Some(run * 2));
        assert_eq!(project.selected_candidate, Some(run * 2 + 1));
        assert_eq!(renderer.seen.len(), (run + 1) * 10);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scene.mog");
    project.save(&path).unwrap();
    let mut project = ModelingProject::load(&path).unwrap();
    let result = resume_session(
        &mut project,
        &next,
        None,
        &cfg,
        "fixture",
        &mut |_| panic!("completed responses must be replayed"),
        &mut Renderer {
            seen: vec![],
            fail: true,
        },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result, next);
    assert_eq!(project.selected_candidate, Some(3));
    assert!(
        !project.stop_reason.starts_with("Stopped:"),
        "{}",
        project.stop_reason
    );
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
        assert_eq!(candidate.views.len(), if during_render { 2 } else { 5 });
        assert!(candidate.findings.contains("awaiting visual review"));
        project.save(&dir.path().join("scene.mog")).unwrap();
        ModelingProject::load(&dir.path().join("scene.mog")).unwrap();
    }
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
fn restart_at_every_durable_stage_reuses_responses_and_captures() {
    let next = ORIGINAL.replace("0.1,0.1,1", "0.12,0.1,1");
    let responses=vec![
        r#"{"findings":["Thin arm."],"complete":false,"improved":false,"correction":"Thicken arm"}"#.to_string(),
        next.clone(),
        json!({"tool":"render","revision":rev(&next),"view":"front"}).to_string(),
        json!({"tool":"finish","revision":rev(&next),"findings":"Arm thickened"}).to_string(),
        r#"{"findings":"Arm is thicker.","complete":true,"improved":true,"correction":""}"#.to_string(),
    ];
    // Crash on each checkpoint in an uninterrupted run, including received,
    // interpreted, applied, captured, reviewed, and selected boundaries.
    for crash_at in 1..36 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scene.mog");
        let mut project = ModelingProject::default();
        let mut cfg = GenerateConfig::new("chair");
        let control = SessionControl::new(project.limits.clone());
        cfg.session_control = Some(control.clone());
        let mut calls = 0;
        let mut checkpoint_count = 0;
        let mut renderer = Renderer {
            seen: vec![],
            fail: false,
        };
        let mut model = |cfg: &GenerateConfig| -> anyhow::Result<GenerateResponse> {
            cfg.session_control
                .as_ref()
                .unwrap()
                .before_call(cfg, None)
                .unwrap();
            let result = response(responses[calls].clone());
            calls += 1;
            cfg.session_control
                .as_ref()
                .unwrap()
                .after_call(Some(&result.usage), None);
            Ok(result)
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            refine_session(
                &mut project,
                ORIGINAL,
                None,
                &cfg,
                "fixture",
                &mut model,
                &mut renderer,
                &mut |p| {
                    checkpoint_count += 1;
                    if checkpoint_count == crash_at {
                        p.save(&path).unwrap();
                        panic!("injected crash");
                    }
                },
            )
        }));
        if !ModelingProject::sidecar(&path).exists() {
            project.save(&path).unwrap();
        }
        let mut saved = ModelingProject::load(&path).unwrap();
        if saved.session_initial.is_none() {
            continue;
        }
        cfg.session_control = Some(SessionControl::resume(
            saved.limits.clone(),
            saved.meter.clone(),
            saved.elapsed_seconds,
        ));
        let mut model = |cfg: &GenerateConfig| -> anyhow::Result<GenerateResponse> {
            cfg.session_control
                .as_ref()
                .unwrap()
                .before_call(cfg, None)
                .unwrap();
            let result = response(responses[calls].clone());
            calls += 1;
            cfg.session_control
                .as_ref()
                .unwrap()
                .after_call(Some(&result.usage), None);
            Ok(result)
        };
        let result = resume_session(
            &mut saved,
            ORIGINAL,
            None,
            &cfg,
            "fixture",
            &mut model,
            &mut renderer,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(result, next, "crash {crash_at}: {}", saved.stop_reason);
        assert_eq!(calls, 5, "crash {crash_at}");
        assert!(
            (5..=6).contains(&saved.meter.calls),
            "crash {crash_at}: interrupted admission reserves an uncertain call"
        );
        assert_eq!(saved.meter.usage.total_tokens, 100, "crash {crash_at}");
        assert_eq!(saved.candidates.len(), 2);
        assert!(saved.candidates[1].reviewed);
        assert_eq!(
            renderer.seen.len(),
            11,
            "completed captures must be reused, crash {crash_at}"
        );
    }
}

#[test]
fn late_raw_edit_is_saved_but_cannot_mutate_cancelled_session() {
    let mut project = ModelingProject::default();
    let mut cfg = GenerateConfig::new("chair");
    let control = SessionControl::new(project.limits.clone());
    cfg.session_control = Some(control.clone());
    let mut calls = 0;
    let next = ORIGINAL.replace("0.1,0.1,1", "0.12,0.1,1");
    let mut model = |_: &GenerateConfig| -> anyhow::Result<GenerateResponse> {
        calls += 1;
        if calls == 1 {
            Ok(response(
                r#"{"findings":[],"complete":false,"improved":false,"correction":"thicken"}"#,
            ))
        } else {
            control.cancel();
            Ok(response(&next))
        }
    };
    let result = refine_session(
        &mut project,
        ORIGINAL,
        None,
        &cfg,
        "fixture",
        &mut model,
        &mut Renderer {
            seen: vec![],
            fail: false,
        },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result, ORIGINAL);
    assert_eq!(project.candidates.len(), 1);
    assert_eq!(project.attempts[1].response, next);
    assert_eq!(project.attempts[1].state, "received");
}

#[test]
fn generation_receipt_survives_restart_without_another_call() {
    let mut p = ModelingProject::default();
    let cfg = GenerateConfig::new("box");
    let mut calls = 0;
    let mut call = |_: &GenerateConfig| {
        calls += 1;
        Ok(response("scene {box}"))
    };
    let source = generate_candidate(&mut p, &cfg, "fixture", &mut call, &mut |_| {}).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("asset.mog");
    p.save(&path).unwrap();
    let mut p = ModelingProject::load(&path).unwrap();
    assert_eq!(
        generate_candidate(&mut p, &cfg, "fixture", &mut call, &mut |_| {}).unwrap(),
        source
    );
    assert_eq!(calls, 1);
}

#[test]
fn exhausted_resume_preserves_the_previously_selected_improvement() {
    let next = ORIGINAL.replace("0.1,0.1,1", "0.12,0.1,1");
    let mut responses = std::collections::VecDeque::from([
        r#"{"findings":"Thin arm.","complete":false,"improved":false,"correction":"Thicken arm"}"#
            .to_string(),
        next.clone(),
        json!({"tool":"finish","revision":rev(&next),"findings":"Thicker arm"}).to_string(),
        r#"{"findings":"Improved.","complete":true,"improved":true,"correction":""}"#.to_string(),
    ]);
    let mut project = ModelingProject::default();
    let mut cfg = GenerateConfig::new("chair");
    let mut renderer = Renderer {
        seen: vec![],
        fail: false,
    };
    let result = refine_session(
        &mut project,
        ORIGINAL,
        None,
        &cfg,
        "fixture",
        &mut |_| Ok(response(responses.pop_front().unwrap())),
        &mut renderer,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result, next);
    let selected = project.selected_candidate;
    cfg.session_control = Some(SessionControl::resume(
        project.limits.clone(),
        project.meter.clone(),
        project.limits.seconds,
    ));
    let result = resume_session(
        &mut project,
        &next,
        None,
        &cfg,
        "fixture",
        &mut |_| panic!("No new call after deadline"),
        &mut renderer,
        &mut |p| assert_eq!(p.selected_candidate, selected),
    )
    .unwrap();
    assert_eq!(result, next);
    assert_eq!(project.selected_candidate, selected);
}
