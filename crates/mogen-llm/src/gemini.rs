//! Hand-rolled Gemini REST client.
//!
//! SDK crates lag the spec; we need `cachedContents` and structured errors.
//! Blocking HTTP keeps the `mogen` binary synchronous — one request per repair
//! iteration is not worth a tokio runtime.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// Re-export the shared types so existing callers using `mogen_llm::gemini::*`
// keep compiling after the multi-provider refactor. New code should import
// these directly from `mogen_llm`.
pub use crate::types::{
    GenerateConfig, GenerateResponse, ImageInput, Role, ThinkingLevel, Turn, Usage,
    DEFAULT_TEMPERATURE,
};

/// Default model for `mogen generate`. Alias auto-rolls to the newest Pro tier.
pub const DEFAULT_MODEL: &str = "gemini-pro-latest";

/// Default model for fast / low-stakes text calls (prompt enhancement, small
/// rewrites). ~4× cheaper than the Pro tier and fast enough for interactive
/// use — callers that need the Pro tier can still override.
pub const DEFAULT_FAST_MODEL: &str = "gemini-flash-latest";

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

#[derive(Debug, Error)]
pub enum GeminiError {
    #[error("missing GEMINI_API_KEY (set env var or pass --api-key)")]
    MissingApiKey,
    #[error("transport error: {}", format_source_chain(.0))]
    Transport(#[from] reqwest::Error),
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("empty response: no candidates or text parts")]
    EmptyResponse,
    #[error("budget exceeded: {used} input+output tokens exceeds --budget-tokens={budget}")]
    BudgetExceeded { used: u32, budget: u32 },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

/// Handle returned after creating a `cachedContents` resource. The resource
/// name is what you pass to [`GenerateConfig::cached_content`] on subsequent
/// calls; `expires_at_unix` lets callers persist cache state locally and
/// re-use the resource until it expires server-side.
#[derive(Debug, Clone)]
pub struct CachedContent {
    pub name: String,
    pub expires_at_unix: u64,
    pub token_count: Option<u32>,
}

pub struct GeminiClient {
    http: reqwest::blocking::Client,
    api_key: String,
    base_url: String,
}

impl GeminiClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        // `gemini-pro-latest` with a large system instruction or thinking
        // enabled can easily push past a couple of minutes end-to-end. The
        // short connect_timeout fails fast when the user is offline so they
        // don't sit through the 600s overall budget.
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self { http, api_key: api_key.into(), base_url: base_url.into() }
    }

    pub fn from_env() -> Result<Self, GeminiError> {
        let key = std::env::var("GEMINI_API_KEY").map_err(|_| GeminiError::MissingApiKey)?;
        if key.trim().is_empty() {
            return Err(GeminiError::MissingApiKey);
        }
        Ok(Self::new(key))
    }

    pub(crate) fn http(&self) -> &reqwest::blocking::Client {
        &self.http
    }

    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, GeminiError> {
        let model = if cfg.model.is_empty() { DEFAULT_MODEL } else { cfg.model.as_str() };
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, model, self.api_key
        );

        let body = build_request(cfg);

        let resp = self.http.post(&url).json(&body).send()?;
        let status = resp.status();
        let bytes = resp.bytes()?;

        if !status.is_success() {
            let message = parse_error_message(&bytes);
            return Err(GeminiError::Api { status: status.as_u16(), message });
        }

        let parsed: RawGenerateResponse = serde_json::from_slice(&bytes)
            .map_err(|e| GeminiError::InvalidResponse(e.to_string()))?;

        let text = parsed
            .candidates
            .first()
            .and_then(|c| c.content.parts.first())
            .map(|p| p.text.clone())
            .ok_or(GeminiError::EmptyResponse)?;

        let usage = parsed
            .usage_metadata
            .map(|u| Usage {
                prompt_tokens: u.prompt_token_count,
                response_tokens: u.candidates_token_count,
                total_tokens: u.total_token_count,
                cached_tokens: u.cached_content_token_count,
            })
            .unwrap_or_default();

        if let Some(budget) = cfg.budget_tokens {
            if usage.total_tokens > budget {
                return Err(GeminiError::BudgetExceeded {
                    used: usage.total_tokens,
                    budget,
                });
            }
        }

        Ok(GenerateResponse { text, usage })
    }

    /// Create a `cachedContents` resource holding `system_instruction`. The
    /// returned resource name can then be passed via
    /// [`GenerateConfig::cached_content`] so subsequent `generateContent`
    /// calls skip re-uploading the instruction — Gemini bills cached input
    /// tokens at a reduced rate.
    ///
    /// `ttl_seconds` is the requested server-side lifetime. Gemini enforces a
    /// minimum token count on cached content (model-dependent); if the
    /// instruction is below that threshold this call returns `Api` with the
    /// server's explanation and the caller should fall back to inline.
    pub fn create_cached_content(
        &self,
        model: &str,
        system_instruction: &str,
        ttl_seconds: u64,
    ) -> Result<CachedContent, GeminiError> {
        let url = format!("{}/cachedContents?key={}", self.base_url, self.api_key);
        // Gemini currently requires at least one `contents` entry even for
        // system-instruction-only caches; a minimal placeholder user turn is
        // accepted and does not materially affect the cache's usefulness.
        let body = serde_json::json!({
            "model": format!("models/{}", model.strip_prefix("models/").unwrap_or(model)),
            "contents": [ { "role": "user", "parts": [{ "text": "." }] } ],
            "systemInstruction": { "parts": [{ "text": system_instruction }] },
            "ttl": format!("{ttl_seconds}s"),
        });

        let resp = self.http.post(&url).json(&body).send()?;
        let status = resp.status();
        let bytes = resp.bytes()?;

        if !status.is_success() {
            let message = parse_error_message(&bytes);
            return Err(GeminiError::Api { status: status.as_u16(), message });
        }

        let parsed: RawCachedContent = serde_json::from_slice(&bytes)
            .map_err(|e| GeminiError::InvalidResponse(e.to_string()))?;

        let name = parsed
            .name
            .ok_or_else(|| GeminiError::InvalidResponse("cachedContents: missing name".into()))?;

        // Compute expiry client-side from the requested TTL — the small clock
        // skew is acceptable for a local cache and avoids adding an RFC 3339
        // parser for `expireTime`.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Ok(CachedContent {
            name,
            expires_at_unix: now.saturating_add(ttl_seconds),
            token_count: parsed.usage_metadata.and_then(|u| u.total_token_count),
        })
    }
}

