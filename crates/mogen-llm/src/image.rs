//! Gemini image generation over the same `generateContent` endpoint used for
//! text. Reuses [`GeminiClient`]'s HTTP plumbing and error type — only the
//! request shape (adds `responseModalities: ["IMAGE"]`) and the response
//! parser (looks for `inlineData` parts carrying base64 PNG bytes) differ.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;

use crate::gemini::{GeminiAuth, GeminiClient, GeminiError};
use crate::google_oauth;

/// Default image model. 2.5 Flash Image ("Nano Banana") is the cheapest tier
/// that honors `responseModalities: ["IMAGE"]` and produces usable PBR albedo.
pub const DEFAULT_IMAGE_MODEL: &str = "gemini-2.5-flash-image";

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
    /// In OAuth mode this returns [`GeminiError::ImageOverOAuthUnverified`]
    /// because image generation has not been verified against the
    /// Cloud Code Assist `v1internal` surface — callers must use the opt-in
    /// [`Self::generate_image_with_oauth_policy`] to probe.
    pub fn generate_image(
        &self,
        model: &str,
        prompt: &str,
        seed: Option<u64>,
    ) -> Result<GeneratedImage, GeminiError> {
        self.generate_image_with_oauth_policy(model, prompt, seed, false)
    }

    /// Same as [`Self::generate_image`] but with explicit control over the
    /// OAuth path. When `allow_oauth=true`, a Cloud Code Assist call is
    /// attempted using the OAuth bundle's project + bearer token — any
    /// upstream error surfaces as a normal [`GeminiError::Api`] so the
    /// caller can decide how to react.
    pub fn generate_image_with_oauth_policy(
        &self,
        model: &str,
        prompt: &str,
        seed: Option<u64>,
        allow_oauth: bool,
    ) -> Result<GeneratedImage, GeminiError> {
        let inner = build_image_request(prompt, seed);

        let bytes = match self.auth() {
            GeminiAuth::ApiKey(_) => {
                // Build URL with key + same wire shape as the existing path.
                let key = match self.auth() {
                    GeminiAuth::ApiKey(k) => k,
                    _ => unreachable!("matched above"),
                };
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
                bytes.to_vec()
            }
            GeminiAuth::OAuth(_) => {
                if !allow_oauth {
                    return Err(GeminiError::ImageOverOAuthUnverified);
                }
                let project = self
                    .oauth_project_id()
                    .ok_or_else(|| GeminiError::OAuth("missing project id in token bundle".into()))?;
                let model_full = if model.starts_with("models/") {
                    model.to_string()
                } else {
                    format!("models/{model}")
                };
                let url = google_oauth::cloudcode::generate_content_url(self.base_url());
                let body = google_oauth::cloudcode::wrap_body(&project, &model_full, inner);
                self.oauth_post_with_retry(&url, &body)?
            }
        };

        let parsed: RawImageEnvelope = serde_json::from_slice(&bytes)
            .map_err(|e| GeminiError::InvalidResponse(e.to_string()))?;

        // OAuth wraps in `{ response: ... }`; API-key returns the response
        // directly. Pick whichever side decoded.
        let response = parsed.response.unwrap_or(RawImageResponse {
            candidates: parsed.candidates,
        });

        // Gemini omits `content` on candidates that were filtered (safety,
        // recitation, MAX_TOKENS, …) and emits only `finishReason`. Surface
        // that reason instead of failing on the missing `content` field, so
        // the user gets an actionable error rather than a parser hiccup.
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

        let inline = match inline {
            Some(i) => i,
            None if !finish_reasons.is_empty() => {
                return Err(GeminiError::InvalidResponse(format!(
                    "no image returned (finishReason: {})",
                    finish_reasons.join(", ")
                )));
            }
            None => return Err(GeminiError::EmptyResponse),
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

fn build_image_request(prompt: &str, seed: Option<u64>) -> serde_json::Value {
    let mut gen_cfg = serde_json::json!({ "responseModalities": ["IMAGE"] });
    if let Some(s) = seed {
        // Gemini accepts `seed` as an i32 — saturate to the positive range,
        // matching what the text path does in `gemini::build_request`.
        let clipped = (s as i64) & 0x7FFF_FFFF;
        gen_cfg["seed"] = serde_json::json!(clipped);
    }
    serde_json::json!({
        "contents": [{
            "role": "user",
            "parts": [{ "text": prompt }],
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
