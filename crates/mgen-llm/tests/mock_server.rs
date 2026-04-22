//! End-to-end tests that drive the Gemini client + repair loop against a
//! `tiny_http` mock. The mock speaks the subset of the `generateContent`
//! response shape the client decodes.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;

use mgen_llm::gemini::{GeminiClient, GenerateConfig};
use mgen_llm::{generate_with_repair, RepairConfig};

struct MockServer {
    port: u16,
    /// Sequence of response texts to hand out, in order.
    responses: Arc<Mutex<Vec<String>>>,
    /// Captured request bodies for assertions.
    requests: Arc<Mutex<Vec<String>>>,
    _handle: thread::JoinHandle<()>,
}

impl MockServer {
    fn start(responses: Vec<&str>) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind server");
        let port = server.server_addr().to_ip().expect("ipv4").port();

        let responses = Arc::new(Mutex::new(
            responses.into_iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        ));
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));

        let responses_clone = responses.clone();
        let requests_clone = requests.clone();
        let handle = thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).ok();
                requests_clone.lock().unwrap().push(body);

                let payload = {
                    let mut q = responses_clone.lock().unwrap();
                    if q.is_empty() {
                        String::from(r#"{"error":{"message":"no more canned responses"}}"#)
                    } else {
                        q.remove(0)
                    }
                };
                let response = tiny_http::Response::from_string(payload)
                    .with_header(
                        "Content-Type: application/json"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    );
                let _ = req.respond(response);
            }
        });

        Self { port, responses, requests, _handle: handle }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1beta", self.port)
    }
}

fn candidate_body(text: &str) -> String {
    serde_json::json!({
        "candidates": [
            {"content": {"parts": [{"text": text}]}}
        ],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 20,
            "totalTokenCount": 30,
        }
    })
    .to_string()
}

#[test]
fn first_call_succeeds_when_dsl_is_valid() {
    let dsl = "scene { box \"b\" (size=[1,1,1]) }";
    let server = MockServer::start(vec![&candidate_body(dsl)]);
    let client = GeminiClient::with_base_url("test-key", server.base_url());

    let outcome = generate_with_repair(
        &client,
        GenerateConfig::new("a box"),
        &RepairConfig::default(),
    )
    .expect("request ok");

    assert!(outcome.is_ok(), "diagnostics: {:?}", outcome.diagnostics);
    assert_eq!(outcome.call_count, 1);
    assert_eq!(outcome.usage.total_tokens, 30);
    assert!(outcome.dsl.contains("scene"));

    // Exactly one outbound request.
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[test]
fn repair_loop_succeeds_on_second_attempt() {
    let bad = "scene { wombat \"oops\" (size=[1,1,1]) }";
    let good = "scene { box \"b\" (size=[1,1,1]) }";
    let server = MockServer::start(vec![&candidate_body(bad), &candidate_body(good)]);
    let client = GeminiClient::with_base_url("test-key", server.base_url());

    let outcome = generate_with_repair(
        &client,
        GenerateConfig::new("a box"),
        &RepairConfig::default(),
    )
    .expect("request ok");

    assert!(outcome.is_ok(), "diagnostics: {:?}", outcome.diagnostics);
    assert_eq!(outcome.call_count, 2);
    // Usage accumulated across both calls.
    assert_eq!(outcome.usage.total_tokens, 60);
    assert_eq!(outcome.dsl, "scene { box \"b\" (size=[1,1,1]) }");

    // Second request folds the prior DSL and diagnostics into a single user
    // turn — no model-role history is carried across iterations.
    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    let second = &reqs[1];
    assert!(second.contains("E0101"), "expected diagnostic code in repair turn: {}", second);
    assert!(second.contains("wombat"), "expected prior DSL inlined in user turn: {}", second);
    assert!(
        !second.contains("\"role\":\"model\""),
        "repair turn should not carry prior model history: {}",
        second
    );
}

#[test]
fn repair_loop_respects_max_iters_and_returns_last_attempt() {
    // Both attempts bad; we cap at 1 repair iter so only 2 calls total.
    let bad = "scene { wombat \"oops\" (size=[1,1,1]) }";
    let server = MockServer::start(vec![&candidate_body(bad), &candidate_body(bad)]);
    let client = GeminiClient::with_base_url("test-key", server.base_url());

    let outcome = generate_with_repair(
        &client,
        GenerateConfig::new("anything"),
        &RepairConfig { max_iters: 1, on_iteration: None },
    )
    .expect("request ok");

    assert!(!outcome.is_ok());
    assert_eq!(outcome.call_count, 2);
    assert!(outcome.diagnostics.iter().any(|d| d.code == "E0101"));
}

#[test]
fn fenced_markdown_output_is_stripped_before_validation() {
    let fenced = "```mgen\nscene { box \"b\" (size=[1,1,1]) }\n```";
    let server = MockServer::start(vec![&candidate_body(fenced)]);
    let client = GeminiClient::with_base_url("test-key", server.base_url());

    let outcome = generate_with_repair(
        &client,
        GenerateConfig::new("a box"),
        &RepairConfig::default(),
    )
    .expect("request ok");

    assert!(outcome.is_ok(), "diagnostics: {:?}", outcome.diagnostics);
    assert!(!outcome.dsl.contains("```"));
}

#[test]
fn api_error_surfaces_with_status_and_message() {
    // Return an error body with HTTP 200 — the client still parses the "error"
    // key when decoding fails? Actually: we simulate a non-2xx. Use a failure body.
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    thread::spawn(move || {
        for req in server.incoming_requests() {
            let body =
                r#"{"error":{"message":"API key not valid","status":"INVALID_ARGUMENT"}}"#;
            let resp = tiny_http::Response::from_string(body)
                .with_status_code(400)
                .with_header(
                    "Content-Type: application/json"
                        .parse::<tiny_http::Header>()
                        .unwrap(),
                );
            let _ = req.respond(resp);
        }
    });

    let client = GeminiClient::with_base_url(
        "test-key",
        format!("http://127.0.0.1:{}/v1beta", port),
    );
    let err = client
        .generate(&GenerateConfig::new("x"))
        .expect_err("should fail");
    let s = err.to_string();
    assert!(s.contains("400"), "got: {s}");
    assert!(s.contains("API key not valid"), "got: {s}");
}
