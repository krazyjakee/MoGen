//! Z.ai (Zhipu) image generation client. Talks to Z.ai's `glm-image` endpoint
//! at `POST {base_url}/images/generations` with an OpenAI-style
//! `{model, prompt, size}` body and a Bearer-token header. The response
//! returns a CDN URL pointing at the generated PNG; we download it inline
//! and surface the bytes through the same [`crate::image::GeneratedImage`]
//! shape as the Gemini path so the textures pipeline stays provider-agnostic.

use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::image::GeneratedImage;

/// Default Z.ai image model. Currently the only ID Z.ai exposes for image
/// generation. Surfaced as the CLI default when `--model` is omitted on
/// the Z.ai path.
pub const DEFAULT_IMAGE_MODEL: &str = "glm-image";

/// Default `size` parameter sent to `glm-image`. 1280×1280 is the documented
/// recommended resolution; the model also accepts 16:9 / 4:3 / 3:4 variants
/// up to 2048×2048. We keep one default here and let the textures pipeline
/// downscale to its `texture_size` knob via [`super::textures`].
pub const DEFAULT_IMAGE_SIZE: &str = "1280x1280";

const DEFAULT_BASE_URL: &str = "https://api.z.ai/api/paas/v4";

#[derive(Debug, Error)]
pub enum ZaiError {
    #[error("missing ZAI_API_KEY (set env var or pass --zai-api-key)")]
    MissingApiKey,
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("empty response: model returned no image data")]
    EmptyResponse,
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

pub struct ZaiClient {
    http: reqwest::blocking::Client,
    api_key: String,
    base_url: String,
}

impl ZaiClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        // Short connect_timeout fails fast when offline; the long overall
        // timeout covers slow image renders + the CDN download that follows.
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self {
            http,
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    pub fn from_env() -> Result<Self, ZaiError> {
        let key = std::env::var("ZAI_API_KEY").map_err(|_| ZaiError::MissingApiKey)?;
        if key.trim().is_empty() {
            return Err(ZaiError::MissingApiKey);
        }
        Ok(Self::new(key))
    }

    /// Issue an image generation call and return decoded PNG bytes.
    ///
    /// `seed` is accepted for API symmetry with [`crate::gemini::GeminiClient`]
    /// but is currently ignored — the public Z.ai `glm-image` surface doesn't
    /// expose a seed parameter, so request-level variation comes from the
    /// prompt alone.
    pub fn generate_image(
        &self,
        model: &str,
        prompt: &str,
        _seed: Option<u64>,
    ) -> Result<GeneratedImage, ZaiError> {
        let url = format!("{}/images/generations", self.base_url);
        let model_name = if model.is_empty() {
            DEFAULT_IMAGE_MODEL
        } else {
            model
        };
        let body = serde_json::json!({
            "model": model_name,
            "prompt": prompt,
            "size": DEFAULT_IMAGE_SIZE,
        });

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()?;
        let status = resp.status();
        let bytes = resp.bytes()?;

        if !status.is_success() {
            return Err(ZaiError::Api {
                status: status.as_u16(),
                message: parse_error_message(&bytes),
            });
        }

        let parsed: RawResponse = serde_json::from_slice(&bytes)
            .map_err(|e| ZaiError::InvalidResponse(e.to_string()))?;

        let image_url = parsed
            .data
            .into_iter()
            .find_map(|d| d.url.filter(|u| !u.is_empty()))
            .ok_or(ZaiError::EmptyResponse)?;

        // Z.ai returns a temporary CDN URL; download the bytes inline so the
        // caller still gets a `GeneratedImage` like the Gemini path.
        //
        // Two real failure modes seen in practice:
        //   1. mfile.z.ai 404s when the request looks "robotic" — empty
        //      User-Agent / no Accept. We send a real UA + Accept image/*.
        //   2. The CDN URL has a short TTL; on a transient 404 we briefly
        //      retry rather than failing the whole material. Retries are
        //      capped tight because a permanent 404 should fail fast.
        let img = download_cdn_image(&self.http, &image_url)?;
        Ok(img)
    }
}

