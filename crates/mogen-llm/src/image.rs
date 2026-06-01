//! Gemini image generation over the same `generateContent` endpoint used for
//! text. Reuses [`GeminiClient`]'s HTTP plumbing and error type — only the
//! request shape (adds `responseModalities: ["IMAGE"]`) and the response
//! parser (looks for `inlineData` parts carrying base64 PNG bytes) differ.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;

use crate::gemini::{GeminiAuth, GeminiClient, GeminiError};
use crate::google_oauth;
use crate::types::ImageInput;

/// Default image model for API-key flows. 2.5 Flash Image ("Nano Banana") is
/// the cheapest tier on the public `generativelanguage.googleapis.com` surface
/// that honors `responseModalities: ["IMAGE"]` and produces usable PBR albedo.
pub const DEFAULT_IMAGE_MODEL: &str = "gemini-2.5-flash-image";

/// Default image model when the credential is an Antigravity OAuth bundle.
/// Image gen goes through Cloud Code Assist's image surface
/// (`daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent`).
/// `gemini-3.1-flash-image` is the only ID observed to route reliably for
/// Antigravity-issued bundles — matches `IMAGE_MODEL_DEFAULT` in McKrei's
/// `opencode-antigravity-nano-banana`. The `-preview` variants 404 here.
pub const DEFAULT_OAUTH_IMAGE_MODEL: &str = "gemini-3.1-flash-image";

/// Pick the right image-model default for the credential type. Callers that
/// expose a `--model` flag should only consult this when the user hasn't
/// passed an explicit value.
pub fn default_image_model_for(auth: &GeminiAuth) -> &'static str {
    match auth {
        GeminiAuth::OAuth(_) | GeminiAuth::AntigravityOAuth(_) => DEFAULT_OAUTH_IMAGE_MODEL,
        GeminiAuth::ApiKey(_) => DEFAULT_IMAGE_MODEL,
    }
}

/// Same as [`default_image_model_for`] but driven by a boolean — convenient at
/// call sites that haven't constructed a [`GeminiAuth`] yet (e.g. the CLI/UI
/// resolves a `GoogleCredential` first and only later builds the client).
pub fn default_image_model_when_oauth(is_oauth: bool) -> &'static str {
    if is_oauth {
        DEFAULT_OAUTH_IMAGE_MODEL
    } else {
        DEFAULT_IMAGE_MODEL
    }
}

/// Raw PNG bytes returned by the model, ready to write to disk as-is.
#[derive(Debug, Clone)]
pub struct GeneratedImage {
    pub png_bytes: Vec<u8>,
    pub mime_type: String,
}

impl GeminiClient {
    /// Call `generateContent` on an image-capable model and return the first
    /// `inlineData` part as decoded bytes. Text parts are ignored — the model
    /// typically emits a short caption alongside the image which we don't need.
    ///
    /// `seed`, when supplied, is forwarded to `generationConfig.seed` so the
    /// caller can drive sampling variation. Gemini doesn't guarantee
    /// determinism, but the field still varies the output for image models.
    ///
    /// Both API-key and OAuth credentials are supported. OAuth requests go to
    /// the Cloud Code Assist `v1internal` surface using the bundle's project
    /// id; API-key requests go to the public `generativelanguage` surface.
    pub fn generate_image(
        &self,
        model: &str,
        prompt: &str,
        seed: Option<u64>,
    ) -> Result<GeneratedImage, GeminiError> {
        self.generate_image_with_inputs(model, prompt, seed, &[])
    }

