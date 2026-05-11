//! End-to-end tests that drive the Gemini client + repair loop against a
//! `tiny_http` mock. The mock speaks the subset of the `generateContent`
//! response shape the client decodes.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;

use mogen_llm::gemini::GeminiClient;
use mogen_llm::{
    generate_edits_with_repair, generate_with_repair, GenerateConfig, ImageInput, LlmClient,
    Provider, RepairConfig,
};

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
    let client = LlmClient::with_base_url(Provider::Gemini, "test-key", server.base_url());

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
    let client = LlmClient::with_base_url(Provider::Gemini, "test-key", server.base_url());

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
    let client = LlmClient::with_base_url(Provider::Gemini, "test-key", server.base_url());

    let outcome = generate_with_repair(
        &client,
        GenerateConfig::new("anything"),
        &RepairConfig { max_iters: 1, on_iteration: None, on_chunk: None, allow_edit_mode: false },
    )
    .expect("request ok");

    assert!(!outcome.is_ok());
    assert_eq!(outcome.call_count, 2);
    assert!(outcome.diagnostics.iter().any(|d| d.code == "E0101"));
}

#[test]
fn fenced_markdown_output_is_stripped_before_validation() {
    let fenced = "```mogen\nscene { box \"b\" (size=[1,1,1]) }\n```";
    let server = MockServer::start(vec![&candidate_body(fenced)]);
    let client = LlmClient::with_base_url(Provider::Gemini, "test-key", server.base_url());

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
fn user_images_become_inline_data_parts_on_the_user_turn() {
    // Image-to-3D path: an attached image should serialize as a sibling
    // `inline_data` part on the user turn, base64-encoded, alongside the text
    // prompt. The legacy text-only request shape is preserved when no image
    // is attached (covered by the other tests in this file).
    let dsl = "scene { box \"b\" (size=[1,1,1]) }";
    let server = MockServer::start(vec![&candidate_body(dsl)]);
    let client = LlmClient::with_base_url(Provider::Gemini, "test-key", server.base_url());

    let mut cfg = GenerateConfig::new("a box");
    cfg.user_images.push(ImageInput {
        mime_type: "image/png".into(),
        // Three bytes the test can spot once base64-encoded ("AQID").
        data: vec![0x01, 0x02, 0x03],
    });
    let _ = generate_with_repair(&client, cfg, &RepairConfig { max_iters: 0, on_iteration: None, on_chunk: None, allow_edit_mode: false })
        .expect("request ok");

    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&reqs[0]).expect("valid JSON");
    let parts = body["contents"][0]["parts"].as_array().expect("parts array");
    // Image part comes first, text part second — vision-prompt convention.
    assert_eq!(parts.len(), 2, "got: {body}");
    assert_eq!(parts[0]["inline_data"]["mime_type"], "image/png");
    assert_eq!(parts[0]["inline_data"]["data"], "AQID");
    assert_eq!(parts[1]["text"], "a box");
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

#[test]
fn planner_then_coder_threads_plan_into_user_turn() {
    // The Architect/Coder split is the reason `--plan` exists. The
    // Architect emits Markdown; the Coder pass is supposed to translate
    // that plan into DSL while still seeing the original prompt. If the
    // wiring drops either piece — plan into the user turn, or original
    // prompt — the value of the split collapses. This test pins the
    // contract end-to-end against the real client + repair stack.
    let plan_text = "## Subject\nA tall wooden stool with four legs.\n\n## Parts\n- seat (top)\n- four legs splayed slightly outward";
    let coder_dsl = "scene { box \"seat\" (size=[0.4, 0.05, 0.4]) }";
    let server = MockServer::start(vec![
        &candidate_body(plan_text),
        &candidate_body(coder_dsl),
    ]);
    let client = LlmClient::with_base_url(Provider::Gemini, "test-key", server.base_url());

    let base = mogen_llm::GenerateConfig::new("a wooden stool");
    let plan = mogen_llm::generate_plan(&client, &base, "a wooden stool")
        .expect("plan call ok");
    assert!(plan.plan.contains("four legs"));

    let coder_user_prompt = mogen_llm::compose_coder_prompt("a wooden stool", &plan.plan);
    let mut coder_cfg = mogen_llm::GenerateConfig::new(coder_user_prompt);
    coder_cfg.model = base.model.clone();
    let outcome = generate_with_repair(
        &client,
        coder_cfg,
        &RepairConfig { max_iters: 0, on_iteration: None, on_chunk: None, allow_edit_mode: false },
    )
    .expect("coder call ok");
    assert!(outcome.is_ok(), "diagnostics: {:?}", outcome.diagnostics);

    // Two HTTP requests landed; the second one (the Coder) must contain
    // both the original prompt and the architect's plan body verbatim.
    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 2, "expected planner + coder calls");
    let coder_req = &reqs[1];
    assert!(
        coder_req.contains("a wooden stool"),
        "Coder turn must keep the original prompt: {coder_req}"
    );
    assert!(
        coder_req.contains("four legs splayed slightly outward"),
        "Coder turn must inline the architect's plan body: {coder_req}"
    );
    assert!(
        coder_req.contains("Plan:"),
        "Coder turn must mark the plan section so the model can find it: {coder_req}"
    );
}

