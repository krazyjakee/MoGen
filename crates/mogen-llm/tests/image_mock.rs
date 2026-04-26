//! Drive `GeminiClient::generate_image` against a `tiny_http` mock that speaks
//! the subset of the Gemini image response shape the client decodes.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use mogen_llm::gemini::GeminiClient;
use mogen_llm::textures::generate_with_recitation_retry;

struct MockServer {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    _handle: thread::JoinHandle<()>,
}

impl MockServer {
    fn start(response_body: String, status: u16) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
        let port = server.server_addr().to_ip().expect("ipv4").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_clone = requests.clone();
        let handle = thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).ok();
                requests_clone.lock().unwrap().push(body);
                let resp = tiny_http::Response::from_string(response_body.clone())
                    .with_status_code(status)
                    .with_header(
                        "Content-Type: application/json"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    );
                let _ = req.respond(resp);
            }
        });
        Self { port, requests, _handle: handle }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1beta", self.port)
    }
}

/// Like `MockServer` but pops a different response per request, so a single
/// test can exercise a retry loop without spinning up multiple servers.
struct SequencedMockServer {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    _handle: thread::JoinHandle<()>,
}

impl SequencedMockServer {
    fn start(responses: Vec<(String, u16)>) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
        let port = server.server_addr().to_ip().expect("ipv4").port();
        let queue: Arc<Mutex<VecDeque<(String, u16)>>> =
            Arc::new(Mutex::new(responses.into()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_clone = requests.clone();
        let handle = thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).ok();
                requests_clone.lock().unwrap().push(body);
                let (resp_body, status) = queue
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| (r#"{"candidates":[]}"#.to_string(), 200));
                let resp = tiny_http::Response::from_string(resp_body)
                    .with_status_code(status)
                    .with_header(
                        "Content-Type: application/json"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    );
                let _ = req.respond(resp);
            }
        });
        Self { port, requests, _handle: handle }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1beta", self.port)
    }
}

fn recitation_response() -> String {
    serde_json::json!({
        "candidates": [{
            "finishReason": "IMAGE_RECITATION",
            "safetyRatings": []
        }]
    })
    .to_string()
}

fn image_response(mime: &str, bytes: &[u8]) -> String {
    let b64 = STANDARD.encode(bytes);
    serde_json::json!({
        "candidates": [{
            "content": {
                "parts": [
                    { "text": "here is an image" },
                    { "inlineData": { "mimeType": mime, "data": b64 } }
                ]
            }
        }]
    })
    .to_string()
}

#[test]
fn decodes_inline_png_bytes() {
    let bytes = b"\x89PNG\r\n\x1a\n fake png body";
    let server = MockServer::start(image_response("image/png", bytes), 200);
    let client = GeminiClient::with_base_url("k", server.base_url());

    let img = client
        .generate_image("gemini-2.5-flash-image", "seamless albedo of oak", None)
        .expect("ok");

    assert_eq!(img.png_bytes, bytes);
    assert_eq!(img.mime_type, "image/png");

    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&reqs[0]).unwrap();
    assert_eq!(
        sent["generationConfig"]["responseModalities"][0],
        "IMAGE"
    );
    assert_eq!(sent["contents"][0]["parts"][0]["text"], "seamless albedo of oak");
}

#[test]
fn rejects_non_image_mime() {
    let server = MockServer::start(image_response("text/plain", b"hello"), 200);
    let client = GeminiClient::with_base_url("k", server.base_url());
    let err = client
        .generate_image("m", "p", None)
        .expect_err("should reject non-image mime");
    assert!(err.to_string().contains("text/plain"), "got: {err}");
}

#[test]
fn empty_response_surfaces_error() {
    let empty = serde_json::json!({ "candidates": [] }).to_string();
    let server = MockServer::start(empty, 200);
    let client = GeminiClient::with_base_url("k", server.base_url());
    let err = client.generate_image("m", "p", None).expect_err("empty");
    assert!(err.to_string().contains("empty response"), "got: {err}");
}

#[test]
fn filtered_candidate_surfaces_finish_reason() {
    // Gemini returns a candidate with no `content` when output is filtered.
    let body = serde_json::json!({
        "candidates": [{
            "finishReason": "IMAGE_SAFETY",
            "safetyRatings": []
        }]
    })
    .to_string();
    let server = MockServer::start(body, 200);
    let client = GeminiClient::with_base_url("k", server.base_url());
    let err = client.generate_image("m", "p", None).expect_err("filtered");
    let s = err.to_string();
    assert!(s.contains("IMAGE_SAFETY"), "got: {s}");
    assert!(s.contains("finishReason"), "got: {s}");
}

#[test]
fn recitation_retry_succeeds_after_first_failure() {
    let bytes = b"\x89PNG\r\n\x1a\n png after retry";
    let server = SequencedMockServer::start(vec![
        (recitation_response(), 200),
        (image_response("image/png", bytes), 200),
    ]);
    let client = GeminiClient::with_base_url("k", server.base_url());

    let img = generate_with_recitation_retry(&client, "m", "albedo of oak", 3, None)
        .expect("retry should recover");

    assert_eq!(img.png_bytes, bytes);

    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 2, "second call should fire after recitation");
    let second: serde_json::Value = serde_json::from_str(&reqs[1]).unwrap();
    let prompt = second["contents"][0]["parts"][0]["text"].as_str().unwrap();
    assert!(prompt.starts_with("albedo of oak"), "base prompt preserved");
    assert!(prompt.contains("Variation hint:"), "retry adds a variation hint");
}

#[test]
fn recitation_retry_gives_up_after_max_attempts() {
    let server = SequencedMockServer::start(vec![
        (recitation_response(), 200),
        (recitation_response(), 200),
        (recitation_response(), 200),
    ]);
    let client = GeminiClient::with_base_url("k", server.base_url());

    let err = generate_with_recitation_retry(&client, "m", "p", 2, None)
        .expect_err("should give up");
    let s = err.to_string();
    assert!(s.contains("IMAGE_RECITATION"), "got: {s}");
    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 3, "1 initial + 2 retries");
}

#[test]
fn api_error_status_propagates() {
    let body = r#"{"error":{"message":"quota exceeded"}}"#.to_string();
    let server = MockServer::start(body, 429);
    let client = GeminiClient::with_base_url("k", server.base_url());
    let err = client.generate_image("m", "p", None).expect_err("429");
    let s = err.to_string();
    assert!(s.contains("429"), "got: {s}");
    assert!(s.contains("quota exceeded"), "got: {s}");
}
