//! Anthropic Messages API client.
//!
//! Talks to `POST {base_url}/messages` with the Claude-style
//! `messages: [{role, content}]` shape. The system instruction goes in a
//! top-level `system` field rather than as a message role.
//!
//! [`ThinkingLevel`] is mapped to the `thinking` block (Claude 3.7+ extended
//! thinking). Pre-3.7 models silently ignore the field.

use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::types::{GenerateConfig, GenerateResponse, Usage};

/// Default heavy text model. Sonnet sits at the price/quality sweet spot for
/// the structured-output workload `mogen` runs.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-5";

/// Default fast model used by the Studio Prompt Enhancer / Ask modal.
pub const DEFAULT_FAST_MODEL: &str = "claude-haiku-4-5";

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Required `anthropic-version` header. Pinned to a known-good revision
/// rather than `latest` so a future breaking change can't silently break the
/// CLI.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Cap on `max_tokens`. Required by the Messages API. Set high enough to
/// fit a sizeable DSL file without truncation; callers that want a hard cap
/// should set [`GenerateConfig::budget_tokens`] instead.
const DEFAULT_MAX_TOKENS: u32 = 8192;

#[derive(Debug, Error)]
pub enum AnthropicError {
    #[error("missing ANTHROPIC_API_KEY (set env var or pass --api-key)")]
    MissingApiKey,
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("empty response: model produced no text content")]
    EmptyResponse,
    #[error("budget exceeded: {used} input+output tokens exceeds --budget-tokens={budget}")]
    BudgetExceeded { used: u32, budget: u32 },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

pub struct AnthropicClient {
    http: reqwest::blocking::Client,
    api_key: String,
    base_url: String,
}

impl AnthropicClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self { http, api_key: api_key.into(), base_url: base_url.into() }
    }

    pub fn from_env() -> Result<Self, AnthropicError> {
        let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| AnthropicError::MissingApiKey)?;
        if key.trim().is_empty() {
            return Err(AnthropicError::MissingApiKey);
        }
        Ok(Self::new(key))
    }

    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, AnthropicError> {
        let url = format!("{}/messages", self.base_url);
        let body = build_request(cfg);

        let resp = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()?;
        let status = resp.status();
        let bytes = resp.bytes()?;

        if !status.is_success() {
            let message = parse_error_message(&bytes);
            return Err(AnthropicError::Api { status: status.as_u16(), message });
        }

        let parsed: RawMessageResponse = serde_json::from_slice(&bytes)
            .map_err(|e| AnthropicError::InvalidResponse(e.to_string()))?;

        // Concatenate every `text` content block in order. Thinking blocks
        // (if any) are skipped — we only want the assistant's actual reply.
        let mut text = String::new();
        for block in parsed.content {
            if block.kind == "text" {
                if let Some(t) = block.text {
                    text.push_str(&t);
                }
            }
        }
        if text.trim().is_empty() {
            return Err(AnthropicError::EmptyResponse);
        }

        let usage = parsed
            .usage
            .map(|u| {
                let prompt = u.input_tokens;
                let response = u.output_tokens;
                let cached = u.cache_read_input_tokens.unwrap_or(0);
                Usage {
                    prompt_tokens: prompt,
                    response_tokens: response,
                    total_tokens: prompt + response,
                    cached_tokens: cached,
                }
            })
            .unwrap_or_default();

        if let Some(budget) = cfg.budget_tokens {
            if usage.total_tokens > budget {
                return Err(AnthropicError::BudgetExceeded {
                    used: usage.total_tokens,
                    budget,
                });
            }
        }

        Ok(GenerateResponse { text, usage })
    }
}

fn build_request(cfg: &GenerateConfig) -> serde_json::Value {
    let mut messages: Vec<serde_json::Value> = Vec::new();
    for turn in &cfg.history {
        messages.push(serde_json::json!({
            "role": turn.role.chat_str(),
            "content": turn.text,
        }));
    }
    messages.push(serde_json::json!({
        "role": "user",
        "content": cfg.user_prompt,
    }));

    let mut req = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "max_tokens": DEFAULT_MAX_TOKENS,
    });

    if let Some(sys) = &cfg.system_instruction {
        req["system"] = serde_json::json!(sys);
    }
    if let Some(t) = cfg.temperature {
        req["temperature"] = serde_json::json!(t);
    }
    if let Some(level) = cfg.thinking_level {
        // Extended thinking requires `temperature = 1.0` server-side; quietly
        // raise it so the call doesn't 400 out. Users who want deterministic-ish
        // sampling should leave `thinking_level = None`.
        let budget = level.budget();
        // `max_tokens` must be greater than `thinking.budget_tokens`.
        let needed = budget.saturating_add(2048);
        if needed > DEFAULT_MAX_TOKENS {
            req["max_tokens"] = serde_json::json!(needed);
        }
        req["thinking"] = serde_json::json!({
            "type": "enabled",
            "budget_tokens": budget,
        });
        req["temperature"] = serde_json::json!(1.0);
    }
    // Anthropic accepts neither `seed` (no determinism control) nor a
    // top-level cache field on the wire — system-prompt caching is opt-in
    // via per-block `cache_control: { "type": "ephemeral" }`. We attach that
    // to the system block when one is present so repeated calls in the same
    // session (e.g. the repair loop) reuse the input cache.
    if let Some(sys) = &cfg.system_instruction {
        req["system"] = serde_json::json!([{
            "type": "text",
            "text": sys,
            "cache_control": { "type": "ephemeral" },
        }]);
    }
    req
}

fn parse_error_message(bytes: &[u8]) -> String {
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
struct RawMessageResponse {
    #[serde(default)]
    content: Vec<RawContentBlock>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ThinkingLevel;

    #[test]
    fn request_body_routes_system_to_top_level_field() {
        let mut cfg = GenerateConfig::new("hello");
        cfg.model = "claude-sonnet-4-5".into();
        cfg.system_instruction = Some("be helpful".into());
        let body = build_request(&cfg);
        // System is wrapped in a cache_control'd text block.
        assert_eq!(body["system"][0]["text"], "be helpful");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn request_body_includes_max_tokens() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = "claude-sonnet-4-5".into();
        cfg.thinking_level = None;
        let body = build_request(&cfg);
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn thinking_level_high_emits_extended_thinking_block() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = "claude-sonnet-4-5".into();
        cfg.thinking_level = Some(ThinkingLevel::High);
        let body = build_request(&cfg);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], ThinkingLevel::High.budget());
        // Extended thinking demands temperature = 1.0; we override.
        assert!((body["temperature"].as_f64().unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn thinking_xhigh_bumps_max_tokens_above_budget() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = "claude-sonnet-4-5".into();
        cfg.thinking_level = Some(ThinkingLevel::XHigh);
        let body = build_request(&cfg);
        let cap = body["max_tokens"].as_u64().unwrap() as u32;
        assert!(cap > ThinkingLevel::XHigh.budget());
    }

    #[test]
    fn parse_error_message_extracts_nested_field() {
        let raw = br#"{"type":"error","error":{"type":"auth","message":"bad key"}}"#;
        assert_eq!(parse_error_message(raw), "bad key");
    }
}
