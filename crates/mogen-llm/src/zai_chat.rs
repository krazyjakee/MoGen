//! Z.ai chat-completions client.
//!
//! Talks to `POST {base_url}/chat/completions` — Z.ai exposes an
//! OpenAI-compatible Chat Completions surface at
//! `https://api.z.ai/api/paas/v4`, so the wire shape mirrors the
//! [`crate::openai`] client. The split exists because the auth env var,
//! default model id, and base URL all differ from OpenAI.
//!
//! The Z.ai *image* path lives in [`crate::zai`] and uses the same API
//! key (Z.ai issues a single key per account that covers both surfaces).

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use thiserror::Error;

use crate::types::{GenerateConfig, GenerateResponse, Usage};

/// Default heavy text model. Z.ai's GLM-5.1 release.
pub const DEFAULT_MODEL: &str = "glm-5.1";

/// Default fast model. Same family as the heavy default; users can
/// override per-tab in Studio if they want a cheaper/faster tier.
pub const DEFAULT_FAST_MODEL: &str = "glm-5.1";

/// Vision-capable Z.ai model. Triggered by [`build_request`] whenever the
/// caller passes a non-empty [`GenerateConfig::user_images`]; the Studio's
/// worker also forces this id when an image is attached on the Z.ai
/// provider so the user-facing model dropdown stays advisory.
pub const DEFAULT_VISION_MODEL: &str = "glm-5v-turbo";

/// Default Z.ai chat-completions endpoint. The general "PaaS v4" surface
/// covers every account but is rate-limited more aggressively for the
/// dedicated coding plan; coding-plan keys carrying heavy system
/// instructions (the MoGen DSL prompt) often trip an `os error 10054`
/// (peer reset) on this URL.
pub const DEFAULT_BASE_URL: &str = "https://api.z.ai/api/paas/v4";

/// Dedicated GLM Coding Plan endpoint. Z.ai documents this surface as the
/// one purpose-built for tools like Claude Code, Cline, Crush, MoGen
/// Studio (see <https://docs.z.ai/devpack/overview>); it accepts the same
/// auth and wire shape as `DEFAULT_BASE_URL` but is the supported target
/// for users on the GLM Coding Plan.
pub const CODING_PLAN_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";

