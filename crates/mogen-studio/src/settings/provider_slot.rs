use mogen_llm::Provider;

/// Studio-side provider selection. Wraps [`Provider`] but splits Gemini into
/// two slots so the user can explicitly pick API-key or OAuth auth from the
/// Preferences dropdown. `mogen-llm` itself stays unaware — every slot maps
/// to a single underlying [`Provider`] via [`Self::to_provider`].
///
/// API-key vs OAuth was previously a fallback inside `resolve_credential`
/// (try saved key → env var → stored OAuth bundle). Users who want the OAuth
/// path while a key is also set had no way to express that. The slot makes
/// the choice explicit: GeminiApiKey forces the public-API path, GeminiOAuth
/// forces Cloud Code Assist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderSlot {
    GeminiApiKey,
    GeminiOAuth,
    OpenAI,
    Anthropic,
    Ollama,
    ClaudeCode,
    /// Fireworks AI's OpenAI-compatible Chat Completions surface. Default
    /// model is the Fire Pass `kimi-k2p6` router; users supply their
    /// `fw_…` API key from `https://fireworks.ai/account/api-keys`.
    Fireworks,
    /// Z.ai (Zhipu) GLM family. OpenAI-compatible Chat Completions; default
    /// model is `glm-5.1`. The same API key drives the `glm-image` texture
    /// path — see [`super::ImageProvider::ZAI`].
    Zai,
    /// Xiaomi MiMo Open Platform. OpenAI-compatible Chat Completions; default
    /// model is `mimo-v2.5-pro`, with `mimo-v2.5` used for image inputs.
    Xiaomi,
    /// Generic OpenAI-compatible local server (llama.cpp, LM Studio, any
    /// `/v1/chat/completions` host). The user supplies an arbitrary base URL
    /// in Preferences; routes through [`Provider::OpenAiCompat`]. Keyless by
    /// default. Text generation only — texture/image generation stays on a
    /// cloud provider.
    OpenAiCompat,
}

impl ProviderSlot {
    pub fn key(self) -> &'static str {
        match self {
            ProviderSlot::GeminiApiKey => "gemini-apikey",
            ProviderSlot::GeminiOAuth => "gemini-oauth",
            ProviderSlot::OpenAI => "openai",
            ProviderSlot::Anthropic => "anthropic",
            ProviderSlot::Ollama => "ollama",
            ProviderSlot::ClaudeCode => "claude-code",
            ProviderSlot::Fireworks => "fireworks",
            ProviderSlot::Zai => "zai",
            ProviderSlot::OpenAiCompat => "openai-compat",
            ProviderSlot::Xiaomi => "xiaomi",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProviderSlot::GeminiApiKey => "Gemini (API key)",
            ProviderSlot::GeminiOAuth => "Gemini (Google OAuth)",
            ProviderSlot::OpenAI => "OpenAI",
            ProviderSlot::Anthropic => "Anthropic",
            ProviderSlot::Ollama => "Ollama (local)",
            ProviderSlot::ClaudeCode => "Claude Code (subscription)",
            ProviderSlot::Fireworks => "Fireworks AI Firepass",
            ProviderSlot::Zai => "Z.ai (GLM)",
            ProviderSlot::OpenAiCompat => "OpenAI-compatible (local)",
            ProviderSlot::Xiaomi => "Xiaomi MiMo",
        }
    }

    /// Parse a persisted slot key. Accepts the explicit slot keys as well as
    /// the legacy `Provider::key` strings — `"gemini"` from a pre-OAuth-slot
    /// settings file maps to `GeminiApiKey`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "gemini-apikey" | "gemini_api_key" | "gemini-key" => Some(Self::GeminiApiKey),
            "gemini-oauth" | "gemini_oauth" | "gemini-google" => Some(Self::GeminiOAuth),
            "gemini" | "google" => Some(Self::GeminiApiKey),
            "openai" | "gpt" | "chatgpt" => Some(Self::OpenAI),
            "anthropic" | "claude" => Some(Self::Anthropic),
            "ollama" | "local" => Some(Self::Ollama),
            "claude-code" | "claude_code" | "claudecode" | "cc" => Some(Self::ClaudeCode),
            "fireworks" | "fireworks-ai" | "firepass" | "kimi" => Some(Self::Fireworks),
            "zai" | "z-ai" | "z.ai" | "zhipu" | "glm" => Some(Self::Zai),
            "xiaomi" | "mimo" | "xiaomimimo" | "xiaomi-mimo" | "xiaomi_mimo" => Some(Self::Xiaomi),
            "openai-compat" | "openai-compatible" | "openai_compat" | "local-openai"
            | "lmstudio" | "lm-studio" | "llamacpp" | "llama-cpp" | "llama.cpp" => {
                Some(Self::OpenAiCompat)
            }
            _ => None,
        }
    }

    /// The wire-level provider this slot speaks to. Both Gemini slots collapse
    /// to [`Provider::Gemini`]; `mogen-llm` doesn't model the auth split — the
    /// Studio's `resolve_credential` picks API-key vs OAuth from the slot.
    pub fn to_provider(self) -> Provider {
        match self {
            ProviderSlot::GeminiApiKey | ProviderSlot::GeminiOAuth => Provider::Gemini,
            ProviderSlot::OpenAI => Provider::OpenAI,
            ProviderSlot::Anthropic => Provider::Anthropic,
            ProviderSlot::Ollama => Provider::Ollama,
            ProviderSlot::ClaudeCode => Provider::ClaudeCode,
            ProviderSlot::Fireworks => Provider::Fireworks,
            ProviderSlot::Zai => Provider::Zai,
            ProviderSlot::OpenAiCompat => Provider::OpenAiCompat,
            ProviderSlot::Xiaomi => Provider::Xiaomi,
        }
    }

    /// True when this slot demands a Google OAuth bundle and must not fall
    /// back to API-key auth even when a key is configured.
    pub fn is_gemini_oauth(self) -> bool {
        matches!(self, ProviderSlot::GeminiOAuth)
    }
}

impl Default for ProviderSlot {
    fn default() -> Self {
        ProviderSlot::GeminiApiKey
    }
}

/// Order in which provider slots appear in the Options dropdown. Both
/// Gemini auth modes are listed up front because Gemini is the historical
/// default; the OAuth slot is the path users with paid Antigravity plans
/// will reach for.
pub const PROVIDER_SLOTS: [ProviderSlot; 10] = [
    ProviderSlot::GeminiApiKey,
    ProviderSlot::GeminiOAuth,
    ProviderSlot::OpenAI,
    ProviderSlot::Anthropic,
    ProviderSlot::Ollama,
    ProviderSlot::OpenAiCompat,
    ProviderSlot::ClaudeCode,
    ProviderSlot::Fireworks,
    ProviderSlot::Zai,
    ProviderSlot::Xiaomi,
];

/// Default thinking model when the active slot is `GeminiOAuth` and no
/// override is set. The public-API `gemini-pro-latest` alias 404s on
/// `cloudcode-pa.googleapis.com/v1internal` — pick a concrete preview tag
/// known to work on the Antigravity OAuth surface.
pub const DEFAULT_OAUTH_GEMINI_MODEL: &str = "gemini-3.1-pro-preview";

/// Default fast model for the `GeminiOAuth` slot. Matches the cloudcode-pa
/// surface; `gemini-flash-latest` is a public-API alias and 404s on OAuth.
pub const DEFAULT_OAUTH_GEMINI_FAST_MODEL: &str = "gemini-3-flash-preview";