fn download_cdn_image(
    http: &reqwest::blocking::Client,
    image_url: &str,
) -> Result<GeneratedImage, ZaiError> {
    const RETRY_DELAYS_MS: &[u64] = &[200, 800];
    let mut last_status: u16 = 0;
    let mut last_body_snippet = String::new();

    for attempt in 0..=RETRY_DELAYS_MS.len() {
        let resp = http
            .get(image_url)
            .header(
                reqwest::header::USER_AGENT,
                concat!("mogen/", env!("CARGO_PKG_VERSION")),
            )
            .header(reqwest::header::ACCEPT, "image/*")
            .send()?;
        let status = resp.status();
        let mime_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "image/png".to_string());
        let bytes = resp.bytes()?;

        if status.is_success() {
            if !mime_type.starts_with("image/") {
                let snippet: String = String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(200)
                    .collect();
                return Err(ZaiError::InvalidResponse(format!(
                    "expected image/* mime type from CDN, got {mime_type} (body: {snippet})"
                )));
            }
            return Ok(GeneratedImage {
                png_bytes: bytes.to_vec(),
                mime_type,
            });
        }

        last_status = status.as_u16();
        last_body_snippet = String::from_utf8_lossy(&bytes)
            .chars()
            .take(200)
            .collect::<String>()
            .trim()
            .to_string();

        // Only 404 has been observed as transient (TTL race). Other codes
        // fail fast — a 401/403 is a key issue, a 5xx will likely repeat.
        if status.as_u16() != 404 {
            break;
        }
        if attempt < RETRY_DELAYS_MS.len() {
            std::thread::sleep(Duration::from_millis(RETRY_DELAYS_MS[attempt]));
        }
    }

    Err(ZaiError::Api {
        status: last_status,
        message: format!(
            "image CDN download failed: {image_url} (body: {last_body_snippet})"
        ),
    })
}

#[derive(Debug, Deserialize)]
struct RawResponse {
    #[serde(default)]
    data: Vec<RawData>,
}

#[derive(Debug, Deserialize)]
struct RawData {
    #[serde(default)]
    url: Option<String>,
}

