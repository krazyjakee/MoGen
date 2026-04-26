//! Gemini image generation over the same `generateContent` endpoint used for
//! text. Reuses [`GeminiClient`]'s HTTP plumbing and error type — only the
//! request shape (adds `responseModalities: ["IMAGE"]`) and the response
//! parser (looks for `inlineData` parts carrying base64 PNG bytes) differ.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;

use crate::gemini::{GeminiClient, GeminiError};

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
    /// caller can drive sampling variation (Gemini doesn't guarantee
    /// determinism but the field still varies the output for image models).
    pub fn generate_image(
        &self,
        model: &str,
        prompt: &str,
        seed: Option<u64>,
    ) -> Result<GeneratedImage, GeminiError> {
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url(),
            model,
            self.api_key(),
        );

        let mut gen_cfg = serde_json::json!({
            "responseModalities": ["IMAGE"],
        });
        if let Some(s) = seed {
            // Gemini accepts `seed` as an i32 — saturate to the positive range,
            // matching what the text path does in `gemini::build_request`.
            let clipped = (s as i64) & 0x7FFF_FFFF;
            gen_cfg["seed"] = serde_json::json!(clipped);
        }
        let body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [{ "text": prompt }],
            }],
            "generationConfig": gen_cfg,
        });

        let resp = self.http().post(&url).json(&body).send()?;
        let status = resp.status();
        let bytes = resp.bytes()?;

        if !status.is_success() {
            let message = crate::gemini::parse_error_message(&bytes);
            return Err(GeminiError::Api { status: status.as_u16(), message });
        }

        let parsed: RawImageResponse = serde_json::from_slice(&bytes)
            .map_err(|e| GeminiError::InvalidResponse(e.to_string()))?;

        // Gemini omits `content` on candidates that were filtered (safety,
        // recitation, MAX_TOKENS, …) and emits only `finishReason`. Surface
        // that reason instead of failing on the missing `content` field, so
        // the user gets an actionable error rather than a parser hiccup.
        let finish_reasons: Vec<String> = parsed
            .candidates
            .iter()
            .filter(|c| c.content.is_none())
            .filter_map(|c| c.finish_reason.clone())
            .collect();

        let inline = parsed
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
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
