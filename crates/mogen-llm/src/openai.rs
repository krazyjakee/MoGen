//! OpenAI Chat Completions client.
//!
//! Talks to `POST {base_url}/chat/completions` with the standard
//! `messages: [{role, content}]` shape. Supports the GPT-4o/4.1/5/5.5 line
//! and the o-series reasoning models — the latter pick up [`ThinkingLevel`]
//! via the top-level `reasoning_effort` field. We only emit that field for
//! models that accept it: current OpenAI Chat Completions rejects unknown
//! parameters with a 400, so sending it unconditionally would break
//! non-reasoning models like `gpt-4o`. Vision input (`cfg.user_images`) is
//! sent as `image_url` content parts on the user turn.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use thiserror::Error;

use crate::types::{GenerateConfig, GenerateResponse, Usage};

/// Default heavy text model. GPT-5.5 (Apr 2026) — current frontier model
/// with a 1M+ token context window and native multimodal input.
pub const DEFAULT_MODEL: &str = "gpt-5.5";

/// Default fast model used by the Studio Prompt Enhancer / Ask modal.
/// `gpt-5-mini` is the cheapest current-generation multimodal option; no
/// `gpt-5.5-mini` exists yet.
pub const DEFAULT_FAST_MODEL: &str = "gpt-5-mini";

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

/// Whether the model accepts the `reasoning_effort` field. Non-reasoning
/// models (`gpt-4o`, `gpt-4.1`, …) now 400 on unknown params, so we gate.
/// Prefix-matched so dated suffixes like `gpt-5.5-2026-04-23` still hit.
fn is_reasoning_model(model: &str) -> bool {
    let m = model.trim();
    m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
        || m.starts_with("gpt-5")
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

    // User turn: a plain string when no images are attached (cheaper to
    // serialize, matches what we send on repair iterations), or an array of
    // typed content parts when there are. OpenAI accepts `image_url` parts
    // with either an https URL or a `data:` URI; we always inline as base64
    // so callers don't need an upload step.
    let user_content = if cfg.user_images.is_empty() {
        serde_json::json!(cfg.user_prompt)
    } else {
        let mut parts: Vec<serde_json::Value> =
            Vec::with_capacity(cfg.user_images.len() + 1);
        for img in &cfg.user_images {
            let url = format!("data:{};base64,{}", img.mime_type, STANDARD.encode(&img.data));
            parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": url },
            }));
        }
        parts.push(serde_json::json!({ "type": "text", "text": cfg.user_prompt }));
        serde_json::json!(parts)
    };
    messages.push(serde_json::json!({ "role": "user", "content": user_content }));

    let mut req = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
    });

    if let Some(t) = cfg.temperature {
        req["temperature"] = serde_json::json!(t);
    }
    if let Some(s) = cfg.seed {
        req["seed"] = serde_json::json!(s as i64);
    }
    if let Some(level) = cfg.thinking_level {
        if is_reasoning_model(&cfg.model) {
            req["reasoning_effort"] = serde_json::json!(level.openai_effort());
        }
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
    use crate::types::{ImageInput, Role, ThinkingLevel, Turn};

    #[test]
    fn request_body_includes_system_message() {
        let mut cfg = GenerateConfig::new("hello");
        cfg.model = "gpt-5.5".into();
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
        cfg.model = "gpt-5.5".into();
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
        cfg.model = "gpt-5.5".into();
        cfg.seed = Some(7);
        cfg.temperature = Some(0.42);
        let body = build_request(&cfg);
        assert_eq!(body["seed"], 7);
        assert!((body["temperature"].as_f64().unwrap() - 0.42).abs() < 1e-6);
    }

    #[test]
    fn reasoning_effort_emitted_for_reasoning_models() {
        for model in ["o3", "o4-mini", "gpt-5", "gpt-5-mini", "gpt-5.5", "gpt-5.5-pro"] {
            let mut cfg = GenerateConfig::new("x");
            cfg.model = model.into();
            cfg.thinking_level = Some(ThinkingLevel::High);
            let body = build_request(&cfg);
            assert_eq!(body["reasoning_effort"], "high", "{model} should accept reasoning_effort");
            assert!(body.get("reasoning").is_none(), "{model} must not emit nested reasoning");
        }
    }

    #[test]
    fn reasoning_effort_omitted_for_non_reasoning_models() {
        for model in ["gpt-4o", "gpt-4o-mini", "gpt-4.1", "gpt-4.1-mini"] {
            let mut cfg = GenerateConfig::new("x");
            cfg.model = model.into();
            cfg.thinking_level = Some(ThinkingLevel::High);
            let body = build_request(&cfg);
            assert!(
                body.get("reasoning_effort").is_none(),
                "{model} must not receive reasoning_effort (server 400s on unknown params)",
            );
        }
    }

    #[test]
    fn user_turn_is_string_when_no_images() {
        let mut cfg = GenerateConfig::new("hello");
        cfg.model = "gpt-5.5".into();
        let body = build_request(&cfg);
        let last = body["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(last["content"], "hello");
    }

    #[test]
    fn user_turn_emits_image_url_parts_when_images_attached() {
        let mut cfg = GenerateConfig::new("describe this");
        cfg.model = "gpt-5.5".into();
        cfg.user_images.push(ImageInput {
            mime_type: "image/png".into(),
            data: vec![0x89, 0x50, 0x4e, 0x47],
        });
        let body = build_request(&cfg);
        let last = body["messages"].as_array().unwrap().last().unwrap();
        let parts = last["content"].as_array().expect("content should be array of parts");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "image_url");
        let url = parts[0]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"), "got {url}");
        assert_eq!(parts[1]["type"], "text");
        assert_eq!(parts[1]["text"], "describe this");
    }

    #[test]
    fn parse_error_message_extracts_nested_field() {
        let raw = br#"{"error":{"message":"invalid api key","type":"auth"}}"#;
        assert_eq!(parse_error_message(raw), "invalid api key");
    }
}