    /// Same as [`Self::generate_image`] but conditions generation on one or
    /// more input images (image-to-image / editing). The images are attached
    /// as `inline_data` parts ahead of the text prompt — the shape Gemini's
    /// image surface uses for "edit this picture" requests. Used by the Scene
    /// Wizard to cut an isolated per-object reference out of a source photo.
    /// Pass an empty slice for plain text-to-image.
    pub fn generate_image_with_inputs(
        &self,
        model: &str,
        prompt: &str,
        seed: Option<u64>,
        input_images: &[ImageInput],
    ) -> Result<GeneratedImage, GeminiError> {
        // Honor the `ImageClient::generate_image` contract that `""` means
        // "use the provider default". Empty model would otherwise produce
        // `/models/:generateContent` and a confusing 404.
        let resolved_model = if model.is_empty() {
            default_image_model_for(self.auth())
        } else {
            model
        };
        let model = resolved_model;
        let inline = match self.auth() {
            GeminiAuth::ApiKey(key) => {
                // Public API speaks `responseModalities: ["IMAGE"]` and returns
                // a single JSON envelope.
                let inner = build_image_request(prompt, seed, Some(&["IMAGE"]), input_images);
                let url = format!(
                    "{}/models/{}:generateContent?key={}",
                    self.base_url(),
                    model,
                    key,
                );
                let resp = self.http().post(&url).json(&inner).send()?;
                let status = resp.status();
                let bytes = resp.bytes()?;
                if !status.is_success() {
                    let message = crate::gemini::parse_error_message(&bytes);
                    return Err(GeminiError::Api { status: status.as_u16(), message });
                }
                pick_inline_image(&bytes)?
            }
            GeminiAuth::OAuth(_) => {
                // The gemini-cli OAuth client gets a 403 from the image
                // surface regardless of scopes. The user must log in with
                // the Antigravity client (`mogen auth login --antigravity`)
                // for image gen, or fall back to an API key.
                return Err(GeminiError::OAuth(
                    "image generation is not available with the gemini-cli OAuth client; \
                     run `mogen auth login --antigravity` to authorize the Antigravity \
                     OAuth client (required for nano-banana / Gemini 3 Pro Image), or \
                     set GEMINI_API_KEY for the public-API surface"
                        .into(),
                ));
            }
            GeminiAuth::AntigravityOAuth(_) => {
                // Antigravity Cloud Code Assist image surface. Body shape
                // is the `pi-nano-antigravity-image` envelope:
                // `imageConfig` + `systemInstruction` + `safetySettings`,
                // wrapped in a `nano-banana-…` requestId. Headers are the
                // Antigravity-flavoured set with `Accept: text/event-stream`
                // for SSE streaming.
                //
                // Three layers of resilience, mirroring McKrei's
                // `opencode-antigravity-nano-banana` reference impl:
                //   1. **Catalog probe** — `:fetchAvailableModels` tells us
                //      which IDs are live for this bundle's project. We
                //      intersect with our static preference list so we
                //      only try names that exist (no more
                //      walk-then-404-then-walk). Fills lazily, cached on
                //      the client across calls.
                //   2. **Endpoint failover** — `IMAGE_ENDPOINT_FALLOVER`
                //      walks `daily-cloudcode-pa` → `…sandbox.…` → prod →
                //      autopush. Different endpoints route to different
                //      capacity pools; lone-material 404s on the primary
                //      tend to succeed on the next.
                //   3. **Unified backoff retry** — 404 / 429 / 503 are all
                //      treated as transient capacity errors when the
                //      candidate is in the catalog. Backoff schedule is
                //      3s / 6s / 12s, matching McKrei's
                //      `CAPACITY_RETRY_BASE_DELAY_MS * 2^retry`.
                //
                // Worst-case wall time on a fully transient outage:
                // 4 endpoints × 1 catalog model × 4 attempts × 12s max
                // wait ≈ 130 s before final failure. Happy path is
                // unchanged at ~3–6 s.
                let project = self
                    .oauth_project_id()
                    .ok_or_else(|| GeminiError::OAuth("missing project id in token bundle".into()))?;
                let inner = build_image_request(prompt, seed, None, input_images);

                let candidates: Vec<(String, bool)> =
                    if model == DEFAULT_OAUTH_IMAGE_MODEL || model.is_empty() {
                        // No explicit user choice — consult the catalog.
                        self.ensure_image_catalog(
                            &project,
                            google_oauth::client::IMAGE_ENDPOINT,
                        )?
                    } else {
                        // Explicit user choice — trust it as if it were
                        // catalog-listed (full backoff retry on 404).
                        vec![(model.to_string(), true)]
                    };

                const CAPACITY_BACKOFFS_SECS: [u64; 3] = [3, 6, 12];
                let mut last_err: Option<GeminiError> = None;
                let mut picked: Option<RawInlineData> = None;

                // Prefer the most actionable error when reporting failure:
                // a 429 with a quota-reset window is more useful to the
                // user than a 403 from a sandbox endpoint they can't enable.
                // Stickier errors (429, 503) win over transient (404) which
                // win over config (403).
                let prefer_err = |new: &GeminiError, old: &Option<GeminiError>| -> bool {
                    let rank = |e: &GeminiError| -> u8 {
                        match e {
                            GeminiError::Api { status: 429, .. } => 4,
                            GeminiError::Api { status: 503, .. } => 3,
                            GeminiError::Api { status: 404, .. } => 2,
                            GeminiError::Api { status: 403, .. } => 1,
                            _ => 0,
                        }
                    };
                    match old {
                        None => true,
                        Some(o) => rank(new) >= rank(o),
                    }
                };
                'walk_endpoints: for endpoint in
                    google_oauth::client::IMAGE_ENDPOINT_FALLOVER.iter()
                {
                    let url = google_oauth::cloudcode::stream_generate_content_url(endpoint);
                    for (candidate, in_catalog) in candidates.iter() {
                        let body = google_oauth::cloudcode::wrap_image_body(
                            &project,
                            candidate,
                            inner.clone(),
                        );
                        let mut attempt: usize = 0;
                        loop {
                            attempt += 1;
                            match self.oauth_post_image_with_retry(&url, &body) {
                                Ok(bytes) => match pick_inline_image_sse(&bytes) {
                                    Ok(inline) => {
                                        picked = Some(inline);
                                        break 'walk_endpoints;
                                    }
                                    Err(e) => {
                                        if prefer_err(&e, &last_err) {
                                            last_err = Some(e);
                                        }
                                        break;
                                    }
                                },
                                // 403: API not enabled on this endpoint's
                                // upstream project. No retry, no other
                                // candidate will help — skip the entire
                                // endpoint and try the next one.
                                Err(e @ GeminiError::Api { status: 403, .. }) => {
                                    if prefer_err(&e, &last_err) {
                                        last_err = Some(e);
                                    }
                                    continue 'walk_endpoints;
                                }
                                // 404 on a model NOT in the catalog → walk
                                // immediately, no backoff. The bundle's
                                // project doesn't route this name; retrying
                                // is wasted wall time.
                                Err(e @ GeminiError::Api { status: 404, .. })
                                    if !in_catalog =>
                                {
                                    if prefer_err(&e, &last_err) {
                                        last_err = Some(e);
                                    }
                                    break;
                                }
                                Err(e @ GeminiError::Api { status: 404, .. })
                                | Err(e @ GeminiError::Api { status: 429, .. })
                                | Err(e @ GeminiError::Api { status: 503, .. }) => {
                                    if prefer_err(&e, &last_err) {
                                        last_err = Some(e);
                                    }
                                    if attempt > CAPACITY_BACKOFFS_SECS.len() {
                                        break;
                                    }
                                    let wait = CAPACITY_BACKOFFS_SECS
                                        [(attempt - 1).min(CAPACITY_BACKOFFS_SECS.len() - 1)];
                                    std::thread::sleep(std::time::Duration::from_secs(wait));
                                    continue;
                                }
                                Err(e) => return Err(e),
                            }
                        }
                    }
                }
                match picked {
                    Some(p) => p,
                    None => return Err(last_err.unwrap_or(GeminiError::EmptyResponse)),
                }
            }
        };

        if !inline.mime_type.starts_with("image/") {
            return Err(GeminiError::InvalidResponse(format!(
                "expected image/* mime type, got {}",
                inline.mime_type
            )));
        }

        let png_bytes = STANDARD.decode(inline.data.as_bytes()).map_err(|e| {
            GeminiError::InvalidResponse(format!("base64 decode failed: {e}"))
        })?;

        Ok(GeneratedImage { png_bytes, mime_type: inline.mime_type })
    }
}