#[test]
fn visual_refine_attaches_image_and_dsl() {
    // The Reviewer agent's whole job is "look at this image of what your
    // last DSL produced, fix it". If the image is dropped or the previous
    // DSL never makes it into the user turn, the critique is text-only —
    // exactly the failure mode `visual_refine` exists to avoid. Pin the
    // wiring here so a refactor can't silently strip either piece.
    let revised = "scene { box \"seat\" (size=[0.4, 0.05, 0.4]) }";
    let server = MockServer::start(vec![&candidate_body(revised)]);
    let client = LlmClient::with_base_url(Provider::Gemini, "test-key", server.base_url());

    let registry = mogen_dsl::stdlib_registry();
    let base = mogen_llm::GenerateConfig::new("a wooden stool");
    let image = ImageInput {
        mime_type: "image/png".into(),
        // Three bytes the test can spot once base64-encoded ("AQID").
        data: vec![0x01, 0x02, 0x03],
    };
    let previous_dsl = "scene { box \"old\" (size=[1, 1, 1]) }";
    let outcome = mogen_llm::visual_refine(
        &client,
        &base,
        &RepairConfig { max_iters: 0, on_iteration: None, on_chunk: None, allow_edit_mode: false },
        registry,
        "a wooden stool",
        previous_dsl,
        image,
    )
    .expect("reviewer call ok");
    assert!(outcome.is_ok());

    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1, "reviewer should issue exactly one call");
    let body: serde_json::Value = serde_json::from_str(&reqs[0]).expect("valid JSON");
    let parts = body["contents"][0]["parts"]
        .as_array()
        .expect("user-turn parts array");
    // Vision convention: image part first, text part second.
    assert_eq!(parts.len(), 2, "user turn must carry image+text: {body}");
    assert_eq!(parts[0]["inline_data"]["mime_type"], "image/png");
    assert_eq!(parts[0]["inline_data"]["data"], "AQID");
    let text = parts[1]["text"].as_str().expect("text part is string");
    assert!(
        text.contains("a wooden stool"),
        "reviewer turn must keep the original prompt: {text}"
    );
    assert!(
        text.contains("box \"old\""),
        "reviewer turn must inline the previous DSL so the model can edit it: {text}"
    );
}

/// Spin up a tiny_http server that responds to every request with an
/// SSE-framed body and the `text/event-stream` content type. `frames`
/// is the list of JSON payloads that go into `data: {…}\n\n` events,
/// in order. Used by the streaming-client tests to verify that
/// `stream_generate` correctly accumulates deltas, surfaces usage from
/// the tail frame, and invokes the per-chunk callback once per frame.
fn start_sse_server(frames: Vec<String>) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind sse server");
    let port = server.server_addr().to_ip().expect("ipv4").port();
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let requests_clone = requests.clone();
    thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).ok();
            requests_clone.lock().unwrap().push(body);
            let mut payload = String::new();
            for frame in &frames {
                payload.push_str("data: ");
                payload.push_str(frame);
                payload.push_str("\n\n");
            }
            payload.push_str("data: [DONE]\n\n");
            let resp = tiny_http::Response::from_string(payload).with_header(
                "Content-Type: text/event-stream"
                    .parse::<tiny_http::Header>()
                    .unwrap(),
            );
            let _ = req.respond(resp);
        }
    });
    (port, requests)
}

#[test]
fn gemini_stream_accumulates_deltas_and_invokes_on_chunk_each_frame() {
    // Three SSE frames: two text deltas, then a tail frame carrying the
    // usage tally only (Gemini emits usageMetadata on the final frame).
    // The client must concatenate the text parts and surface the tail
    // frame's usage in the returned `GenerateResponse`.
    let f1 = serde_json::json!({
        "candidates": [{"content":{"parts":[{"text":"scene { box \"a\" "}]}}],
    })
    .to_string();
    let f2 = serde_json::json!({
        "candidates": [{"content":{"parts":[{"text":"(size=[1,1,1]) }"}]}}],
    })
    .to_string();
    let tail = serde_json::json!({
        "candidates": [{"content":{"parts":[{"text":""}]}}],
        "usageMetadata": {
            "promptTokenCount": 11,
            "candidatesTokenCount": 22,
            "totalTokenCount": 33,
        }
    })
    .to_string();
    let (port, _reqs) = start_sse_server(vec![f1, f2, tail]);

    let client = GeminiClient::with_base_url(
        "test-key",
        format!("http://127.0.0.1:{}/v1beta", port),
    );

    let cumulative_snapshots: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut on_chunk = |_delta: &str, cumulative: &str| {
        cumulative_snapshots
            .lock()
            .unwrap()
            .push(cumulative.to_string());
    };

    let resp = client
        .stream_generate(&GenerateConfig::new("a box"), &mut on_chunk)
        .expect("stream ok");

    assert_eq!(resp.text, "scene { box \"a\" (size=[1,1,1]) }");
    assert_eq!(resp.usage.total_tokens, 33);
    assert_eq!(resp.usage.prompt_tokens, 11);
    assert_eq!(resp.usage.response_tokens, 22);

    // Two text frames → two callbacks (the tail's empty-string delta is
    // skipped so the panel doesn't get a no-op transition).
    let snaps = cumulative_snapshots.lock().unwrap();
    assert_eq!(snaps.len(), 2, "got: {snaps:?}");
    assert_eq!(snaps[0], "scene { box \"a\" ");
    assert_eq!(snaps[1], "scene { box \"a\" (size=[1,1,1]) }");
}

