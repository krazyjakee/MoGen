//! Hand-rolled Gemini REST client.
//!
//! SDK crates lag the spec; we need `cachedContents` and structured errors.
//! Blocking HTTP keeps the `mgen` binary synchronous — one request per repair
//! iteration is not worth a tokio runtime.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default model for `mgen generate`. Alias auto-rolls to the newest Pro tier.
pub const DEFAULT_MODEL: &str = "gemini-pro-latest";

/// Default sampling temperature. Low because `mgen` DSL is a structured
/// output task that compiles — creative variance costs repair iterations.
pub const DEFAULT_TEMPERATURE: f32 = 0.3;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Preset buckets for Gemini's `thinkingConfig.thinkingBudget`. Gemini 2.5 Pro
/// enables reasoning by default and will happily spend 60–120 s "thinking"
/// before emitting a token — bounding the budget is the biggest single lever
/// on end-to-end latency for a structured-output task like DSL generation.
///
/// Concrete token counts are chosen to stay within both Pro (128–32768) and
/// Flash (0–24576) ranges, so the same preset works across models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingLevel {
    /// 512 tokens — fast path. Usually enough for a well-specified DSL prompt.
    Low,
    /// 2048 tokens — modest reasoning for slightly ambiguous prompts.
    Medium,
    /// 8192 tokens — for complex scenes where the model benefits from planning.
    High,
    /// 24576 tokens — near-max; use when you're willing to trade 2 min for quality.
    XHigh,
}

impl ThinkingLevel {
    /// Token budget sent as `generationConfig.thinkingConfig.thinkingBudget`.
    pub fn budget(self) -> u32 {
        match self {
            ThinkingLevel::Low => 512,
            ThinkingLevel::Medium => 2048,
            ThinkingLevel::High => 8192,
            ThinkingLevel::XHigh => 24576,
        }
    }

    /// Parse a case-insensitive label. Accepts `low`, `medium`, `high`, `xhigh`
    /// (with an `x-high` / `x_high` alias for the last, since some shells mangle it).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" | "x-high" | "x_high" => Some(Self::XHigh),
            _ => None,
        }
    }
}

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

/// One full request to `generateContent`.
#[derive(Debug, Clone)]
pub struct GenerateConfig {
    pub model: String,
    /// Single-shot prompt from the user (e.g. `"a wooden stool"`).
    pub user_prompt: String,
    /// Prior turns (e.g. first attempt's DSL + diagnostic feedback for repair).
    pub history: Vec<Turn>,
    /// System instruction (grammar + stdlib). See [`crate::prompt`].
    pub system_instruction: Option<String>,
    /// Name of a `cachedContents` resource — if set, skips re-uploading
    /// the system instruction. Overrides `system_instruction`.
    pub cached_content: Option<String>,
    /// Output cap enforced client-side after the response arrives.
    /// `None` means no cap.
    pub budget_tokens: Option<u32>,
    /// Sampling temperature. `None` uses the API default (typically 1.0);
    /// [`Self::new`] sets this to [`DEFAULT_TEMPERATURE`].
    pub temperature: Option<f32>,
    /// Server-side seed request — note that Gemini does not currently
    /// guarantee determinism, so we also embed the seed in the DSL header.
    pub seed: Option<u64>,
    /// Cap on server-side reasoning. `None` lets the model use its default
    /// (dynamic, up to ~32k tokens on Pro) — which is what produces the
    /// 2-minute latencies. Library default is `Some(ThinkingLevel::High)` —
    /// a middle ground that still bounds latency while leaving room for the
    /// model to plan complex scenes.
    pub thinking_level: Option<ThinkingLevel>,
}

impl GenerateConfig {
    pub fn new(user_prompt: impl Into<String>) -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            user_prompt: user_prompt.into(),
            history: Vec::new(),
            system_instruction: None,
            cached_content: None,
            budget_tokens: None,
            temperature: Some(DEFAULT_TEMPERATURE),
            seed: None,
            thinking_level: Some(ThinkingLevel::High),
        }
    }
}

/// One turn of conversation, from either side.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Model,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Model => "model",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub response_tokens: u32,
    pub total_tokens: u32,
    /// Portion of `prompt_tokens` served from a `cachedContents` resource —
    /// billed at a reduced rate. Reported by the API when caching is used.
    pub cached_tokens: u32,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.response_tokens += other.response_tokens;
        self.total_tokens += other.total_tokens;
        self.cached_tokens += other.cached_tokens;
    }
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

#[derive(Debug, Clone)]
pub struct GenerateResponse {
    pub text: String,
    pub usage: Usage,
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
        // enabled can easily push past a couple of minutes end-to-end.
        let http = reqwest::blocking::Client::builder()
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
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, cfg.model, self.api_key
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
    for turn in &cfg.history {
        contents.push(serde_json::json!({
            "role": turn.role.as_str(),
            "parts": [{ "text": turn.text }],
        }));
    }
    contents.push(serde_json::json!({
        "role": "user",
        "parts": [{ "text": cfg.user_prompt }],
    }));

    let mut req = serde_json::json!({ "contents": contents });

    if let Some(cached) = &cfg.cached_content {
        req["cachedContent"] = serde_json::Value::String(cached.clone());
    } else if let Some(sys) = &cfg.system_instruction {
        req["systemInstruction"] = serde_json::json!({
            "parts": [{ "text": sys }],
        });
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
    fn cached_content_overrides_system_instruction() {
        let mut cfg = GenerateConfig::new("x");
        cfg.system_instruction = Some("should be ignored".into());
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
