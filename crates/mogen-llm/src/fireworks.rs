//! Fireworks AI client.
//!
//! Talks to `POST {base_url}/chat/completions` — Fireworks ships an
//! OpenAI-compatible Chat Completions surface (see
//! <https://docs.fireworks.ai/firepass>), so the wire shape mirrors the
//! [`crate::openai`] client. The split exists because the auth env var,
//! default model, and model id format differ enough that sharing a single
//! client would leak provider-specific knowledge into both call sites.
//!
//! Fire Pass routes (e.g. `accounts/fireworks/routers/kimi-k2p6`) bill the
//! Kimi K2 family at zero per-token cost for personal agentic coding use;
//! other models are charged at the standard per-token rate.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use thiserror::Error;

use crate::types::{GenerateConfig, GenerateResponse, Usage};

/// Default heavy text model. Picked to land on the Fire Pass router that
/// covers the latest-tier Kimi K2 with zero per-token cost — the user's
/// expected default for `accounts/fireworks/routers/kimi-k2p6`.
pub const DEFAULT_MODEL: &str = "accounts/fireworks/routers/kimi-k2p6";

/// Default fast model. Reuses the turbo variant of the same Kimi tier so
/// the Studio Prompt Enhancer / Ask modal land on the lower-latency router
/// without falling off the Fire Pass coverage list.
pub const DEFAULT_FAST_MODEL: &str = "accounts/fireworks/routers/kimi-k2p6-turbo";

const DEFAULT_BASE_URL: &str = "https://api.fireworks.ai/inference/v1";

#[derive(Debug, Error)]
pub enum FireworksError {
    #[error("missing FIREWORKS_API_KEY (set env var or pass --api-key)")]
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

pub struct FireworksClient {
    http: reqwest::blocking::Client,
    api_key: String,
    base_url: String,
}

impl FireworksClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self { http, api_key: api_key.into(), base_url: base_url.into() }
    }

    pub fn from_env() -> Result<Self, FireworksError> {
        let key = std::env::var("FIREWORKS_API_KEY").map_err(|_| FireworksError::MissingApiKey)?;
        if key.trim().is_empty() {
            return Err(FireworksError::MissingApiKey);
        }
        Ok(Self::new(key))
    }

    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, FireworksError> {
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
            return Err(FireworksError::Api { status: status.as_u16(), message });
        }

        let parsed: RawChatResponse = serde_json::from_slice(&bytes)
            .map_err(|e| FireworksError::InvalidResponse(e.to_string()))?;

        let text = parsed
            .choices
            .into_iter()
            .find_map(|c| c.message.content)
            .ok_or(FireworksError::EmptyResponse)?;

        if text.trim().is_empty() {
            return Err(FireworksError::EmptyResponse);
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
                return Err(FireworksError::BudgetExceeded { used: usage.total_tokens, budget });
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
    // User turn. When images are attached, switch to the OpenAI-compatible
    // vision shape: `content` becomes an array of `{type:"text"}` plus
    // `{type:"image_url", image_url:{url:"data:<mime>;base64,..."}}` parts.
    // Kimi K2.5 / K2.6 (the Fire Pass `kimi-k2p6` router) are native
    // multimodal models and accept this shape via Fireworks' OpenAI-
    // compatible endpoint. Text-first ordering matches the Z.ai chat
    // surface and OpenAI's documented convention.
    if cfg.user_images.is_empty() {
        messages.push(serde_json::json!({
            "role": "user",
            "content": cfg.user_prompt,
        }));
    } else {
        let mut parts: Vec<serde_json::Value> =
            Vec::with_capacity(cfg.user_images.len() + 1);
        parts.push(serde_json::json!({
            "type": "text",
            "text": cfg.user_prompt,
        }));
        for img in &cfg.user_images {
            let url = format!(
                "data:{};base64,{}",
                img.mime_type,
                STANDARD.encode(&img.data),
            );
            parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": url },
            }));
        }
        messages.push(serde_json::json!({
            "role": "user",
            "content": parts,
        }));
    }

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
    // Fireworks routers run Kimi K2; the model has no `reasoning.effort`
    // analogue and ignores unknown top-level fields. Skip emitting one so
    // the request body stays tight on the wire.
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
        if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
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
    use crate::types::ImageInput;

    #[test]
    fn request_body_includes_system_message() {
        let mut cfg = GenerateConfig::new("hello");
        cfg.model = DEFAULT_MODEL.into();
        cfg.system_instruction = Some("be helpful".into());
        let body = build_request(&cfg);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "be helpful");
        assert_eq!(messages.last().unwrap()["role"], "user");
        assert_eq!(messages.last().unwrap()["content"], "hello");
        assert_eq!(body["model"], DEFAULT_MODEL);
    }

    #[test]
    fn request_body_threads_history_with_assistant_role() {
        let mut cfg = GenerateConfig::new("again");
        cfg.model = DEFAULT_MODEL.into();
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
        cfg.model = DEFAULT_MODEL.into();
        cfg.seed = Some(7);
        cfg.temperature = Some(0.42);
        let body = build_request(&cfg);
        assert_eq!(body["seed"], 7);
        assert!((body["temperature"].as_f64().unwrap() - 0.42).abs() < 1e-6);
    }

    #[test]
    fn request_body_omits_reasoning_field() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = DEFAULT_MODEL.into();
        cfg.thinking_level = Some(crate::types::ThinkingLevel::High);
        let body = build_request(&cfg);
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn parse_error_message_extracts_nested_field() {
        let raw = br#"{"error":{"message":"invalid api key","type":"auth"}}"#;
        assert_eq!(parse_error_message(raw), "invalid api key");
    }

    #[test]
    fn parse_error_message_falls_back_to_top_level_message() {
        let raw = br#"{"message":"server overloaded"}"#;
        assert_eq!(parse_error_message(raw), "server overloaded");
    }

    #[test]
    fn default_model_is_kimi_k2p6_router() {
        assert_eq!(DEFAULT_MODEL, "accounts/fireworks/routers/kimi-k2p6");
    }

    #[test]
    fn request_body_uses_image_url_content_when_image_attached() {
        // Vision input on Fireworks must serialise as the OpenAI-compatible
        // `content: [{type:"text"}, {type:"image_url", image_url:{url:"data:..."}}]`
        // shape — Kimi K2.5 / K2.6 are native multimodal and accept this
        // wire format via Fireworks' OpenAI-compatible chat endpoint.
        let mut cfg = GenerateConfig::new("describe");
        cfg.model = DEFAULT_MODEL.into();
        cfg.user_images.push(ImageInput {
            mime_type: "image/png".into(),
            // Three bytes that base64-encode to "AQID".
            data: vec![0x01, 0x02, 0x03],
        });
        let body = build_request(&cfg);
        let messages = body["messages"].as_array().unwrap();
        let user = messages.last().unwrap();
        assert_eq!(user["role"], "user");
        let parts = user["content"].as_array().expect("content array on vision turn");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "describe");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AQID");
    }

    #[test]
    fn request_body_uses_string_content_when_no_image() {
        // The text-only fast path must keep emitting `content: <string>` —
        // a regression to an array shape would burn extra tokens and break
        // the existing wire-shape contract Fireworks documents for chat.
        let mut cfg = GenerateConfig::new("hello");
        cfg.model = DEFAULT_MODEL.into();
        let body = build_request(&cfg);
        let messages = body["messages"].as_array().unwrap();
        let user = messages.last().unwrap();
        assert_eq!(user["content"], "hello");
    }
}
