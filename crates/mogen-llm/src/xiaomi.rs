//! Xiaomi MiMo Open Platform client.
//!
//! Talks to `POST {base_url}/chat/completions` on Xiaomi's OpenAI-compatible
//! API. The docs also expose an Anthropic-compatible endpoint, but the OpenAI
//! surface is the better fit for MoGen: it accepts standard Bearer auth, uses
//! the same image-url content parts as the existing OpenAI-style providers,
//! and keeps system messages in the `messages` array instead of needing a
//! separate Anthropic-style `system` field.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use thiserror::Error;

use crate::types::{GenerateConfig, GenerateResponse, ThinkingLevel, Usage};

/// Default heavy text model. MiMo-V2.5-Pro is Xiaomi's current reasoning/text
/// flagship, with a 1M context window and 128K maximum output.
pub const DEFAULT_MODEL: &str = "mimo-v2.5-pro";

/// Default fast model used by Studio's Prompt Enhancer / Ask modal.
pub const DEFAULT_FAST_MODEL: &str = "mimo-v2-flash";

/// Vision-capable MiMo model. Triggered by Studio/CLI refinement paths when a
/// screenshot or user reference image is attached.
pub const DEFAULT_VISION_MODEL: &str = "mimo-v2.5";

/// Xiaomi's defaults (32K–128K completion tokens on text models) are too high
/// for MoGen's "emit one DSL file" contract, but the reasoning-enabled models
/// still need more headroom than Flash or they can spend the whole cap on
/// `reasoning_content` and never reach the final DSL. Use a provider-specific
/// middle ground.
const MAX_COMPLETION_TOKENS_DISABLED: u32 = 8_192;
const MAX_COMPLETION_TOKENS_REASONING: u32 = 32_768;
const REQUEST_TIMEOUT_DISABLED_SECS: u64 = 600;
const REQUEST_TIMEOUT_REASONING_SECS: u64 = 1_800;

const DEFAULT_BASE_URL: &str = "https://api.xiaomimimo.com/v1";

#[derive(Debug, Error)]
pub enum XiaomiError {
    #[error("missing XIAOMI_API_KEY (set env var or pass --api-key)")]
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

pub struct XiaomiClient {
    http: reqwest::blocking::Client,
    api_key: String,
    base_url: String,
}

impl XiaomiClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        // Keep the client-level timeout unset. Xiaomi's reasoning-enabled
        // models can legitimately run much longer than the fast/text-only
        // paths, so `generate` applies the overall timeout per request based
        // on the active thinking mode.
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        Self {
            http,
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    pub fn from_env() -> Result<Self, XiaomiError> {
        let key = std::env::var("XIAOMI_API_KEY").map_err(|_| XiaomiError::MissingApiKey)?;
        if key.trim().is_empty() {
            return Err(XiaomiError::MissingApiKey);
        }
        Ok(Self::new(key))
    }

    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, XiaomiError> {
        if self.api_key.trim().is_empty() {
            return Err(XiaomiError::MissingApiKey);
        }

        let url = format!("{}/chat/completions", self.base_url);
        let body = build_request(cfg);

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .timeout(request_timeout(cfg))
            .json(&body)
            .send()?;
        let status = resp.status();

        let bytes = resp.bytes()?;

        if std::env::var("MOGEN_DEBUG_HTTP").ok().as_deref() == Some("1") {
            let preview = String::from_utf8_lossy(&bytes);
            let preview = if preview.len() > 4096 {
                &preview[..4096]
            } else {
                &preview
            };
            eprintln!(
                "[mogen xiaomi] model={} status={} body={}",
                cfg.model, status, preview
            );
        }

        if !status.is_success() {
            let message = parse_error_message(&bytes);
            return Err(XiaomiError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: RawChatResponse = serde_json::from_slice(&bytes)
            .map_err(|e| XiaomiError::InvalidResponse(e.to_string()))?;

        let text = parsed
            .choices
            .into_iter()
            .find_map(|c| c.message.content)
            .ok_or(XiaomiError::EmptyResponse)?;

        if text.trim().is_empty() {
            return Err(XiaomiError::EmptyResponse);
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
                return Err(XiaomiError::BudgetExceeded {
                    used: usage.total_tokens,
                    budget,
                });
            }
        }

        Ok(GenerateResponse { text, usage })
    }
}

fn requested_thinking_type(cfg: &GenerateConfig) -> &'static str {
    match cfg.thinking_level.unwrap_or(ThinkingLevel::High) {
        // Xiaomi only exposes enabled/disabled, not budget tiers. Treat
        // Low/Medium as "skip CoT" so fast/cheap calls behave like the other
        // providers' lower reasoning settings; High/XHigh leave CoT on.
        ThinkingLevel::Low | ThinkingLevel::Medium => "disabled",
        ThinkingLevel::High | ThinkingLevel::XHigh => "enabled",
    }
}

fn max_completion_tokens(cfg: &GenerateConfig) -> u32 {
    if requested_thinking_type(cfg) == "enabled" {
        MAX_COMPLETION_TOKENS_REASONING
    } else {
        MAX_COMPLETION_TOKENS_DISABLED
    }
}

/// Xiaomi's server-side thinking can hold a request open for much longer than
/// the no-CoT path. Give reasoning-enabled calls more wall-clock headroom so
/// Studio generation doesn't fail at the generic 10-minute cap, while still
/// letting low/medium-thinking requests fail fast on a truly stalled server.
fn request_timeout(cfg: &GenerateConfig) -> Duration {
    if requested_thinking_type(cfg) == "enabled" {
        Duration::from_secs(REQUEST_TIMEOUT_REASONING_SECS)
    } else {
        Duration::from_secs(REQUEST_TIMEOUT_DISABLED_SECS)
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

    // Xiaomi's vision docs use the same OpenAI-compatible content-part shape
    // as OpenAI itself: image_url parts first, text last. Text-only calls keep
    // the cheaper plain-string content shape.
    if cfg.user_images.is_empty() {
        messages.push(serde_json::json!({
            "role": "user",
            "content": cfg.user_prompt,
        }));
    } else {
        let mut parts: Vec<serde_json::Value> = Vec::with_capacity(cfg.user_images.len() + 1);
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
        parts.push(serde_json::json!({
            "type": "text",
            "text": cfg.user_prompt,
        }));
        messages.push(serde_json::json!({
            "role": "user",
            "content": parts,
        }));
    }

    let thinking_type = requested_thinking_type(cfg);
    let mut req = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "max_completion_tokens": max_completion_tokens(cfg),
        "stream": false,
        "thinking": { "type": thinking_type },
    });

