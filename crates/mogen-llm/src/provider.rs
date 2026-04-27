//! Provider selector + unified [`LlmClient`] used by every higher-level entry
//! point in this crate (the repair loop, the textures pipeline's text calls,
//! and the Studio Q&A modal).
//!
//! Adding a new provider means: drop a new module under `src/`, define a
//! client struct that implements `generate(&GenerateConfig) ->
//! Result<GenerateResponse, ProviderError>`, then wire it into [`Provider`]
//! and [`LlmClient`] here.

use std::fmt;

use crate::anthropic::{AnthropicClient, AnthropicError};
use crate::claude_code::{ClaudeCodeClient, ClaudeCodeError};
use crate::gemini::{GeminiClient, GeminiError};
use crate::ollama::{OllamaClient, OllamaError};
use crate::openai::{OpenAIClient, OpenAIError};
use crate::types::{GenerateConfig, GenerateResponse};

/// Closed set of LLM backends the CLI / Studio can talk to. Persisted as a
/// lowercase label (see [`Provider::key`]) so adding new variants doesn't
/// invalidate old settings files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Gemini,
    OpenAI,
    Anthropic,
    Ollama,
    /// Local Claude Code CLI (`claude -p`). Authenticates through the user's
    /// installed Claude Code login (Pro/Max subscription or API key managed
    /// by `claude`), so no API key is collected here.
    ClaudeCode,
}

impl Provider {
    /// Round-trip key. `gemini` is preserved as the legacy/default value so
    /// old settings files keep working when the field is absent.
    pub fn key(self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::OpenAI => "openai",
            Provider::Anthropic => "anthropic",
            Provider::Ollama => "ollama",
            Provider::ClaudeCode => "claude-code",
        }
    }

    /// Human-friendly label for UI dropdowns.
    pub fn label(self) -> &'static str {
        match self {
            Provider::Gemini => "Gemini",
            Provider::OpenAI => "OpenAI",
            Provider::Anthropic => "Anthropic",
            Provider::Ollama => "Ollama (local)",
            Provider::ClaudeCode => "Claude Code (subscription)",
        }
    }

    /// Short brand name for inline status text ("waiting for X…",
    /// "re-calling X"). Drops the parenthetical qualifier from
    /// [`Self::label`] so progress strings stay tight.
    pub fn display_name(self) -> &'static str {
        match self {
            Provider::Gemini => "Gemini",
            Provider::OpenAI => "OpenAI",
            Provider::Anthropic => "Anthropic",
            Provider::Ollama => "Ollama",
            Provider::ClaudeCode => "Claude Code",
        }
    }

    /// Parse a case-insensitive label / key. Accepts the canonical keys as
    /// well as common spellings the user might pass on the CLI
    /// (`google`, `gpt`, `claude`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "gemini" | "google" => Some(Self::Gemini),
            "openai" | "gpt" | "chatgpt" => Some(Self::OpenAI),
            "anthropic" | "claude" => Some(Self::Anthropic),
            "ollama" | "local" => Some(Self::Ollama),
            "claude-code" | "claude_code" | "claudecode" | "cc" => Some(Self::ClaudeCode),
            _ => None,
        }
    }

    /// Environment variable consulted for the API key by [`resolve_api_key`].
    /// Ollama runs locally and has no key by default — `OLLAMA_API_KEY` is
    /// still consulted for users who put a reverse proxy in front of it.
    /// Claude Code authenticates through the user's `claude` login, so
    /// the env var is unused (returns an empty string).
    pub fn env_var(self) -> &'static str {
        match self {
            Provider::Gemini => "GEMINI_API_KEY",
            Provider::OpenAI => "OPENAI_API_KEY",
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::Ollama => "OLLAMA_API_KEY",
            Provider::ClaudeCode => "",
        }
    }

    /// Default heavy / "thinking" model id for this provider. Surfaced as
    /// the CLI default when `--model` is omitted.
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Gemini => crate::gemini::DEFAULT_MODEL,
            Provider::OpenAI => crate::openai::DEFAULT_MODEL,
            Provider::Anthropic => crate::anthropic::DEFAULT_MODEL,
            Provider::Ollama => crate::ollama::DEFAULT_MODEL,
            Provider::ClaudeCode => crate::claude_code::DEFAULT_MODEL,
        }
    }

    /// Default fast / cheap model id (used by the Studio Prompt Enhancer
    /// and Ask modal).
    pub fn default_fast_model(self) -> &'static str {
        match self {
            Provider::Gemini => crate::gemini::DEFAULT_FAST_MODEL,
            Provider::OpenAI => crate::openai::DEFAULT_FAST_MODEL,
            Provider::Anthropic => crate::anthropic::DEFAULT_FAST_MODEL,
            Provider::Ollama => crate::ollama::DEFAULT_MODEL,
            Provider::ClaudeCode => crate::claude_code::DEFAULT_FAST_MODEL,
        }
    }

    /// True when the provider has no concept of a paste-in API key (auth is
    /// either absent or managed externally). Drives the Studio onboarding
    /// flow — these providers don't need a key field on the welcome screen.
    pub fn is_keyless(self) -> bool {
        matches!(self, Provider::Ollama | Provider::ClaudeCode)
    }

    /// Whether this provider supports image generation (used by the textures
    /// pipeline). Today only Gemini does — non-Gemini providers fall back to
    /// a clear error in that command.
    pub fn supports_images(self) -> bool {
        matches!(self, Provider::Gemini)
    }

    /// Whether this provider supports the persistent `cachedContents`-style
    /// system-instruction cache. Only Gemini today.
    pub fn supports_cached_content(self) -> bool {
        matches!(self, Provider::Gemini)
    }
}