#[test]
fn gemini_stream_targets_the_stream_endpoint_with_sse_alt() {
    // The streaming path must hit `:streamGenerateContent?alt=sse`, not
    // `:generateContent`. If a future refactor accidentally aliases the
    // method to the non-streaming URL the deltas would never arrive
    // — pin the URL shape against the mock's recorded request.
    let frame = serde_json::json!({
        "candidates": [{"content":{"parts":[{"text":"x"}]}}],
        "usageMetadata": {"promptTokenCount":1, "candidatesTokenCount":1, "totalTokenCount":2}
    })
    .to_string();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let urls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let urls_clone = urls.clone();
    thread::spawn(move || {
        for req in server.incoming_requests() {
            urls_clone.lock().unwrap().push(req.url().to_string());
            let body = format!("data: {frame}\n\ndata: [DONE]\n\n");
            let resp = tiny_http::Response::from_string(body).with_header(
                "Content-Type: text/event-stream"
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
    let mut cb = |_d: &str, _c: &str| {};
    let _ = client
        .stream_generate(&GenerateConfig::new("x"), &mut cb)
        .expect("stream ok");

    let urls = urls.lock().unwrap();
    assert_eq!(urls.len(), 1);
    assert!(
        urls[0].contains(":streamGenerateContent"),
        "expected streaming endpoint, got {}",
        urls[0]
    );
    assert!(urls[0].contains("alt=sse"), "expected SSE alt, got {}", urls[0]);
}

#[test]
fn openai_stream_accumulates_deltas_and_surfaces_tail_usage() {
    // OpenAI streams `choices[0].delta.content` deltas, then a
    // choice-less tail frame with `usage` when `include_usage` is set.
    // Mirror the same accumulation+tail-usage contract Gemini's test
    // exercises so a regression in either provider is caught by its
    // own test.
    let f1 = serde_json::json!({
        "choices": [{"index":0, "delta":{"content":"scene { box \"a\" "}}],
    })
    .to_string();
    let f2 = serde_json::json!({
        "choices": [{"index":0, "delta":{"content":"(size=[1,1,1]) }"}}],
    })
    .to_string();
    let tail = serde_json::json!({
        "choices": [],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 12,
            "total_tokens": 17,
        }
    })
    .to_string();
    let (port, reqs) = start_sse_server(vec![f1, f2, tail]);

    let client = LlmClient::with_base_url(
        Provider::OpenAI,
        "test-key",
        format!("http://127.0.0.1:{}/v1", port),
    );

    let cumulative_snapshots: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut on_chunk = |_delta: &str, cumulative: &str| {
        cumulative_snapshots
            .lock()
            .unwrap()
            .push(cumulative.to_string());
    };
    let resp = client
        .stream_generate(&GenerateConfig::new("a box"), &mut on_chunk)
        .expect("stream ok");

    assert_eq!(resp.text, "scene { box \"a\" (size=[1,1,1]) }");
    assert_eq!(resp.usage.total_tokens, 17);
    assert_eq!(resp.usage.prompt_tokens, 5);
    assert_eq!(resp.usage.response_tokens, 12);

    // Two non-empty deltas → two callbacks.
    let snaps = cumulative_snapshots.lock().unwrap();
    assert_eq!(snaps.len(), 2, "got: {snaps:?}");

    // Request body must carry `stream:true` and request usage on the tail.
    let reqs = reqs.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&reqs[0]).expect("valid JSON");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

/// Variant of [`start_sse_server`] that hands out a different frame
/// sequence per incoming request (the repair-loop test needs distinct
/// streaming responses across iterations). `responses[i]` is the SSE
/// frames returned to request `i`; once exhausted, additional requests
/// are answered with an empty stream.
fn start_sse_server_per_request(
    responses: Vec<Vec<String>>,
) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind sse server");
    let port = server.server_addr().to_ip().expect("ipv4").port();
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let requests_clone = requests.clone();
    let queue = std::sync::Arc::new(std::sync::Mutex::new(responses));
    let queue_clone = queue.clone();
    thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).ok();
            requests_clone.lock().unwrap().push(body);
            let frames = {
                let mut q = queue_clone.lock().unwrap();
                if q.is_empty() {
                    Vec::new()
                } else {
                    q.remove(0)
                }
            };
            let mut payload = String::new();
            for frame in &frames {
                payload.push_str("data: ");
                payload.push_str(frame);
                payload.push_str("\n\n");
            }
            payload.push_str("data: [DONE]\n\n");
            let resp = tiny_http::Response::from_string(payload).with_header(
                "Content-Type: text/event-stream"
                    .parse::<tiny_http::Header>()
                    .unwrap(),
            );
            let _ = req.respond(resp);
        }
    });
    (port, requests)
}

/// Mock that answers `:streamGenerateContent` with SSE `sse_frames` and
/// any other path (notably `:generateContent`) with `non_streaming_json`.
/// Used to exercise the repair loop's transparent stream-to-non-stream
/// fallback: streaming hits the bad endpoint, non-streaming hits the
/// good one.
fn start_gemini_dual_endpoint_mock(
    sse_frames: Vec<String>,
    non_streaming_json: String,
) -> u16 {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind dual mock");
    let port = server.server_addr().to_ip().expect("ipv4").port();
    let sse_frames = std::sync::Arc::new(sse_frames);
    let non_streaming_json = std::sync::Arc::new(non_streaming_json);
    thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).ok();
            let url = req.url().to_string();
            if url.contains(":streamGenerateContent") {
                let mut payload = String::new();
                for frame in sse_frames.iter() {
                    payload.push_str("data: ");
                    payload.push_str(frame);
                    payload.push_str("\n\n");
                }
                payload.push_str("data: [DONE]\n\n");
                let resp = tiny_http::Response::from_string(payload).with_header(
                    "Content-Type: text/event-stream"
                        .parse::<tiny_http::Header>()
                        .unwrap(),
                );
                let _ = req.respond(resp);
            } else {
                let resp = tiny_http::Response::from_string((*non_streaming_json).clone())
                    .with_header(
                        "Content-Type: application/json"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    );
                let _ = req.respond(resp);
            }
        }
    });
    port
}

