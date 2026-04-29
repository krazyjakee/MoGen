//! Ollama local-LLM client.
//!
//! Talks to `POST {base_url}/api/chat` against a local Ollama server
//! (default `http://localhost:11434`). The shape is similar to OpenAI but
//! lives under `message.content` and reports usage as `prompt_eval_count` /
//! `eval_count`.
//!
//! Ollama runs entirely on the user's machine and has no concept of
//! `cachedContents`, extended thinking, or per-call API keys. The
//! corresponding fields on [`GenerateConfig`] are silently ignored —
//! `seed` and `temperature` ride through `options.{seed, temperature}`.

use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::types::{GenerateConfig, GenerateResponse, Usage};

/// Default model. Pick a widely-available, mid-sized open weights model so
/// `mogen generate --provider ollama` works the moment a user runs
/// `ollama pull llama3.1`.
pub const DEFAULT_MODEL: &str = "llama3.1";

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

#[derive(Debug, Error)]
pub enum OllamaError {
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("empty response: model produced no message content")]
    EmptyResponse,
    #[error("budget exceeded: {used} input+output tokens exceeds --budget-tokens={budget}")]
    BudgetExceeded { used: u32, budget: u32 },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

pub struct OllamaClient {
    http: reqwest::blocking::Client,
    /// Optional bearer token. Empty for the common keyless local-only setup.
    api_key: String,
    base_url: String,
}

impl OllamaClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        // Local generation can take many minutes on small CPUs; give it the
        // same headroom as the cloud paths. The short connect_timeout fails
        // fast when the local Ollama server isn't running, instead of waiting
        // for the OS to give up on `localhost:11434`.
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self { http, api_key: api_key.into(), base_url: base_url.into() }
    }

    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, OllamaError> {
        let url = format!("{}/api/chat", self.base_url);
        let body = build_request(cfg);

        let mut req = self.http.post(&url).json(&body);
        if !self.api_key.trim().is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req.send()?;
        let status = resp.status();
        let bytes = resp.bytes()?;

        if !status.is_success() {
            let message = parse_error_message(&bytes);
            return Err(OllamaError::Api { status: status.as_u16(), message });
        }

        let parsed: RawChatResponse = serde_json::from_slice(&bytes)
            .map_err(|e| OllamaError::InvalidResponse(e.to_string()))?;

        let text = parsed.message.and_then(|m| m.content).unwrap_or_default();
        if text.trim().is_empty() {
            return Err(OllamaError::EmptyResponse);
        }

        let prompt = parsed.prompt_eval_count.unwrap_or(0);
        let response = parsed.eval_count.unwrap_or(0);
        let usage = Usage {
            prompt_tokens: prompt,
            response_tokens: response,
            total_tokens: prompt + response,
            cached_tokens: 0,
        };

        if let Some(budget) = cfg.budget_tokens {
            if usage.total_tokens > budget {
                return Err(OllamaError::BudgetExceeded { used: usage.total_tokens, budget });
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

    let mut options = serde_json::Map::new();
    if let Some(t) = cfg.temperature {
        options.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(s) = cfg.seed {
        // Ollama's seed is an i32 — saturate.
        let clipped = (s as i64) & 0x7FFF_FFFF;
        options.insert("seed".into(), serde_json::json!(clipped));
    }

    let mut req = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        // Streaming would interleave many partial JSON docs in the response
        // body; the synchronous client only handles a single response.
        "stream": false,
    });
    if !options.is_empty() {
        req["options"] = serde_json::Value::Object(options);
    }
    req
}

fn parse_error_message(bytes: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if let Some(msg) = v.get("error").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawChatResponse {
    #[serde(default)]
    message: Option<RawMessage>,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    #[serde(default)]
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Role, Turn};

    #[test]
    fn request_body_includes_system_message() {
        let mut cfg = GenerateConfig::new("hello");
        cfg.model = "llama3.1".into();
        cfg.system_instruction = Some("be helpful".into());
        let body = build_request(&cfg);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "be helpful");
    }

    #[test]
    fn request_body_threads_history_with_assistant_role() {
        let mut cfg = GenerateConfig::new("again");
        cfg.model = "llama3.1".into();
        cfg.history.push(Turn { role: Role::Model, text: "scene { box }".into() });
        let body = build_request(&cfg);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "assistant");
    }

    #[test]
    fn options_carry_seed_and_temperature() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = "llama3.1".into();
        cfg.seed = Some(7);
        cfg.temperature = Some(0.42);
        let body = build_request(&cfg);
        assert_eq!(body["options"]["seed"], 7);
        assert!((body["options"]["temperature"].as_f64().unwrap() - 0.42).abs() < 1e-6);
    }

    #[test]
    fn streaming_is_disabled() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = "llama3.1".into();
        let body = build_request(&cfg);
        assert_eq!(body["stream"], false);
    }
}