    if let Some(t) = cfg.temperature {
        // Match the Z.ai workaround: serialise the shortest decimal for f32
        // values so provider-side schema validators never see a widened
        // artifact such as 0.30000001192092896.
        let t_str = format!("{}", t);
        let t_f64: f64 = t_str.parse().unwrap_or_else(|_| f64::from(t));
        req["temperature"] = serde_json::json!(t_f64);
    }

    // Xiaomi's documented OpenAI-compatible schema does not list `seed`. Keep
    // the DSL `meta(seed=...)` contract on the MoGen side, but do not send the
    // field over the wire.

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
    use crate::types::{ImageInput, Role, ThinkingLevel, Turn};

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
        cfg.history.push(Turn {
            role: Role::User,
            text: "make a chair".into(),
        });
        cfg.history.push(Turn {
            role: Role::Model,
            text: "scene { box }".into(),
        });
        let body = build_request(&cfg);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
    }

    #[test]
    fn request_body_omits_seed_and_maps_thinking_to_xiaomi_toggle() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = DEFAULT_MODEL.into();
        cfg.seed = Some(7);
        cfg.temperature = Some(0.3);
        cfg.thinking_level = Some(ThinkingLevel::High);
        let body = build_request(&cfg);
        assert!(
            body.get("seed").is_none(),
            "seed must stay out of Xiaomi payload: {body}"
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(serde_json::to_string(&body["temperature"]).unwrap(), "0.3");
        assert_eq!(
            body["max_completion_tokens"],
            MAX_COMPLETION_TOKENS_REASONING
        );
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn low_thinking_disables_cot_and_uses_small_cap() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = DEFAULT_FAST_MODEL.into();
        cfg.thinking_level = Some(ThinkingLevel::Low);
        let body = build_request(&cfg);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(
            body["max_completion_tokens"],
            MAX_COMPLETION_TOKENS_DISABLED
        );
    }

    #[test]
    fn high_thinking_uses_extended_timeout() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = DEFAULT_MODEL.into();
        cfg.thinking_level = Some(ThinkingLevel::High);
        assert_eq!(
            request_timeout(&cfg),
            Duration::from_secs(REQUEST_TIMEOUT_REASONING_SECS)
        );
    }

    #[test]
    fn medium_thinking_uses_default_timeout() {
        let mut cfg = GenerateConfig::new("x");
        cfg.model = DEFAULT_MODEL.into();
        cfg.thinking_level = Some(ThinkingLevel::Medium);
        assert_eq!(
            request_timeout(&cfg),
            Duration::from_secs(REQUEST_TIMEOUT_DISABLED_SECS)
        );
    }

    #[test]
    fn user_image_becomes_image_url_part_before_text() {
        let mut cfg = GenerateConfig::new("describe this");
        cfg.model = DEFAULT_VISION_MODEL.into();
        cfg.user_images.push(ImageInput {
            mime_type: "image/png".into(),
            data: vec![0x01, 0x02, 0x03],
        });
        let body = build_request(&cfg);
        let messages = body["messages"].as_array().unwrap();
        let user = messages.last().unwrap();
        assert_eq!(user["role"], "user");
        let parts = user["content"]
            .as_array()
            .expect("content array on vision turn");
        assert_eq!(parts.len(), 2, "got: {body}");
        assert_eq!(parts[0]["type"], "image_url");
        assert_eq!(parts[0]["image_url"]["url"], "data:image/png;base64,AQID");
        assert_eq!(parts[1]["type"], "text");
        assert_eq!(parts[1]["text"], "describe this");
    }

    #[test]
    fn parse_error_message_extracts_nested_field() {
        let raw = br#"{"error":{"message":"invalid api key","type":"auth"}}"#;
        assert_eq!(parse_error_message(raw), "invalid api key");
    }

    #[test]
    fn default_models_match_mimo_series() {
        assert_eq!(DEFAULT_MODEL, "mimo-v2.5-pro");
        assert_eq!(DEFAULT_FAST_MODEL, "mimo-v2-flash");
        assert_eq!(DEFAULT_VISION_MODEL, "mimo-v2.5");
    }
}