#[test]
fn repair_loop_falls_back_to_non_streaming_when_stream_returns_truncated_text() {
    // Server-side truncation (max_tokens, content filter, body
    // closed after a brief preamble) is a real production failure
    // mode that the SSE accumulator can't detect — from its
    // perspective the stream completed cleanly. The repair loop has
    // to look at the returned text length and treat a few-byte
    // "successful" response as a stream-transport hiccup, re-issuing
    // the call against the non-streaming endpoint so the user gets
    // the proper response instead of having their file overwritten
    // with a junk preamble like "I'll modify…".
    let truncated = "I'll modify";
    let truncated_frame = serde_json::json!({
        "candidates": [{"content":{"parts":[{"text": truncated}]}}],
        "usageMetadata": {"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2},
    })
    .to_string();
    let good_dsl = "scene { box \"b\" (size=[1,1,1]) }";
    let non_streaming = serde_json::json!({
        "candidates": [{"content":{"parts":[{"text": good_dsl}]}}],
        "usageMetadata": {
            "promptTokenCount": 8,
            "candidatesTokenCount": 16,
            "totalTokenCount": 24,
        }
    })
    .to_string();
    let port = start_gemini_dual_endpoint_mock(vec![truncated_frame], non_streaming);
    let client = LlmClient::with_base_url(
        Provider::Gemini,
        "test-key",
        format!("http://127.0.0.1:{}/v1beta", port),
    );

    let outcome = generate_with_repair(
        &client,
        GenerateConfig::new("a box"),
        &RepairConfig {
            max_iters: 0,
            on_iteration: None,
            on_chunk: Some(Box::new(|_d, _c| {})),
            allow_edit_mode: false,
        },
    )
    .expect("fallback should rescue the call");

    assert!(outcome.is_ok(), "diagnostics: {:?}", outcome.diagnostics);
    assert!(outcome.dsl.contains("box \"b\""), "got: {:?}", outcome.dsl);
}

#[test]
fn repair_loop_falls_back_to_non_streaming_when_stream_returns_long_prose_preamble() {
    // A 60+ byte prose preamble (well past the old 30-byte length gate)
    // that doesn't contain edit-block markers and doesn't parse as DSL.
    // The content-aware `stream_response_is_useful` predicate rejects it
    // so the loop falls back to the non-streaming endpoint, which here
    // returns valid DSL — proving the gate isn't being defeated by long
    // preambles the way the old length-only gate was.
    let prose_preamble = "Sure, I'll help with that. Let me think about how to modify the file to satisfy your request.";
    let prose_frame = serde_json::json!({
        "candidates": [{"content":{"parts":[{"text": prose_preamble}]}}],
        "usageMetadata": {"promptTokenCount":4,"candidatesTokenCount":20,"totalTokenCount":24},
    })
    .to_string();
    let good_dsl = "scene { box \"b\" (size=[1,1,1]) }";
    let non_streaming = serde_json::json!({
        "candidates": [{"content":{"parts":[{"text": good_dsl}]}}],
        "usageMetadata": {
            "promptTokenCount": 8,
            "candidatesTokenCount": 16,
            "totalTokenCount": 24,
        }
    })
    .to_string();
    let port = start_gemini_dual_endpoint_mock(vec![prose_frame], non_streaming);
    let client = LlmClient::with_base_url(
        Provider::Gemini,
        "test-key",
        format!("http://127.0.0.1:{}/v1beta", port),
    );

    let outcome = generate_with_repair(
        &client,
        GenerateConfig::new("a box"),
        &RepairConfig {
            max_iters: 0,
            on_iteration: None,
            on_chunk: Some(Box::new(|_d, _c| {})),
            allow_edit_mode: false,
        },
    )
    .expect("fallback should rescue the call");

    assert!(outcome.is_ok(), "diagnostics: {:?}", outcome.diagnostics);
    assert!(outcome.dsl.contains("box \"b\""), "got: {:?}", outcome.dsl);
}

#[test]
fn repair_loop_falls_back_to_non_streaming_when_stream_errors() {
    // Streaming endpoint returns zero text-bearing frames — the
    // accumulator surfaces an `EmptyResponse` error. The repair loop
    // must transparently re-issue the call against the non-streaming
    // endpoint and use *that* response as the source of truth. Without
    // this fallback, a single streaming hiccup turns into a hard
    // failure even when the underlying `:generateContent` endpoint is
    // perfectly healthy — which is exactly what was happening in
    // production when Gemini/OpenAI returned an unparseable
    // intermediate frame.
    let bad_stream = vec![]; // forces the SSE accumulator to see only `[DONE]`
    let good_dsl = "scene { box \"b\" (size=[1,1,1]) }";
    let non_streaming = serde_json::json!({
        "candidates": [{"content":{"parts":[{"text": good_dsl}]}}],
        "usageMetadata": {
            "promptTokenCount": 8,
            "candidatesTokenCount": 16,
            "totalTokenCount": 24,
        }
    })
    .to_string();
    let port = start_gemini_dual_endpoint_mock(bad_stream, non_streaming);
    let client = LlmClient::with_base_url(
        Provider::Gemini,
        "test-key",
        format!("http://127.0.0.1:{}/v1beta", port),
    );

    let chunk_fires: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
    let chunk_fires_cb = chunk_fires.clone();
    let outcome = generate_with_repair(
        &client,
        GenerateConfig::new("a box"),
        &RepairConfig {
            max_iters: 0,
            on_iteration: None,
            on_chunk: Some(Box::new(move |_d, _c| {
                *chunk_fires_cb.lock().unwrap() += 1;
            })),
            allow_edit_mode: false,
        },
    )
    .expect("fallback should rescue the call");

    assert!(outcome.is_ok(), "diagnostics: {:?}", outcome.diagnostics);
    assert!(outcome.dsl.contains("box \"b\""), "got: {:?}", outcome.dsl);
    // The bad stream had no text frames, so on_chunk should never
    // have fired even once — confirms the fallback didn't double-emit
    // progress from streaming's partial state.
    assert_eq!(*chunk_fires.lock().unwrap(), 0);
}

fn gemini_stream_frame(text: &str) -> String {
    serde_json::json!({
        "candidates": [{"content":{"parts":[{"text": text}]}}],
    })
    .to_string()
}

fn gemini_stream_tail(prompt: u32, candidates: u32, total: u32) -> String {
    serde_json::json!({
        "candidates": [{"content":{"parts":[{"text":""}]}}],
        "usageMetadata": {
            "promptTokenCount": prompt,
            "candidatesTokenCount": candidates,
            "totalTokenCount": total,
        }
    })
    .to_string()
}

#[test]
fn repair_loop_streams_each_iteration_and_invokes_on_chunk() {
    // The repair loop must route through `stream_generate` whenever
    // `on_chunk` is set, on every iteration — not just the first.
    // A refactor that accidentally calls `client.generate` after the
    // initial attempt would silently lose progress updates mid-repair.
    // This test pins the contract by serving two streaming responses
    // (a bad one, then a fixed one) and asserting `on_chunk` fires
    // across both.
    let bad = "scene { wombat \"oops\" (size=[1,1,1]) }";
    let good = "scene { box \"b\" (size=[1,1,1]) }";
    let attempt1 = vec![
        gemini_stream_frame(bad),
        gemini_stream_tail(10, 20, 30),
    ];
    let attempt2 = vec![
        gemini_stream_frame(good),
        gemini_stream_tail(10, 20, 30),
    ];
    let (port, _reqs) = start_sse_server_per_request(vec![attempt1, attempt2]);
    let client = LlmClient::with_base_url(
        Provider::Gemini,
        "test-key",
        format!("http://127.0.0.1:{}/v1beta", port),
    );

    // `on_chunk` is boxed into a `'static` callback, so the shared
    // buffer it writes through must outlive the closure — `Arc` here,
    // not a stack-local `Mutex`.
    let snapshots: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let snapshots_cb = snapshots.clone();
    let outcome = generate_with_repair(
        &client,
        GenerateConfig::new("a box"),
        &RepairConfig {
            max_iters: 1,
            on_iteration: None,
            on_chunk: Some(Box::new(move |_delta, cumulative| {
                snapshots_cb.lock().unwrap().push(cumulative.to_string());
            })),
            allow_edit_mode: false,
        },
    )
    .expect("request ok");

    assert!(outcome.is_ok(), "diagnostics: {:?}", outcome.diagnostics);
    assert_eq!(outcome.call_count, 2);

    // One non-empty text frame per attempt → at least one snapshot per
    // call, with the second iteration's payload distinct from the first.
    let snaps = snapshots.lock().unwrap();
    assert!(
        snaps.iter().any(|s| s.contains("wombat")),
        "expected first-attempt text in snapshots: {snaps:?}"
    );
    assert!(
        snaps.iter().any(|s| s.contains("box \"b\"")),
        "expected repaired text in snapshots: {snaps:?}"
    );
}

#[test]
fn gemini_stream_tolerates_intermediate_frame_without_content() {
    // Real Gemini streams interleave text-bearing frames with metadata
    // frames that carry only `finishReason` (and sometimes only
    // `safetyRatings`) — no `content` block. If the SSE accumulator
    // treats the absent field as a parse error it aborts the entire
    // stream and the caller sees a one-token response. Replay that
    // exact shape: first frame text, second frame finish-only, then a
    // usage tail. Expect the full text to round-trip and usage to land.
    let text_frame = serde_json::json!({
        "candidates": [{"content":{"parts":[{"text":"scene { box \"a\" (size=[1,1,1]) }"}], "role":"model"}, "index":0}],
    })
    .to_string();
    let finish_frame = serde_json::json!({
        "candidates": [{"finishReason":"STOP", "index":0}],
    })
    .to_string();
    let tail = serde_json::json!({
        "usageMetadata": {
            "promptTokenCount": 7,
            "candidatesTokenCount": 14,
            "totalTokenCount": 21,
        }
    })
    .to_string();
    let (port, _reqs) = start_sse_server(vec![text_frame, finish_frame, tail]);
    let client = GeminiClient::with_base_url(
        "test-key",
        format!("http://127.0.0.1:{}/v1beta", port),
    );
    let mut cb = |_d: &str, _c: &str| {};
    let resp = client
        .stream_generate(&GenerateConfig::new("a box"), &mut cb)
        .expect("stream must survive content-less frames");
    assert_eq!(resp.text, "scene { box \"a\" (size=[1,1,1]) }");
    assert_eq!(resp.usage.total_tokens, 21);
}

#[test]
fn gemini_stream_enforces_budget_tokens_against_tail_usage() {
    // Streaming must apply the same client-side budget cap as the
    // non-streaming path. Tail-frame usage of 33 against a 20-token
    // budget should produce `BudgetExceeded`, not a successful response.
    let frames = vec![
        gemini_stream_frame("scene { box \"a\" (size=[1,1,1]) }"),
        gemini_stream_tail(11, 22, 33),
    ];
    let (port, _reqs) = start_sse_server(frames);
    let client = GeminiClient::with_base_url(
        "test-key",
        format!("http://127.0.0.1:{}/v1beta", port),
    );

    let mut cfg = GenerateConfig::new("a box");
    cfg.budget_tokens = Some(20);
    let mut cb = |_d: &str, _c: &str| {};
    let err = client
        .stream_generate(&cfg, &mut cb)
        .expect_err("budget should trip");
    assert!(
        matches!(
            err,
            mogen_llm::gemini::GeminiError::BudgetExceeded { used: 33, budget: 20 }
        ),
        "expected BudgetExceeded, got {err:?}",
    );
}

#[test]
fn openai_stream_tolerates_null_delta_and_unparseable_intermediate_frame() {
    // Two production-realistic failure modes to verify a single bad
    // frame doesn't tear down the stream:
    // 1. `"delta": null` on a finish-reason frame — emitted by some
    //    OpenAI-compatible gateways and historically by OpenAI's own
    //    reasoning models. `null` can't deserialize into a struct, so
    //    a naive `serde_json::from_str::<RawChatChunk>` rejects it.
    // 2. A complete-garbage interleaved frame (server-side error
    //    envelope, dropped chunk, partial body). The accumulator must
    //    skip it and keep reading.
    // Expect the full text from the surrounding good frames to land.
    let text_frame = serde_json::json!({
        "choices": [{"index":0, "delta":{"content":"scene { box \"a\" "}}],
    })
    .to_string();
    let null_delta_frame = serde_json::json!({
        "choices": [{"index":0, "delta": serde_json::Value::Null, "finish_reason":"stop"}],
    })
    .to_string();
    let garbage_frame = String::from("not even json");
    let text_frame_2 = serde_json::json!({
        "choices": [{"index":0, "delta":{"content":"(size=[1,1,1]) }"}}],
    })
    .to_string();
    let tail = serde_json::json!({
        "choices": [],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 12,
            "total_tokens": 17,
        }
    })
    .to_string();
    let (port, _reqs) = start_sse_server(vec![
        text_frame,
        null_delta_frame,
        garbage_frame,
        text_frame_2,
        tail,
    ]);
    let client = LlmClient::with_base_url(
        Provider::OpenAI,
        "test-key",
        format!("http://127.0.0.1:{}/v1", port),
    );
    let mut cb = |_d: &str, _c: &str| {};
    let resp = client
        .stream_generate(&GenerateConfig::new("a box"), &mut cb)
        .expect("stream must survive a null delta and a garbage interleaved frame");
    assert_eq!(resp.text, "scene { box \"a\" (size=[1,1,1]) }");
    assert_eq!(resp.usage.total_tokens, 17);
}