/// Walk a one-shot JSON response (`{ candidates: [...] }` or
/// `{ response: { candidates: [...] } }`) and return the first inline
/// image found, surfacing `finishReason` if every candidate was filtered.
fn pick_inline_image(bytes: &[u8]) -> Result<RawInlineData, GeminiError> {
    let parsed: RawImageEnvelope = serde_json::from_slice(bytes)
        .map_err(|e| GeminiError::InvalidResponse(e.to_string()))?;
    let response = parsed
        .response
        .unwrap_or(RawImageResponse { candidates: parsed.candidates });

    let finish_reasons: Vec<String> = response
        .candidates
        .iter()
        .filter(|c| c.content.is_none())
        .filter_map(|c| c.finish_reason.clone())
        .collect();

    let inline = response
        .candidates
        .into_iter()
        .filter_map(|c| c.content)
        .flat_map(|c| c.parts.into_iter())
        .find_map(|p| p.inline_data);

    match inline {
        Some(i) => Ok(i),
        None if !finish_reasons.is_empty() => Err(GeminiError::InvalidResponse(format!(
            "no image returned (finishReason: {})",
            finish_reasons.join(", ")
        ))),
        None => Err(GeminiError::EmptyResponse),
    }
}

/// Parse a Server-Sent-Events response from `:streamGenerateContent?alt=sse`.
/// Each non-empty `data: …` line is a JSON chunk in the same `{ response:
/// { candidates: [...] } }` shape as one-shot OAuth replies. We scan every
/// chunk because the model emits text and image parts in separate frames —
/// the image typically arrives last.
fn pick_inline_image_sse(bytes: &[u8]) -> Result<RawInlineData, GeminiError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| GeminiError::InvalidResponse(format!("non-utf8 SSE body: {e}")))?;

    if std::env::var("MOGEN_DEBUG_HTTP").ok().as_deref() == Some("1") {
        // Trim base64 inline_data payloads — full SSE chunks are megabytes
        // when an image lands. We only need to see error paths.
        let preview = truncate_at_char_boundary(text, 4096);
        eprintln!("[mogen image SSE] {} bytes, preview:\n{}", text.len(), preview);
    }
    let mut last_finish_reasons: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(payload) = line.strip_prefix("data: ") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(env) = serde_json::from_str::<RawImageEnvelope>(payload) else {
            continue;
        };
        let response = env
            .response
            .unwrap_or(RawImageResponse { candidates: env.candidates });
        for c in response.candidates {
            if let Some(reason) = c.finish_reason.clone() {
                last_finish_reasons.push(reason);
            }
            if let Some(content) = c.content {
                for part in content.parts {
                    if let Some(inline) = part.inline_data {
                        if inline.mime_type.starts_with("image/") {
                            return Ok(inline);
                        }
                    }
                }
            }
        }
    }

    if !last_finish_reasons.is_empty() {
        return Err(GeminiError::InvalidResponse(format!(
            "no image returned (finishReason: {})",
            last_finish_reasons.join(", ")
        )));
    }
    Err(GeminiError::EmptyResponse)
}

