//! Provider-agnostic request/response types shared by every LLM backend.
//!
//! Every provider implementation in this crate (`gemini`, `openai`,
//! `anthropic`, `ollama`) consumes [`GenerateConfig`] and produces
//! [`GenerateResponse`]. Provider-specific knobs that have no analogue across
//! backends (e.g. Gemini's `cachedContents`) live on [`GenerateConfig`] as
//! optional fields and are simply ignored by providers that don't honour them.

/// Default sampling temperature. Low because `mogen` DSL is a structured
/// output task that compiles — creative variance costs repair iterations.
pub const DEFAULT_TEMPERATURE: f32 = 0.3;

/// Cap on the total reasoning budget the model may spend before emitting a
/// token. Each provider maps these buckets onto its own native control:
///
/// - Gemini: `generationConfig.thinkingConfig.thinkingBudget` (token count).
/// - Anthropic: `thinking.budget_tokens` (token count, requires
///   `thinking.type = "enabled"`).
/// - OpenAI: `reasoning.effort` (`low`/`medium`/`high`/`high`).
/// - Ollama: ignored (local models don't expose a separate reasoning budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingLevel {
    /// 512 tokens / `low` effort — fast path. Usually enough for a
    /// well-specified DSL prompt.
    Low,
    /// 2048 tokens / `medium` effort — modest reasoning for slightly
    /// ambiguous prompts.
    Medium,
    /// 8192 tokens / `high` effort — for complex scenes where the model
    /// benefits from planning.
    High,
    /// 24576 tokens / `high` effort — near-max; use when you're willing to
    /// trade ~2 min for quality. OpenAI tops out at `high`, so XHigh and
    /// High collapse to the same setting there.
    XHigh,
}

impl ThinkingLevel {
    /// Token budget — used by providers that take a numeric cap (Gemini,
    /// Anthropic). Concrete values stay within both Gemini Pro (128–32768)
    /// and Flash (0–24576) ranges.
    pub fn budget(self) -> u32 {
        match self {
            ThinkingLevel::Low => 512,
            ThinkingLevel::Medium => 2048,
            ThinkingLevel::High => 8192,
            ThinkingLevel::XHigh => 24576,
        }
    }

    /// OpenAI `reasoning.effort` mapping. OpenAI exposes `low`, `medium`,
    /// `high` only — XHigh maps to `high`.
    pub fn openai_effort(self) -> &'static str {
        match self {
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High | ThinkingLevel::XHigh => "high",
        }
    }

    /// Parse a case-insensitive label. Accepts `low`, `medium`, `high`,
    /// `xhigh` (with `x-high` / `x_high` aliases for the last, since some
    /// shells mangle it).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" | "x-high" | "x_high" => Some(Self::XHigh),
            _ => None,
        }
    }

    /// Canonical lowercase key; round-trips through [`Self::parse`]. Used
    /// when writing the file's `meta(thinking=…)` attribute.
    pub fn key(self) -> &'static str {
        match self {
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::XHigh => "xhigh",
        }
    }
}

/// One image attached to the user turn. Sent to providers that support
/// vision input (see [`crate::Provider::supports_images`]) as a base64-encoded
/// inline part alongside the text prompt — `inline_data` for Gemini,
/// `image_url` data-URI for OpenAI Chat Completions. Non-vision providers
/// silently ignore the field. Used by Studio's "New from Prompt" dialog to
/// enable image-to-3D generation.
#[derive(Debug, Clone)]
pub struct ImageInput {
    /// MIME type, e.g. `"image/png"` or `"image/jpeg"`. Must start with
    /// `image/` — Gemini rejects anything else on a vision turn.
    pub mime_type: String,
    /// Raw bytes of the image. Encoded as base64 by the provider client.
    pub data: Vec<u8>,
}

/// One full request to a backend's chat-completion-style endpoint.
///
/// The same struct is consumed by every provider in this crate. Fields that
/// only one provider understands (`cached_content` and `user_images` for
/// Gemini, `seed` for some) are silently ignored elsewhere.
#[derive(Debug, Clone)]
pub struct GenerateConfig {
    pub model: String,
    /// Single-shot prompt from the user (e.g. `"a wooden stool"`). May be
    /// empty when [`Self::user_images`] is non-empty — the image alone is a
    /// valid input on vision-capable providers.
    pub user_prompt: String,
    /// Optional images attached to the user turn (image-to-3D). Vision-capable
    /// providers re-send these on every call (including repair iterations) so
    /// the model retains the visual reference while it fixes validator errors.
    /// Non-vision providers ignore this field.
    pub user_images: Vec<ImageInput>,
    /// Prior turns (e.g. first attempt's DSL + diagnostic feedback for repair).
    pub history: Vec<Turn>,
    /// System instruction (grammar + stdlib). See [`crate::prompt`].
    pub system_instruction: Option<String>,
    /// **Gemini only.** Name of a `cachedContents` resource — if set, the
    /// stable reference block lives server-side and is not re-uploaded per
    /// call. May coexist with `system_instruction`: the Gemini API forbids
    /// sending both `cachedContent` and `systemInstruction` on the same
    /// request, so the client moves the inline portion into a synthetic
    /// preamble inside `contents` instead. Other providers ignore this field.
    pub cached_content: Option<String>,
    /// Output cap enforced client-side after the response arrives.
    /// `None` means no cap.
    pub budget_tokens: Option<u32>,
    /// Sampling temperature. `None` uses the API default;
    /// [`Self::new`] sets this to [`DEFAULT_TEMPERATURE`].
    pub temperature: Option<f32>,
    /// Server-side seed request — note that no provider currently
    /// guarantees determinism, so callers also embed the seed in the DSL
    /// header. Ollama and Gemini honour this; Anthropic and OpenAI partially.
    pub seed: Option<u64>,
    /// Cap on server-side reasoning. `None` lets the provider use its
    /// default. Library default (set by [`Self::new`]) is
    /// [`ThinkingLevel::High`] — a middle ground that bounds latency while
    /// still leaving room for the model to plan complex scenes.
    pub thinking_level: Option<ThinkingLevel>,
}

impl GenerateConfig {
    pub fn new(user_prompt: impl Into<String>) -> Self {
        Self {
            model: String::new(),
            user_prompt: user_prompt.into(),
            user_images: Vec::new(),
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
    /// Wire string used by Gemini (`user`/`model`).
    pub fn gemini_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Model => "model",
        }
    }

    /// Wire string used by OpenAI / Ollama / Anthropic (`user`/`assistant`).
    pub fn chat_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Model => "assistant",
        }
    }
}

/// Token usage reported by the provider for a single call. Fields are
/// best-effort: providers that don't break out cached tokens leave that
/// counter at 0.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub response_tokens: u32,
    pub total_tokens: u32,
    /// Portion of `prompt_tokens` served from a cached prefix — billed at a
    /// reduced rate. Only Gemini reports this today.
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

/// Decoded text + telemetry from a single provider call.
#[derive(Debug, Clone)]
pub struct GenerateResponse {
    pub text: String,
    pub usage: Usage,
}
