//! OpenAI Chat Completions client.
//!
//! Talks to `POST {base_url}/chat/completions` with the standard
//! `messages: [{role, content}]` shape. Supports the GPT-4/4o/4.1/5 line and
//! the o-series reasoning models — the latter pick up [`ThinkingLevel`] via
//! the `reasoning.effort` field, which non-reasoning models silently ignore.

use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::types::{GenerateConfig, GenerateResponse, ThinkingLevel, Usage};

/// Default heavy text model. Picked for broad availability and a sensible
/// quality/cost ratio for structured-output tasks like DSL generation.
pub const DEFAULT_MODEL: &str = "gpt-4o";

/// Default fast model used by the Studio Prompt Enhancer / Ask modal.
pub const DEFAULT_FAST_MODEL: &str = "gpt-4o-mini";

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Debug, Error)]
pub enum OpenAIError {
    #[error("missing OPENAI_API_KEY (set env var or pass --api-key)")]
    MissingApiKey,
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("empty response: model produced no choices or text")]
    EmptyResponse,
    #[error("budget exceeded: {used} input+output tokens exceeds --budget-tokens={budget}")]
    BudgetExceeded { used: u32, budget: u32 },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

pub struct OpenAIClient {
    http: reqwest::blocking::Client,
    api_key: String,
    base_url: String,
}

impl OpenAIClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        // Short connect_timeout fails fast when offline; the long overall
        // timeout still covers slow reasoning models.
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self { http, api_key: api_key.into(), base_url: base_url.into() }
    }

    pub fn from_env() -> Result<Self, OpenAIError> {
        let key = std::env::var("OPENAI_API_KEY").map_err(|_| OpenAIError::MissingApiKey)?;
        if key.trim().is_empty() {
            return Err(OpenAIError::MissingApiKey);
        }
        Ok(Self::new(key))
    }

    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, OpenAIError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = build_request(cfg);

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()?;
        let status = resp.status();
        let bytes = resp.bytes()?;

        if !status.is_success() {
            let message = parse_error_message(&bytes);
            return Err(OpenAIError::Api { status: status.as_u16(), message });
        }

        let parsed: RawChatResponse = serde_json::from_slice(&bytes)
            .map_err(|e| OpenAIError::InvalidResponse(e.to_string()))?;

        let text = parsed
            .choices
            .into_iter()
            .find_map(|c| c.message.content)
            .ok_or(OpenAIError::EmptyResponse)?;

        if text.trim().is_empty() {
            return Err(OpenAIError::EmptyResponse);
        }

        let usage = parsed
            .usage
            .map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                response_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
                cached_tokens: u
                    .prompt_tokens_details
                    .and_then(|d| d.cached_tokens)
                    .unwrap_or(0),
            })
            .unwrap_or_default();

        if let Some(budget) = cfg.budget_tokens {
            if usage.total_tokens > budget {
                return Err(OpenAIError::BudgetExceeded { used: usage.total_tokens, budget });
            }
        }

        Ok(GenerateResponse { text, usage })
    }
}

fn build_request(cfg: &GenerateConfig) -> serde_json::Value {
    let mut messages: Vec<serde_json::Value> = Vec::new();
    if let Some(sys) = &cfg.system_instruction {
        messages.push(serde_json::json!({ "role": "system", "content": sys }));
    }
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
    });

    if let Some(t) = cfg.temperature {
        req["temperature"] = serde_json::json!(t);
    }
    if let Some(s) = cfg.seed {
        // OpenAI accepts seed as a 64-bit signed integer.
        req["seed"] = serde_json::json!(s as i64);
    }
    if let Some(level) = cfg.thinking_level {
        // o-series and gpt-5 reasoning models accept this; non-reasoning
        // models silently ignore unknown top-level fields. We send it
        // unconditionally to keep the request shape uniform.
        req["reasoning"] = serde_json::json!({ "effort": level.openai_effort() });
    }
    let _ = ThinkingLevel::Low; // keep ThinkingLevel imported even if pruned later
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
struct RawChatResponse {
    #[serde(default)]
    choices: Vec<RawChoice>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawChoice {
    message: RawMessage,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<RawPromptDetails>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawPromptDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Role, Turn};

    #[test]
    fn request_body_includes_system_message() {
        let mut cfg = GenerateConfig::new("hello");
        cfg.model = "gpt-4o".into();
        cfg.system_instruction = Some("be helpful".into());
        let body = build_request(&cfg);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "be helpful");
        assert_eq!(messages.last().unwrap()["role"], "user");
        assert_eq!(messages.last().unwrap()["content"], "hello");
    }

    #[test]
    fn request_body_threads_history_with_assistant_role() {
        let mut cfg = GenerateConfig::new("again");
        cfg.model = "gpt-4o".into();
        cfg.history.push(Turn { role: Role::User, text: "make a chair".into() });
        cfg.history.push(Turn { role: Role::Model, text: "scene { box }".into() });
        let body = build_request(&cfg);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
    }

    #[test]
    fn request_body_includes_seed_and_temperature() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = "gpt-4o".into();
        cfg.seed = Some(7);
        cfg.temperature = Some(0.42);
        let body = build_request(&cfg);
        assert_eq!(body["seed"], 7);
        assert!((body["temperature"].as_f64().unwrap() - 0.42).abs() < 1e-6);
    }

    #[test]
    fn request_body_emits_reasoning_effort_for_thinking_level() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = "gpt-4o".into();
        cfg.thinking_level = Some(ThinkingLevel::High);
        let body = build_request(&cfg);
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn parse_error_message_extracts_nested_field() {
        let raw = br#"{"error":{"message":"invalid api key","type":"auth"}}"#;
        assert_eq!(parse_error_message(raw), "invalid api key");
    }
}
