use super::super::*;
use super::support::*;
use crate::{GenerateConfig, GenerateResponse};

#[test]
fn format_retry_is_bounded_and_preserves_raw_evidence() {
    let mut project = ModelingProject::default();
    let mut calls = 0;
    let mut model = |cfg: &GenerateConfig| -> anyhow::Result<GenerateResponse> {
        calls += 1;
        if calls == 2 {
            assert!(cfg.user_images.is_empty());
            assert!(cfg.user_prompt.contains("do not invent"));
        }
        Ok(response("{\"findings\":"))
    };
    let result = refine_session(
        &mut project,
        ORIGINAL,
        None,
        &GenerateConfig::new("chair"),
        "fixture",
        &mut model,
        &mut Renderer {
            seen: vec![],
            fail: false,
        },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(result, ORIGINAL);
    assert!(project.stop_reason.contains("format recovery exhausted"));
    assert_eq!(project.attempts.len(), 2);
    assert!(!project.candidates[0].reviewed);
}

#[test]
fn successful_format_repair_keeps_original_error_and_judgments() {
    let mut project = ModelingProject::default();
    let mut calls = 0;
    refine_session(
        &mut project, ORIGINAL, None, &GenerateConfig::new("chair"), "fixture",
        &mut |_| {
            calls += 1;
            Ok(response(if calls == 1 {
                r#"{"findings":"No remaining defects.","complete":true,"improved":false,"correction":"","extra":"discard"}"#
            } else {
                r#"{"findings":"No remaining defects.","complete":true,"improved":false,"correction":""}"#
            }))
        },
        &mut Renderer { seen: vec![], fail: false }, &mut |_| {},
    ).unwrap();
    assert_eq!(calls, 2);
    assert!(project.candidates[0].reviewed);
    let outcome = project.attempts[0].outcome.as_ref().unwrap();
    assert!(outcome["parse_error"]
        .as_str()
        .unwrap()
        .contains("unknown field"));
    assert_eq!(outcome["findings"], "No remaining defects.");
    assert!(project.attempts[0].provenance.contains("repair"));
}

#[test]
fn oversized_response_is_retained_without_a_repair_call() {
    let mut project = ModelingProject::default();
    let oversized = "x".repeat(MAX_RESPONSE_BYTES + 1);
    let mut calls = 0;
    let result = refine_session(
        &mut project,
        ORIGINAL,
        None,
        &GenerateConfig::new("chair"),
        "fixture",
        &mut |_| {
            calls += 1;
            Ok(response(&oversized))
        },
        &mut Renderer {
            seen: vec![],
            fail: false,
        },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(result, ORIGINAL);
    assert_eq!(project.attempts[0].response, oversized);
    assert_eq!(project.attempts[0].state, "oversized");
    assert!(!project.candidates[0].reviewed);
}