// -- request/response wire types ---------------------------------------------

fn build_request(cfg: &GenerateConfig) -> serde_json::Value {
    let mut contents: Vec<serde_json::Value> = Vec::new();

    // Gemini rejects requests that set `systemInstruction` alongside
    // `cachedContent` ("CachedContent can not be used with GenerateContent
    // request setting system_instruction, tools or tool_config"). The cache
    // resource carries the stable reference block (grammar, kinds, attribute
    // allowlist); the inline dynamic prefix (preamble, conventions, fewshots,
    // stdlib summary, output contract) still has to reach the model fresh
    // per call. When both are present, inject the inline portion as a
    // synthetic user/model preamble at the head of `contents` so the model
    // sees the same total instruction without tripping the API constraint.
    let inline_in_contents = cfg.cached_content.is_some() && cfg.system_instruction.is_some();
    if inline_in_contents {
        let sys = cfg.system_instruction.as_deref().unwrap_or("");
        contents.push(serde_json::json!({
            "role": "user",
            "parts": [{ "text": sys }],
        }));
        contents.push(serde_json::json!({
            "role": "model",
            "parts": [{ "text": "Understood." }],
        }));
    }

    for turn in &cfg.history {
        contents.push(serde_json::json!({
            "role": turn.role.gemini_str(),
            "parts": [{ "text": turn.text }],
        }));
    }

    // Build the user turn's parts list: optional images first (Gemini's docs
    // recommend image-before-text ordering for vision prompts), then the text.
    // The text part is always present even when empty, so the request is
    // well-formed when only images were supplied.
    let mut user_parts: Vec<serde_json::Value> = Vec::with_capacity(cfg.user_images.len() + 1);
    for img in &cfg.user_images {
        user_parts.push(serde_json::json!({
            "inline_data": {
                "mime_type": img.mime_type,
                "data": STANDARD.encode(&img.data),
            }
        }));
    }
    user_parts.push(serde_json::json!({ "text": cfg.user_prompt }));
    contents.push(serde_json::json!({
        "role": "user",
        "parts": user_parts,
    }));

    let mut req = serde_json::json!({ "contents": contents });

    if let Some(cached) = &cfg.cached_content {
        req["cachedContent"] = serde_json::Value::String(cached.clone());
    }
    // Only emit `systemInstruction` when there is no cached resource; with a
    // cache, the inline portion was already moved into `contents` above.
    if !inline_in_contents {
        if let Some(sys) = &cfg.system_instruction {
            req["systemInstruction"] = serde_json::json!({
                "parts": [{ "text": sys }],
            });
        }
    }

    let mut gen_cfg = serde_json::Map::new();
    if let Some(t) = cfg.temperature {
        gen_cfg.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(s) = cfg.seed {
        // Gemini accepts `seed` as an i32 — saturate to the valid range.
        let clipped = (s as i64) & 0x7FFF_FFFF;
        gen_cfg.insert("seed".into(), serde_json::json!(clipped));
    }
    if let Some(level) = cfg.thinking_level {
        gen_cfg.insert(
            "thinkingConfig".into(),
            serde_json::json!({ "thinkingBudget": level.budget() }),
        );
    }
    if !gen_cfg.is_empty() {
        req["generationConfig"] = serde_json::Value::Object(gen_cfg);
    }

    req
}

fn format_source_chain(err: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut out = err.to_string();
    let mut src: Option<&dyn std::error::Error> = err.source();
    while let Some(e) = src {
        out.push_str(": ");
        out.push_str(&e.to_string());
        src = e.source();
    }
    out
}

pub(crate) fn parse_error_message(bytes: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if let Some(msg) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return msg.to_string();
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawGenerateResponse {
    #[serde(default)]
    candidates: Vec<RawCandidate>,
    #[serde(default)]
    usage_metadata: Option<RawUsageMetadata>,
}

#[derive(Debug, Deserialize)]
struct RawCandidate {
    content: RawContent,
}

#[derive(Debug, Deserialize)]
struct RawContent {
    #[serde(default)]
    parts: Vec<RawPart>,
}

#[derive(Debug, Deserialize)]
struct RawPart {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RawUsageMetadata {
    #[serde(default)]
    prompt_token_count: u32,
    #[serde(default)]
    candidates_token_count: u32,
    #[serde(default)]
    total_token_count: u32,
    #[serde(default)]
    cached_content_token_count: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCachedContent {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    usage_metadata: Option<RawCachedMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCachedMetadata {
    #[serde(default)]
    total_token_count: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_includes_system_instruction() {
        let mut cfg = GenerateConfig::new("hello");
        cfg.system_instruction = Some("be helpful".into());
        let body = build_request(&cfg);
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be helpful");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hello");
    }

    #[test]
    fn cached_content_moves_inline_block_into_contents() {
        // Gemini rejects `systemInstruction` + `cachedContent` on the same
        // request. The wire format must therefore drop `systemInstruction`
        // and instead seed `contents` with a synthetic user/model preamble
        // carrying the inline portion. The cache holds the stable block;
        // contents carry the dynamic prefix followed by the actual user turn.
        let mut cfg = GenerateConfig::new("real prompt");
        cfg.system_instruction = Some("inline portion".into());
        cfg.cached_content = Some("cachedContents/abc123".into());
        let body = build_request(&cfg);
        assert_eq!(body["cachedContent"], "cachedContents/abc123");
        assert!(
            body.get("systemInstruction").is_none(),
            "systemInstruction must not be sent alongside cachedContent",
        );
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "inline portion");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"][0]["text"], "real prompt");
    }

    #[test]
    fn cached_content_with_inline_preserves_history_alternation() {
        // History turns must still appear between the synthetic preamble and
        // the final user prompt, in order, so the model sees:
        //   user(inline) -> model(ack) -> user(history0) -> model(history1) -> user(prompt)
        let mut cfg = GenerateConfig::new("fix it");
        cfg.system_instruction = Some("inline portion".into());
        cfg.cached_content = Some("cachedContents/abc123".into());
        cfg.history.push(Turn { role: Role::User, text: "make a chair".into() });
        cfg.history.push(Turn { role: Role::Model, text: "scene { box }".into() });
        let body = build_request(&cfg);
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 5);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"][0]["text"], "make a chair");
        assert_eq!(contents[3]["role"], "model");
        assert_eq!(contents[4]["parts"][0]["text"], "fix it");
    }

    #[test]
    fn cached_content_alone_omits_system_instruction() {
        // Belt-and-braces: when only `cached_content` is set (e.g. a caller
        // pinned a pre-split cache resource that already carries the full
        // instruction), don't synthesize an empty `systemInstruction` field.
        let mut cfg = GenerateConfig::new("x");
        cfg.cached_content = Some("cachedContents/abc123".into());
        let body = build_request(&cfg);
        assert_eq!(body["cachedContent"], "cachedContents/abc123");
        assert!(body.get("systemInstruction").is_none());
    }

    #[test]
    fn history_is_threaded_before_current_prompt() {
        let mut cfg = GenerateConfig::new("fix the diagnostics");
        cfg.history.push(Turn { role: Role::User, text: "make a chair".into() });
        cfg.history.push(Turn { role: Role::Model, text: "scene { box }".into() });
        let body = build_request(&cfg);
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["parts"][0]["text"], "fix the diagnostics");
    }

    #[test]
    fn new_defaults_temperature_to_low_variance() {
        let cfg = GenerateConfig::new("x");
        assert_eq!(cfg.temperature, Some(DEFAULT_TEMPERATURE));
        let body = build_request(&cfg);
        assert!(
            (body["generationConfig"]["temperature"].as_f64().unwrap()
                - DEFAULT_TEMPERATURE as f64)
                .abs()
                < 1e-6
        );
    }

    #[test]
    fn seed_and_temperature_land_in_generation_config() {
        let mut cfg = GenerateConfig::new("x");
        cfg.seed = Some(42);
        cfg.temperature = Some(0.3);
        let body = build_request(&cfg);
        assert_eq!(body["generationConfig"]["seed"], 42);
        assert!((body["generationConfig"]["temperature"].as_f64().unwrap() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn new_defaults_thinking_to_high() {
        let cfg = GenerateConfig::new("x");
        assert_eq!(cfg.thinking_level, Some(ThinkingLevel::High));
        let body = build_request(&cfg);
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            ThinkingLevel::High.budget()
        );
    }

    #[test]
    fn thinking_none_omits_thinking_config() {
        let mut cfg = GenerateConfig::new("x");
        cfg.thinking_level = None;
        let body = build_request(&cfg);
        assert!(body["generationConfig"].get("thinkingConfig").is_none());
    }

    #[test]
    fn thinking_level_budgets_are_monotonic_and_in_range() {
        let budgets = [
            ThinkingLevel::Low.budget(),
            ThinkingLevel::Medium.budget(),
            ThinkingLevel::High.budget(),
            ThinkingLevel::XHigh.budget(),
        ];
        for w in budgets.windows(2) {
            assert!(w[0] < w[1], "levels must strictly increase");
        }
        // Gemini 2.5 Flash max is 24576; Pro min is 128.
        assert!(*budgets.first().unwrap() >= 128);
        assert!(*budgets.last().unwrap() <= 24576);
    }

    #[test]
    fn thinking_level_parse_accepts_case_and_aliases() {
        assert_eq!(ThinkingLevel::parse("low"), Some(ThinkingLevel::Low));
        assert_eq!(ThinkingLevel::parse("MEDIUM"), Some(ThinkingLevel::Medium));
        assert_eq!(ThinkingLevel::parse("Med"), Some(ThinkingLevel::Medium));
        assert_eq!(ThinkingLevel::parse(" High "), Some(ThinkingLevel::High));
        assert_eq!(ThinkingLevel::parse("xhigh"), Some(ThinkingLevel::XHigh));
        assert_eq!(ThinkingLevel::parse("x-high"), Some(ThinkingLevel::XHigh));
        assert_eq!(ThinkingLevel::parse("x_high"), Some(ThinkingLevel::XHigh));
        assert_eq!(ThinkingLevel::parse("wat"), None);
    }

    #[test]
    fn parses_error_message_from_api_body() {
        let json = br#"{"error":{"message":"API key invalid","status":"INVALID_ARGUMENT"}}"#;
        assert_eq!(parse_error_message(json), "API key invalid");
    }

    #[test]
    fn parse_error_falls_back_to_raw_body() {
        assert_eq!(parse_error_message(b"oh no"), "oh no");
    }
}