#[test]
fn openai_stream_enforces_budget_tokens_against_tail_usage() {
    // Same contract as the Gemini test above, exercised through the
    // OpenAI streaming path so a regression in either client surfaces
    // as a dedicated failure rather than an unexplained drift in the
    // generic `LlmClient` wrapper.
    let f1 = serde_json::json!({
        "choices": [{"index":0, "delta":{"content":"scene { box \"a\" (size=[1,1,1]) }"}}],
    })
    .to_string();
    let tail = serde_json::json!({
        "choices": [],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 12,
            "total_tokens": 17,
        }
    })
    .to_string();
    let (port, _reqs) = start_sse_server(vec![f1, tail]);
    let client = LlmClient::with_base_url(
        Provider::OpenAI,
        "test-key",
        format!("http://127.0.0.1:{}/v1", port),
    );
    let mut cfg = GenerateConfig::new("a box");
    cfg.budget_tokens = Some(10);
    let mut cb = |_d: &str, _c: &str| {};
    let err = client
        .stream_generate(&cfg, &mut cb)
        .expect_err("budget should trip");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("BudgetExceeded") && msg.contains("17") && msg.contains("10"),
        "expected BudgetExceeded with 17/10, got {msg}",
    );
}

/// Helper that produces a Z.ai-shaped chat-completions response carrying
/// `text` as the assistant's reply. Mirrors `candidate_body` for Gemini.
fn zai_chat_body(text: &str) -> String {
    serde_json::json!({
        "choices": [
            { "message": { "role": "assistant", "content": text } }
        ],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 20,
            "total_tokens": 30,
        }
    })
    .to_string()
}