/// Slice `s` to at most `max_bytes`, snapping back to the previous UTF-8
/// char boundary when the byte index lands inside a multi-byte sequence.
/// `&str[..n]` panics on a non-boundary `n`; Google error responses do
/// carry non-ASCII (smart quotes, em-dashes), so a debug-mode hex slice
/// of one would crash the request mid-image-gen.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Inner request body for image gen. The API-key path needs
/// `responseModalities` (`["IMAGE"]`) and runs against the public
/// `generativelanguage.googleapis.com` surface. The OAuth path runs
/// against Cloud Code Assist's image surface, which rejects
/// `responseModalities` — `wrap_image_body` strips it and substitutes
/// `imageConfig` instead. Pass `None` for the OAuth path so we don't
/// build a key the wrapper will throw away.
fn build_image_request(
    prompt: &str,
    seed: Option<u64>,
    modalities: Option<&[&str]>,
    input_images: &[ImageInput],
) -> serde_json::Value {
    let mut gen_cfg = serde_json::json!({});
    if let Some(m) = modalities {
        gen_cfg["responseModalities"] = serde_json::json!(m);
    }
    if let Some(s) = seed {
        // Gemini accepts `seed` as an i32 — saturate to the positive range,
        // matching what the text path does in `gemini::build_request`.
        let clipped = (s as i64) & 0x7FFF_FFFF;
        gen_cfg["seed"] = serde_json::json!(clipped);
    }
    // Input images first, then the text instruction — the ordering Gemini
    // recommends for image-editing turns (mirrors the text vision path in
    // `gemini::build_request`).
    let mut parts: Vec<serde_json::Value> = Vec::with_capacity(input_images.len() + 1);
    for img in input_images {
        parts.push(serde_json::json!({
            "inline_data": {
                "mime_type": img.mime_type,
                "data": STANDARD.encode(&img.data),
            }
        }));
    }
    parts.push(serde_json::json!({ "text": prompt }));
    serde_json::json!({
        "contents": [{
            "role": "user",
            "parts": parts,
        }],
        "generationConfig": gen_cfg,
    })
}