impl Default for Provider {
    fn default() -> Self {
        Provider::Gemini
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Provider-agnostic error type emitted by [`LlmClient::generate`]. Each
/// per-provider error variant is mapped into one of these on the way out so
/// callers (the repair loop, the Studio classifier) don't need to match on
/// four error enums.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("missing {var} (set the environment variable or pass --api-key)")]
    MissingApiKey { var: &'static str },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("empty response: no candidates or text parts")]
    EmptyResponse,
    #[error("budget exceeded: {used} input+output tokens exceeds --budget-tokens={budget}")]
    BudgetExceeded { used: u32, budget: u32 },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("provider {provider} does not support {feature}")]
    Unsupported { provider: Provider, feature: &'static str },
}

impl From<GeminiError> for ProviderError {
    fn from(e: GeminiError) -> Self {
        match e {
            GeminiError::MissingApiKey => Self::MissingApiKey { var: "GEMINI_API_KEY" },
            GeminiError::Transport(err) => Self::Transport(format_reqwest(&err)),
            GeminiError::Api { status, message } => Self::Api { status, message },
            GeminiError::EmptyResponse => Self::EmptyResponse,
            GeminiError::BudgetExceeded { used, budget } => Self::BudgetExceeded { used, budget },
            GeminiError::InvalidResponse(s) => Self::InvalidResponse(s),
        }
    }
}

impl From<OpenAIError> for ProviderError {
    fn from(e: OpenAIError) -> Self {
        match e {
            OpenAIError::MissingApiKey => Self::MissingApiKey { var: "OPENAI_API_KEY" },
            OpenAIError::Transport(err) => Self::Transport(format_reqwest(&err)),
            OpenAIError::Api { status, message } => Self::Api { status, message },
            OpenAIError::EmptyResponse => Self::EmptyResponse,
            OpenAIError::BudgetExceeded { used, budget } => Self::BudgetExceeded { used, budget },
            OpenAIError::InvalidResponse(s) => Self::InvalidResponse(s),
        }
    }
}

impl From<AnthropicError> for ProviderError {
    fn from(e: AnthropicError) -> Self {
        match e {
            AnthropicError::MissingApiKey => Self::MissingApiKey { var: "ANTHROPIC_API_KEY" },
            AnthropicError::Transport(err) => Self::Transport(format_reqwest(&err)),
            AnthropicError::Api { status, message } => Self::Api { status, message },
            AnthropicError::EmptyResponse => Self::EmptyResponse,
            AnthropicError::BudgetExceeded { used, budget } => Self::BudgetExceeded { used, budget },
            AnthropicError::InvalidResponse(s) => Self::InvalidResponse(s),
        }
    }
}

impl From<OllamaError> for ProviderError {
    fn from(e: OllamaError) -> Self {
        match e {
            OllamaError::Transport(err) => Self::Transport(format_reqwest(&err)),
            OllamaError::Api { status, message } => Self::Api { status, message },
            OllamaError::EmptyResponse => Self::EmptyResponse,
            OllamaError::BudgetExceeded { used, budget } => Self::BudgetExceeded { used, budget },
            OllamaError::InvalidResponse(s) => Self::InvalidResponse(s),
        }
    }
}

impl From<ClaudeCodeError> for ProviderError {
    fn from(e: ClaudeCodeError) -> Self {
        match e {
            ClaudeCodeError::SpawnFailed { path, source } => {
                Self::Transport(format!("failed to spawn `{path}`: {source}"))
            }
            ClaudeCodeError::Io(err) => Self::Transport(err.to_string()),
            ClaudeCodeError::NonZeroExit { code, message } => Self::Api {
                status: code as u16,
                message,
            },
            ClaudeCodeError::EmptyResponse => Self::EmptyResponse,
            ClaudeCodeError::BudgetExceeded { used, budget } => {
                Self::BudgetExceeded { used, budget }
            }
            ClaudeCodeError::InvalidResponse(s) => Self::InvalidResponse(s),
        }
    }
}

fn format_reqwest(err: &reqwest::Error) -> String {
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

/// Unified handle to any backend. Constructed via [`LlmClient::new`] or one of
/// the per-provider `from_…` constructors.
///
/// Cloning is cheap (the underlying reqwest client uses an Arc-pool inside).
pub enum LlmClient {
    Gemini(GeminiClient),
    OpenAI(OpenAIClient),
    Anthropic(AnthropicClient),
    Ollama(OllamaClient),
    ClaudeCode(ClaudeCodeClient),
}

impl LlmClient {
    /// Construct a client for `provider`. `api_key` is required for the cloud
    /// providers; pass an empty string for Ollama (keyless local) and Claude
    /// Code (auth is delegated to the user's `claude` CLI install).
    pub fn new(provider: Provider, api_key: impl Into<String>) -> Self {
        let api_key = api_key.into();
        match provider {
            Provider::Gemini => LlmClient::Gemini(GeminiClient::new(api_key)),
            Provider::OpenAI => LlmClient::OpenAI(OpenAIClient::new(api_key)),
            Provider::Anthropic => LlmClient::Anthropic(AnthropicClient::new(api_key)),
            Provider::Ollama => LlmClient::Ollama(OllamaClient::new(api_key)),
            Provider::ClaudeCode => LlmClient::ClaudeCode(ClaudeCodeClient::new()),
        }
    }

    /// Construct a client for `provider`, reading the API key from the
    /// matching environment variable ([`Provider::env_var`]).
    pub fn from_env(provider: Provider) -> Result<Self, ProviderError> {
        if provider.is_keyless() {
            return Ok(Self::new(provider, String::new()));
        }
        let var = provider.env_var();
        let key = std::env::var(var).unwrap_or_default();
        if key.trim().is_empty() {
            return Err(ProviderError::MissingApiKey { var });
        }
        Ok(Self::new(provider, key))
    }

    /// Override the base URL — used by tests to point at a `tiny_http` mock
    /// server. For Ollama this also lets users point at a non-default host.
    /// For Claude Code, `base_url` is reinterpreted as the path to the
    /// `claude` binary (allowing tests to point at a stub script).
    pub fn with_base_url(
        provider: Provider,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let api_key = api_key.into();
        let base_url = base_url.into();
        match provider {
            Provider::Gemini => LlmClient::Gemini(GeminiClient::with_base_url(api_key, base_url)),
            Provider::OpenAI => LlmClient::OpenAI(OpenAIClient::with_base_url(api_key, base_url)),
            Provider::Anthropic => {
                LlmClient::Anthropic(AnthropicClient::with_base_url(api_key, base_url))
            }
            Provider::Ollama => LlmClient::Ollama(OllamaClient::with_base_url(api_key, base_url)),
            Provider::ClaudeCode => LlmClient::ClaudeCode(ClaudeCodeClient::with_path(base_url)),
        }
    }

    /// Which backend this client talks to.
    pub fn provider(&self) -> Provider {
        match self {
            LlmClient::Gemini(_) => Provider::Gemini,
            LlmClient::OpenAI(_) => Provider::OpenAI,
            LlmClient::Anthropic(_) => Provider::Anthropic,
            LlmClient::Ollama(_) => Provider::Ollama,
            LlmClient::ClaudeCode(_) => Provider::ClaudeCode,
        }
    }

    /// Borrow the underlying [`GeminiClient`] for image generation and the
    /// `cachedContents` cache. Returns `None` for non-Gemini providers — the
    /// caller decides whether to fall back or surface
    /// [`ProviderError::Unsupported`].
    pub fn as_gemini(&self) -> Option<&GeminiClient> {
        match self {
            LlmClient::Gemini(c) => Some(c),
            _ => None,
        }
    }

    /// Borrow the underlying Claude Code client to set a non-default binary
    /// path. Returns `None` for other providers.
    pub fn as_claude_code_mut(&mut self) -> Option<&mut ClaudeCodeClient> {
        match self {
            LlmClient::ClaudeCode(c) => Some(c),
            _ => None,
        }
    }

    /// Issue a single text-completion call. Mapping is provider-specific;
    /// see each module for the wire shape.
    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, ProviderError> {
        match self {
            LlmClient::Gemini(c) => c.generate(cfg).map_err(Into::into),
            LlmClient::OpenAI(c) => c.generate(cfg).map_err(Into::into),
            LlmClient::Anthropic(c) => c.generate(cfg).map_err(Into::into),
            LlmClient::Ollama(c) => c.generate(cfg).map_err(Into::into),
            LlmClient::ClaudeCode(c) => c.generate(cfg).map_err(Into::into),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_keys_round_trip() {
        for p in [
            Provider::Gemini,
            Provider::OpenAI,
            Provider::Anthropic,
            Provider::Ollama,
            Provider::ClaudeCode,
        ] {
            assert_eq!(Provider::parse(p.key()), Some(p));
        }
    }

    #[test]
    fn provider_parse_accepts_aliases() {
        assert_eq!(Provider::parse("Google"), Some(Provider::Gemini));
        assert_eq!(Provider::parse("gpt"), Some(Provider::OpenAI));
        assert_eq!(Provider::parse("CLAUDE"), Some(Provider::Anthropic));
        assert_eq!(Provider::parse("local"), Some(Provider::Ollama));
        assert_eq!(Provider::parse("CLAUDE_CODE"), Some(Provider::ClaudeCode));
        assert_eq!(Provider::parse("cc"), Some(Provider::ClaudeCode));
        assert_eq!(Provider::parse("wat"), None);
    }

    #[test]
    fn claude_code_is_keyless_in_from_env() {
        // Should construct without consulting any env var.
        let c = LlmClient::from_env(Provider::ClaudeCode)
            .unwrap_or_else(|_| panic!("claude-code should be keyless"));
        assert_eq!(c.provider(), Provider::ClaudeCode);
    }

    #[test]
    fn from_env_returns_missing_key_for_blank_cloud_provider() {
        // SAFETY: tests are single-threaded per file by default in cargo, but
        // these env mutations could race under `--test-threads`. Using
        // unique variable names per provider would be ideal; here we just
        // unset and assert the error path.
        std::env::remove_var("OPENAI_API_KEY");
        match LlmClient::from_env(Provider::OpenAI) {
            Err(ProviderError::MissingApiKey { var: "OPENAI_API_KEY" }) => {}
            Err(other) => panic!("wrong error: {other}"),
            Ok(_) => panic!("expected MissingApiKey but got Ok"),
        }
    }

    #[test]
    fn from_env_succeeds_for_ollama_without_key() {
        // Ollama tolerates a blank key; this should construct a client.
        std::env::remove_var("OLLAMA_API_KEY");
        let c = LlmClient::from_env(Provider::Ollama).unwrap_or_else(|_| {
            panic!("ollama should work keyless")
        });
        assert_eq!(c.provider(), Provider::Ollama);
    }
}