#[test]
fn zai_vision_uses_image_url_content() {
    // Vision input on Z.ai must serialise as the OpenAI-compatible
    // `content: [{type:"text"}, {type:"image_url", image_url:{url:"data:..."}}]`
    // shape — the Java backend rejects raw `inline_data` parts (Gemini's
    // wire format) on `glm-5v-turbo`. Pin the contract so a refactor
    // can't silently regress the wire shape.
    let dsl = "scene { box \"b\" (size=[1,1,1]) }";
    let server = MockServer::start(vec![&zai_chat_body(dsl)]);
    let client = LlmClient::with_base_url(Provider::Zai, "test-key", server.base_url());

    let mut cfg = GenerateConfig::new("describe");
    cfg.model = mogen_llm::ZAI_DEFAULT_VISION_MODEL.to_string();
    cfg.user_images.push(ImageInput {
        mime_type: "image/png".into(),
        // Three bytes that base64-encode to "AQID".
        data: vec![0x01, 0x02, 0x03],
    });
    let _ = generate_with_repair(&client, cfg, &RepairConfig { max_iters: 0, on_iteration: None, on_chunk: None, allow_edit_mode: false })
        .expect("request ok");

    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&reqs[0]).expect("valid JSON");
    assert_eq!(body["model"], mogen_llm::ZAI_DEFAULT_VISION_MODEL);
    let messages = body["messages"].as_array().expect("messages array");
    let user = messages.last().expect("user turn");
    assert_eq!(user["role"], "user");
    let parts = user["content"].as_array().expect("content array on vision turn");
    assert_eq!(parts.len(), 2, "got: {body}");
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "describe");
    assert_eq!(parts[1]["type"], "image_url");
    assert_eq!(
        parts[1]["image_url"]["url"],
        "data:image/png;base64,AQID"
    );
}

