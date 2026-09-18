use super::super::*;
use crate::{GenerateConfig, ImageInput};
use serde_json::json;
use std::time::Duration;

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