/// Tolerates both shapes:
///   - public API:   `{ candidates: [...], usageMetadata: {...} }`
///   - cloudcode-pa: `{ response: { candidates: [...], ... } }`
#[derive(Debug, Deserialize)]
struct RawImageEnvelope {
    #[serde(default)]
    candidates: Vec<RawImageCandidate>,
    #[serde(default)]
    response: Option<RawImageResponse>,
}

#[derive(Debug, Deserialize)]
struct RawImageResponse {
    #[serde(default)]
    candidates: Vec<RawImageCandidate>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawImageCandidate {
    #[serde(default)]
    content: Option<RawImageContent>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawImageContent {
    #[serde(default)]
    parts: Vec<RawImagePart>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawImagePart {
    #[serde(default)]
    inline_data: Option<RawInlineData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInlineData {
    mime_type: String,
    data: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with_image_part(b64: &str) -> String {
        // Cloud Code Assist envelopes the public-API response in
        // `{ response: {...} }`. Match that shape so we exercise the
        // non-default branch in `pick_inline_image_sse`.
        format!(
            "data: {}\n\n",
            serde_json::json!({
                "response": {
                    "candidates": [{
                        "content": {
                            "parts": [{
                                "inlineData": {
                                    "mimeType": "image/png",
                                    "data": b64,
                                }
                            }]
                        }
                    }]
                }
            })
        )
    }

    #[test]
    fn build_image_request_text_only_has_single_text_part() {
        let req = build_image_request("a chair", Some(7), Some(&["IMAGE"]), &[]);
        let parts = req["contents"][0]["parts"].as_array().expect("parts array");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "a chair");
        assert_eq!(req["generationConfig"]["responseModalities"][0], "IMAGE");
    }

    #[test]
    fn build_image_request_input_image_comes_before_text() {
        let inputs = vec![ImageInput {
            mime_type: "image/png".into(),
            data: b"hello".to_vec(),
        }];
        let req = build_image_request("extract the chair", None, None, &inputs);
        let parts = req["contents"][0]["parts"].as_array().expect("parts array");
        assert_eq!(parts.len(), 2);
        // Image part is first.
        assert_eq!(parts[0]["inline_data"]["mime_type"], "image/png");
        assert_eq!(parts[0]["inline_data"]["data"], STANDARD.encode(b"hello"));
        // Text part is last.
        assert_eq!(parts[1]["text"], "extract the chair");
    }

    #[test]
    fn test_pick_inline_image_sse_single_frame_returns_inline_data() {
        // Arrange
        let body = frame_with_image_part("aGVsbG8=");

        // Act
        let inline = pick_inline_image_sse(body.as_bytes()).expect("frame parses");

        // Assert
        assert_eq!(inline.mime_type, "image/png");
        assert_eq!(inline.data, "aGVsbG8=");
    }

    #[test]
    fn test_pick_inline_image_sse_done_sentinel_after_image_returns_inline_data() {
        // Arrange — `[DONE]` may follow the last data frame; the parser
        // must not bail before processing the image-bearing chunk.
        let mut body = frame_with_image_part("Zm9v");
        body.push_str("data: [DONE]\n\n");

        // Act
        let inline = pick_inline_image_sse(body.as_bytes()).expect("frame parses");

        // Assert
        assert_eq!(inline.data, "Zm9v");
    }

    #[test]
    fn test_pick_inline_image_sse_malformed_data_line_is_skipped() {
        // Arrange — a non-JSON `data:` line in front of a valid frame must
        // be ignored, not surface as an error.
        let mut body = String::from("data: not-json-at-all\n\n");
        body.push_str(&frame_with_image_part("YmFy"));

        // Act
        let inline = pick_inline_image_sse(body.as_bytes()).expect("valid frame still parses");

        // Assert
        assert_eq!(inline.data, "YmFy");
    }

    #[test]
    fn test_pick_inline_image_sse_empty_body_returns_empty_response() {
        // Arrange
        let body = b"";

        // Act
        let err = pick_inline_image_sse(body).expect_err("empty body must error");

        // Assert
        assert!(
            matches!(err, GeminiError::EmptyResponse),
            "expected EmptyResponse, got {err:?}",
        );
    }

    #[test]
    fn test_pick_inline_image_sse_finish_reason_without_image_returns_invalid_response() {
        // Arrange — model refused / hit a safety filter; finishReason is
        // populated but no inline image part is emitted.
        let body = format!(
            "data: {}\n\n",
            serde_json::json!({
                "response": {
                    "candidates": [{
                        "finishReason": "IMAGE_RECITATION"
                    }]
                }
            })
        );

        // Act
        let err = pick_inline_image_sse(body.as_bytes()).expect_err("must error");

        // Assert — surfaces the reason so the retry layer can match on it.
        match err {
            GeminiError::InvalidResponse(msg) => {
                assert!(
                    msg.contains("IMAGE_RECITATION"),
                    "error message must include finishReason, got: {msg}",
                );
            }
            other => panic!("expected InvalidResponse, got {other:?}"),
        }
    }

    #[test]
    fn test_pick_inline_image_sse_non_utf8_body_returns_invalid_response() {
        // Arrange — corrupt the byte stream so the leading from_utf8 fails.
        let body = [0xff_u8, 0xfe, 0xfd];

        // Act
        let err = pick_inline_image_sse(&body).expect_err("non-utf8 must error");

        // Assert
        assert!(
            matches!(err, GeminiError::InvalidResponse(_)),
            "expected InvalidResponse, got {err:?}",
        );
    }

    #[test]
    fn truncate_at_char_boundary_returns_full_string_when_short() {
        let s = "hello";
        assert_eq!(truncate_at_char_boundary(s, 4096), "hello");
    }

    #[test]
    fn truncate_at_char_boundary_snaps_back_from_inside_multibyte_char() {
        // Regression: `&s[..3]` panics on a 4-byte UTF-8 char that starts
        // before byte 3 ends after it. Google SSE bodies can include
        // non-ASCII characters in error descriptions; the debug-print
        // path must not crash.
        let s = "abc😀def"; // emoji is 4 bytes (F0 9F 98 80)
        let out = truncate_at_char_boundary(s, 4); // lands inside the emoji
        assert_eq!(out, "abc"); // snapped back to last char boundary
    }

    #[test]
    fn truncate_at_char_boundary_keeps_complete_chars_at_exact_boundary() {
        let s = "abc😀def";
        let out = truncate_at_char_boundary(s, 7); // boundary just after emoji
        assert_eq!(out, "abc😀");
    }
}