#[test]
fn fireworks_vision_uses_image_url_content() {
    // Vision input on Fireworks must serialise as the OpenAI-compatible
    // `content: [{type:"text"}, {type:"image_url", image_url:{url:"data:..."}}]`
    // shape — Kimi K2.5 / K2.6 are native multimodal and accept this wire
    // format via Fireworks' OpenAI-compatible chat endpoint. The Z.ai-shaped
    // helper response works here because both providers run the same
    // OpenAI-compatible Chat Completions surface.
    let dsl = "scene { box \"b\" (size=[1,1,1]) }";
    let server = MockServer::start(vec![&zai_chat_body(dsl)]);
    let client =
        LlmClient::with_base_url(Provider::Fireworks, "test-key", server.base_url());

    let mut cfg = GenerateConfig::new("describe");
    cfg.model = "accounts/fireworks/routers/kimi-k2p6".to_string();
    cfg.user_images.push(ImageInput {
        mime_type: "image/png".into(),
        // Three bytes that base64-encode to "AQID".
        data: vec![0x01, 0x02, 0x03],
    });
    let _ = generate_with_repair(&client, cfg, &RepairConfig { max_iters: 0, on_iteration: None, on_chunk: None, allow_edit_mode: false })
        .expect("request ok");

    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&reqs[0]).expect("valid JSON");
    assert_eq!(body["model"], "accounts/fireworks/routers/kimi-k2p6");
    let messages = body["messages"].as_array().expect("messages array");
    let user = messages.last().expect("user turn");
    assert_eq!(user["role"], "user");
    let parts = user["content"].as_array().expect("content array on vision turn");
    assert_eq!(parts.len(), 2, "got: {body}");
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "describe");
    assert_eq!(parts[1]["type"], "image_url");
    assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AQID");
}

#[test]
fn edit_mode_first_call_applies_search_replace_against_baseline() {
    // The model returns a single SEARCH/REPLACE block. The repair loop
    // should apply it against the supplied baseline and treat the result as
    // the candidate DSL — no full rewrite required, dramatically smaller
    // response payload than re-emitting the entire file.
    let baseline = "scene { box \"b\" (size=[1,1,1]) }\n";
    let edit_block = "<<<<<<< SEARCH\nsize=[1,1,1]\n=======\nsize=[2,2,2]\n>>>>>>> REPLACE\n";
    let server = MockServer::start(vec![&candidate_body(edit_block)]);
    let client = LlmClient::with_base_url(Provider::Gemini, "test-key", server.base_url());

    let outcome = generate_edits_with_repair(
        &client,
        GenerateConfig::new("make it bigger"),
        &RepairConfig::default(),
        baseline,
    )
    .expect("request ok");

    assert!(outcome.is_ok(), "diagnostics: {:?}", outcome.diagnostics);
    assert_eq!(outcome.call_count, 1, "edit mode should resolve in one call");
    assert!(
        outcome.dsl.contains("size=[2,2,2]"),
        "edit block should mutate the baseline: {}",
        outcome.dsl
    );
    assert!(
        !outcome.dsl.contains("size=[1,1,1]"),
        "old size should be replaced: {}",
        outcome.dsl
    );
}

#[test]
fn edit_mode_falls_back_to_rewrite_when_model_returns_full_dsl() {
    // Some models ignore the SEARCH/REPLACE format and emit a full file
    // anyway (especially when the requested edit is structural). The loop
    // must transparently treat a parseable DSL response as a rewrite,
    // otherwise edit-mode would regress the legacy modify happy path.
    let baseline = "scene { box \"b\" (size=[1,1,1]) }\n";
    let full_rewrite = "scene { sphere \"s\" (radius=2) }";
    let server = MockServer::start(vec![&candidate_body(full_rewrite)]);
    let client = LlmClient::with_base_url(Provider::Gemini, "test-key", server.base_url());

    let outcome = generate_edits_with_repair(
        &client,
        GenerateConfig::new("turn it into a sphere"),
        &RepairConfig::default(),
        baseline,
    )
    .expect("request ok");

    assert!(outcome.is_ok(), "diagnostics: {:?}", outcome.diagnostics);
    assert_eq!(outcome.call_count, 1);
    // The model's whole-file rewrite wins, not the baseline.
    assert!(
        outcome.dsl.contains("sphere"),
        "full-file rewrite should pass through: {}",
        outcome.dsl
    );
    assert!(
        !outcome.dsl.contains("box"),
        "baseline should not leak into the response: {}",
        outcome.dsl
    );
}