/// Pull a human-readable message out of a Z.ai error response. Z.ai's
/// errors come back as `{ "error": { "message": "..." } }` (OpenAI-style)
/// most of the time; we fall back to the raw body when parsing fails so
/// debugging never hits an opaque empty string.
fn parse_error_message(bytes: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if let Some(s) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return s.to_string();
        }
        if let Some(s) = v.get("message").and_then(|m| m.as_str()) {
            return s.to_string();
        }
    }
    String::from_utf8_lossy(bytes).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// Minimal mock for the two-step Z.ai flow (POST generate → GET CDN).
    /// Binds a fresh ephemeral port via `127.0.0.1:0` and reports it back so
    /// the caller can embed a CDN URL pointing at the same listener. Records
    /// every (url, body) tuple so tests can assert on wire shape.
    struct MockServer {
        port: u16,
        requests: Arc<Mutex<Vec<(String, String)>>>,
        _handle: thread::JoinHandle<()>,
    }

    impl MockServer {
        /// `generate_response` is a closure so the body can reference the
        /// port the server is about to bind on (used for embedding the CDN
        /// URL in the JSON response). `status` is the HTTP code returned
        /// from `/images/generations`. Anything not matching that path is
        /// treated as the CDN download and replied to with `png_bytes` +
        /// `image/png`.
        fn start(
            generate_response: impl FnOnce(u16) -> String,
            status: u16,
            png_bytes: Vec<u8>,
        ) -> Self {
            let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
            let port = server.server_addr().to_ip().expect("ipv4").port();
            let body = generate_response(port);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let requests_clone = requests.clone();
            let handle = thread::spawn(move || {
                for mut req in server.incoming_requests() {
                    let url = req.url().to_string();
                    let mut req_body = String::new();
                    req.as_reader().read_to_string(&mut req_body).ok();
                    requests_clone.lock().unwrap().push((url.clone(), req_body));
                    if url.contains("/images/generations") {
                        let resp = tiny_http::Response::from_string(body.clone())
                            .with_status_code(status)
                            .with_header(
                                "Content-Type: application/json"
                                    .parse::<tiny_http::Header>()
                                    .unwrap(),
                            );
                        let _ = req.respond(resp);
                    } else {
                        let resp = tiny_http::Response::from_data(png_bytes.clone())
                            .with_status_code(200)
                            .with_header(
                                "Content-Type: image/png"
                                    .parse::<tiny_http::Header>()
                                    .unwrap(),
                            );
                        let _ = req.respond(resp);
                    }
                }
            });
            Self {
                port,
                requests,
                _handle: handle,
            }
        }

        fn base_url(&self) -> String {
            format!("http://127.0.0.1:{}/api/paas/v4", self.port)
        }
    }

    #[test]
    fn test_zai_generate_image_success_returns_png_and_sends_expected_body() {
        // Arrange
        let png_bytes = b"\x89PNG\r\n\x1a\n hello world".to_vec();
        let server = MockServer::start(
            |port| {
                serde_json::json!({
                    "data": [{ "url": format!("http://127.0.0.1:{port}/cdn/img.png") }],
                })
                .to_string()
            },
            200,
            png_bytes.clone(),
        );
        let client = ZaiClient::with_base_url("zai-key", server.base_url());

        // Act
        let img = client
            .generate_image("glm-image", "seamless cork bark albedo", None)
            .expect("ok");

        // Assert
        assert_eq!(img.png_bytes, png_bytes);
        assert!(img.mime_type.starts_with("image/"));
        let reqs = server.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2, "one POST + one GET expected");
        assert!(reqs[0].0.contains("/images/generations"), "first must be POST");
        let parsed: serde_json::Value = serde_json::from_str(&reqs[0].1).unwrap();
        assert_eq!(parsed["model"], "glm-image");
        assert_eq!(parsed["prompt"], "seamless cork bark albedo");
        assert_eq!(parsed["size"], DEFAULT_IMAGE_SIZE);
        assert!(reqs[1].0.contains("/cdn/img.png"), "second must hit CDN");
    }

    #[test]
    fn test_zai_generate_image_api_error_propagates_status_and_message() {
        // Arrange
        let server = MockServer::start(
            |_| r#"{"error":{"message":"bad key"}}"#.to_string(),
            401,
            Vec::new(),
        );
        let client = ZaiClient::with_base_url("zai-key", server.base_url());

        // Act
        let err = client
            .generate_image("glm-image", "p", None)
            .expect_err("401 should propagate");

        // Assert
        let s = err.to_string();
        assert!(s.contains("401"), "got: {s}");
        assert!(s.contains("bad key"), "got: {s}");
    }

    #[test]
    fn test_zai_generate_image_empty_data_array_returns_empty_response_error() {
        // Arrange — well-formed JSON but no URL in the data array.
        let server = MockServer::start(
            |_| serde_json::json!({ "data": [] }).to_string(),
            200,
            Vec::new(),
        );
        let client = ZaiClient::with_base_url("zai-key", server.base_url());

        // Act
        let err = client
            .generate_image("glm-image", "p", None)
            .expect_err("empty data should fail");

        // Assert
        assert!(err.to_string().contains("empty response"), "got: {err}");
    }

    #[test]
    fn test_zai_default_model_falls_back_when_caller_passes_empty_string() {
        // Arrange — verifies `model_name` substitution path inside generate_image.
        let png_bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let server = MockServer::start(
            |port| {
                serde_json::json!({
                    "data": [{ "url": format!("http://127.0.0.1:{port}/cdn/img.png") }],
                })
                .to_string()
            },
            200,
            png_bytes,
        );
        let client = ZaiClient::with_base_url("zai-key", server.base_url());

        // Act
        let _ = client.generate_image("", "p", None).expect("ok");

        // Assert
        let reqs = server.requests.lock().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&reqs[0].1).unwrap();
        assert_eq!(parsed["model"], DEFAULT_IMAGE_MODEL);
    }

    #[test]
    fn test_zai_parse_error_message_falls_back_to_raw_body_on_unknown_shape() {
        // Arrange — body is plain text, not JSON.
        let raw = b"   gateway timeout\n";
        // Act
        let msg = parse_error_message(raw);
        // Assert
        assert_eq!(msg, "gateway timeout");
    }
}