#[derive(Debug, Error)]
pub enum ZaiChatError {
    #[error("missing ZAI_API_KEY (set env var or pass --api-key)")]
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

pub struct ZaiChatClient {
    http: reqwest::blocking::Client,
    api_key: String,
    base_url: String,
}

impl ZaiChatClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self {
            http,
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    pub fn from_env() -> Result<Self, ZaiChatError> {
        let key = std::env::var("ZAI_API_KEY").map_err(|_| ZaiChatError::MissingApiKey)?;
        if key.trim().is_empty() {
            return Err(ZaiChatError::MissingApiKey);
        }
        Ok(Self::new(key))
    }

    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, ZaiChatError> {
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
            return Err(ZaiChatError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: RawChatResponse = serde_json::from_slice(&bytes)
            .map_err(|e| ZaiChatError::InvalidResponse(e.to_string()))?;

        let text = parsed
            .choices
            .into_iter()
            .find_map(|c| c.message.content)
            .ok_or(ZaiChatError::EmptyResponse)?;

        if text.trim().is_empty() {
            return Err(ZaiChatError::EmptyResponse);
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
                return Err(ZaiChatError::BudgetExceeded {
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
    // Z.ai's docs recommend text-first ordering on the chat surface, which
    // also matches what `glm-5v-turbo` expects in production.
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
        // Round-trip through Display → f64 to strip the f32-widening
        // artifact: `serde_json::json!(0.3_f32)` emits
        // "0.30000001192092896", which Z.ai 400s as "Invalid API
        // parameter" even though the value is within `[0.0, 1.0]`.
        // Going through `format!("{}", t)` lands on the shortest
        // decimal that round-trips ("0.3"), parsed as f64 it's the
        // closest representable, and serde_json's ryu emits the same
        // short form back on the wire.
        let t_str = format!("{}", t);
        let t_f64: f64 = t_str.parse().unwrap_or_else(|_| f64::from(t));
        req["temperature"] = serde_json::json!(t_f64);
    }
    // Note: Z.ai's chat completions surface does NOT accept a `seed`
    // field — the schema (https://docs.z.ai/api-reference/llm/chat-completion)
    // exposes only `temperature`, `top_p`, `max_tokens`, `do_sample`,
    // `thinking`, etc. Sending `seed` 400s the request with "Invalid API
    // parameter" regardless of the value's type. The seed still rides
    // along in the DSL `meta(seed=...)` header for cross-provider
    // reproducibility; we just don't put it on the wire here.
    let _ = cfg.seed;
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
    fn request_body_omits_seed_and_includes_temperature() {
        // Z.ai's chat completions schema does not list `seed` as a
        // recognised field — sending it 400s the request with
        // "Invalid API parameter" regardless of value type. The seed
        // travels through the DSL `meta(seed=…)` header instead, so
        // cross-provider reproducibility still works.
        let mut cfg = GenerateConfig::new("x");
        cfg.model = DEFAULT_MODEL.into();
        cfg.seed = Some(7);
        cfg.temperature = Some(0.42);
        let body = build_request(&cfg);
        assert!(
            body.get("seed").is_none(),
            "seed must not appear in the wire payload: {body}"
        );
        assert!((body["temperature"].as_f64().unwrap() - 0.42).abs() < 1e-6);
    }

    #[test]
    fn temperature_avoids_f32_widening_artifact() {
        // f32 0.3 widens to f64 0.30000001192092896 — Z.ai's backend
        // 400s anything outside the documented `[0.0, 1.0]` decimal
        // form, even though the f64 value still satisfies the bound.
        // The fix routes through Display → parse → f64 so the shortest
        // round-trip decimal (0.3) is what ends up on the wire.
        let mut cfg = GenerateConfig::new("x");
        cfg.model = DEFAULT_MODEL.into();
        cfg.temperature = Some(0.3);
        let body = build_request(&cfg);
        let serialized = serde_json::to_string(&body["temperature"]).unwrap();
        assert_eq!(serialized, "0.3", "got: {serialized}");
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
    fn default_model_is_glm_5_1() {
        assert_eq!(DEFAULT_MODEL, "glm-5.1");
    }

    #[test]
    fn nanos_seed_does_not_appear_on_the_wire() {
        // Studio's `pick_default_seed()` returns `as_nanos() as u64`,
        // which can be ~1.7e18 — a value that previously overflowed a
        // Java `int` on the wire. The fix is to drop the field
        // entirely; this test pins that the field is absent regardless
        // of how large the seed grows.
        let mut cfg = GenerateConfig::new("x");
        cfg.model = DEFAULT_MODEL.into();
        cfg.seed = Some(1_778_138_120_133_371_400u64);
        let body = build_request(&cfg);
        assert!(
            body.get("seed").is_none(),
            "seed must not appear in the wire payload: {body}"
        );
    }

    #[test]
    fn user_image_becomes_image_url_part() {
        // Vision input: a non-empty `user_images` switches the user
        // message's `content` from a plain string to an array of typed
        // parts (text + image_url). The image is delivered as a
        // `data:<mime>;base64,...` URL — Z.ai's chat surface rejects raw
        // bytes / hosted-URL forms when the model is glm-5v-turbo.
        let mut cfg = GenerateConfig::new("describe this");
        cfg.model = DEFAULT_VISION_MODEL.into();
        cfg.user_images.push(crate::types::ImageInput {
            mime_type: "image/png".into(),
            data: vec![0x01, 0x02, 0x03],
        });
        let body = build_request(&cfg);
        let messages = body["messages"].as_array().unwrap();
        let user_msg = messages.last().unwrap();
        assert_eq!(user_msg["role"], "user");
        let parts = user_msg["content"].as_array().expect("content is array");
        assert_eq!(parts.len(), 2, "got: {body}");
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "describe this");
        assert_eq!(parts[1]["type"], "image_url");
        let url = parts[1]["image_url"]["url"]
            .as_str()
            .expect("image_url.url is string");
        // Three bytes of 0x01 0x02 0x03 base64-encode to "AQID".
        assert_eq!(url, "data:image/png;base64,AQID");
    }
}