// ---------------------------------------------------------------------------
// Trace: what does the Studio Modify path actually do when the model emits a
// realistic-but-unhelpful response? Runs `generate_edits_with_repair` against
// a mock that streams a typical "preamble + non-matching SEARCH block" and
// prints the candidate DSL + diagnostics at each iteration so we can watch
// where the response goes wrong. Run with:
//
//   cargo test -p mogen-llm --test mock_server trace_modify_preamble_then_bad_edit_block -- --nocapture
// ---------------------------------------------------------------------------
#[test]
fn trace_modify_preamble_then_bad_edit_block() {
    // Baseline matches what a real `.mog` looks like — a meta header plus a
    // small scene. The non-matching SEARCH block paraphrases the source so
    // it doesn't match byte-for-byte, which is the dominant real failure
    // mode for SEARCH/REPLACE responses. `seed` is a string per the meta
    // schema (the production `embed_seed_header` writes it that way).
    let baseline = "meta(seed=\"1\", prompt=\"a box\")\n\
                    scene {\n  \
                      box \"b\" (size=[1,1,1])\n\
                    }\n";
    // Model response: a 60+ byte preamble (well past the 30-byte gate),
    // then a SEARCH block whose SEARCH text uses different whitespace from
    // the baseline so `apply_edit_blocks` rejects it.
    let response_text = "Sure — I'll make the box taller.\n\n\
                         <<<<<<< SEARCH\n\
                         box \"b\"(size=[1,1,1])\n\
                         =======\n\
                         box \"b\" (size=[1,2,1])\n\
                         >>>>>>> REPLACE\n";

    // Per-iteration counters so we can see whether streaming or non-streaming
    // fired. Both endpoints return the SAME response — real-world failure mode.
    let stream_calls = Arc::new(Mutex::new(0u32));
    let nonstream_calls = Arc::new(Mutex::new(0u32));
    let stream_clone = stream_calls.clone();
    let nonstream_clone = nonstream_calls.clone();

    let port = {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
        let port = server.server_addr().to_ip().expect("ipv4").port();
        let frame = serde_json::json!({
            "candidates": [{"content":{"parts":[{"text": response_text}]}}],
            "usageMetadata": {"promptTokenCount":10,"candidatesTokenCount":40,"totalTokenCount":50},
        })
        .to_string();
        let non_streaming = frame.clone();
        thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).ok();
                let url = req.url().to_string();
                if url.contains(":streamGenerateContent") {
                    *stream_clone.lock().unwrap() += 1;
                    let mut payload = String::new();
                    payload.push_str("data: ");
                    payload.push_str(&frame);
                    payload.push_str("\n\ndata: [DONE]\n\n");
                    let resp = tiny_http::Response::from_string(payload).with_header(
                        "Content-Type: text/event-stream"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    );
                    let _ = req.respond(resp);
                } else {
                    *nonstream_clone.lock().unwrap() += 1;
                    let resp = tiny_http::Response::from_string(non_streaming.clone())
                        .with_header(
                            "Content-Type: application/json"
                                .parse::<tiny_http::Header>()
                                .unwrap(),
                        );
                    let _ = req.respond(resp);
                }
            }
        });
        port
    };

    let client = LlmClient::with_base_url(
        Provider::Gemini,
        "test-key",
        format!("http://127.0.0.1:{}/v1beta", port),
    );

    // on_iteration prints the diagnostics the loop saw between calls.
    let on_iter = Box::new(|iter: u32, diags: &[mogen_core::Diagnostic]| {
        eprintln!("\n--- repair iteration {iter} starting ---");
        for d in diags {
            eprintln!(
                "  diag [{}] {:?} {}",
                d.code, d.severity, d.message
            );
        }
    });

    let chunk_fires = Arc::new(Mutex::new(0u32));
    let chunk_fires_cb = chunk_fires.clone();

    eprintln!("=== baseline ===");
    eprintln!("{baseline}");
    eprintln!("=== model response (literal, will be returned by EVERY call) ===");
    eprintln!("{response_text}");
    eprintln!("=== response length (trimmed) = {} bytes (>= 30 byte gate? {}) ===",
        response_text.trim().len(),
        response_text.trim().len() >= 30,
    );

    let outcome = generate_edits_with_repair(
        &client,
        GenerateConfig::new("make the box taller"),
        &RepairConfig {
            max_iters: 2,
            on_iteration: Some(on_iter),
            on_chunk: Some(Box::new(move |_d, c| {
                *chunk_fires_cb.lock().unwrap() += 1;
                eprintln!("on_chunk: cumulative={:?}", c);
            })),
            allow_edit_mode: true,
        },
        baseline,
    )
    .expect("loop should not error — it must return Ok with diagnostics");

    eprintln!("\n=== FINAL OUTCOME ===");
    eprintln!("call_count   = {}", outcome.call_count);
    eprintln!("stream_calls = {}", *stream_calls.lock().unwrap());
    eprintln!("nonstream    = {}", *nonstream_calls.lock().unwrap());
    eprintln!("chunk_fires  = {}", *chunk_fires.lock().unwrap());
    eprintln!("is_ok        = {}", outcome.is_ok());
    eprintln!("diagnostics  = {} entry/entries", outcome.diagnostics.len());
    for d in &outcome.diagnostics {
        eprintln!("  [{}] {}", d.code, d.message);
    }
    eprintln!("--- outcome.dsl (this is what apply_llm_outcome writes to f.source) ---");
    eprintln!("{}", outcome.dsl);
    eprintln!("--- end outcome.dsl ---");

    // Post-fix contract: the loop should NEVER hand a caller back markup
    // pretending to be DSL. When `apply_edit_blocks` fails the loop must
    //   1) keep the previous (valid) DSL as the candidate, and
    //   2) surface a synthetic E0801 diagnostic so the next iteration has
    //      something actionable.
    // The pre-fix behaviour returned the edit-block markup as `outcome.dsl`,
    // which `apply_llm_outcome` then wrote to the user's file. These
    // assertions pin the new contract.
    assert!(
        !outcome.is_ok(),
        "loop should still flag the call as failed",
    );
    assert!(
        !outcome.dsl.contains("<<<<<<< SEARCH"),
        "outcome.dsl must NOT carry edit-block markup forward (would clobber the user's file): {}",
        outcome.dsl,
    );
    assert!(
        outcome.dsl.contains("box \"b\""),
        "outcome.dsl should be the baseline DSL when apply fails, got: {}",
        outcome.dsl,
    );
    assert!(
        outcome.diagnostics.iter().any(|d| d.code == "E0801"),
        "expected an E0801 synthetic diagnostic in: {:?}",
        outcome.diagnostics,
    );
}

