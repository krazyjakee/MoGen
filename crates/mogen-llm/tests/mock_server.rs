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
