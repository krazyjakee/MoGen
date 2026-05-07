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
use crate::fireworks::{FireworksClient, FireworksError};
use crate::gemini::{GeminiClient, GeminiError};
use crate::google_oauth::OAuthBundle;
use crate::ollama::{OllamaClient, OllamaError};
use crate::openai::{OpenAIClient, OpenAIError};
use crate::types::{GenerateConfig, GenerateResponse};
use crate::zai_chat::{ZaiChatClient, ZaiChatError};

/// How the user's Google credential is supplied. Surfaced in the CLI's
/// credential resolver — flag/env API-key beats stored OAuth, so users can
/// always force the public-API path even when an OAuth token is on disk.
///
/// Two OAuth variants exist because Google gates the image-generation
/// surface (`:streamGenerateContent` for nano-banana / Gemini 3 Pro Image)
/// behind the **Antigravity** OAuth client; the gemini-cli OAuth client
/// gets a 403 there. Text gen accepts either OAuth client.
#[derive(Debug, Clone)]
pub enum GoogleCredential {
    ApiKey(String),
    /// Bundle issued by the gemini-cli OAuth client (`mogen auth login`).
    /// Drives text generation against `cloudcode-pa.googleapis.com/v1internal`.
    OAuth(OAuthBundle),
    /// Bundle issued by the Antigravity OAuth client
    /// (`mogen auth login --antigravity`). Required for image generation;
    /// also works for text gen.
    AntigravityOAuth(OAuthBundle),
}

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
    /// Fireworks AI's OpenAI-compatible Chat Completions surface. Default
    /// model is the Fire Pass `kimi-k2p6` router which bills the Kimi K2
    /// family at zero per-token cost for personal agentic-coding use.
    Fireworks,
    /// Z.ai (Zhipu AI) GLM family. OpenAI-compatible Chat Completions at
    /// `api.z.ai/api/paas/v4/chat/completions`. Default model is `glm-5.1`.
    Zai,
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
            Provider::Fireworks => "fireworks",
            Provider::Zai => "zai",
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
            Provider::Fireworks => "Fireworks AI Firepass",
            Provider::Zai => "Z.ai (GLM)",
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
            Provider::Fireworks => "Fireworks AI Firepass",
            Provider::Zai => "Z.ai",
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
            "fireworks" | "fireworks-ai" | "firepass" | "kimi" => Some(Self::Fireworks),
            "zai" | "z-ai" | "z.ai" | "zhipu" | "glm" => Some(Self::Zai),
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
            Provider::Fireworks => "FIREWORKS_API_KEY",
            Provider::Zai => "ZAI_API_KEY",
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
            Provider::Fireworks => crate::fireworks::DEFAULT_MODEL,
            Provider::Zai => crate::zai_chat::DEFAULT_MODEL,
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
            Provider::Fireworks => crate::fireworks::DEFAULT_FAST_MODEL,
            Provider::Zai => crate::zai_chat::DEFAULT_FAST_MODEL,
        }
    }

    /// True when the provider has no concept of a paste-in API key (auth is
    /// either absent or managed externally). Drives the Studio onboarding
    /// flow — these providers don't need a key field on the welcome screen.
    pub fn is_keyless(self) -> bool {
        matches!(self, Provider::Ollama | Provider::ClaudeCode)
    }

    /// Whether this provider supports vision **input** — i.e. the user can
    /// attach a reference image alongside the text prompt and the model
    /// will read it. Drives the Studio's image-to-3D Generate flow and
    /// gates the auto-refine button (which feeds rendered scene PNGs
    /// back to the model). Today: Gemini and Z.ai (`glm-5v-turbo`).
    ///
    /// This is **not** about image *generation* (the textures pipeline) —
    /// that lives behind the `ImageProvider` enum in the Studio settings.
    pub fn supports_images(self) -> bool {
        matches!(self, Provider::Gemini | Provider::Zai)
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
///
/// Network failures are split into [`Self::Offline`] / [`Self::Timeout`] /
/// [`Self::Tls`] / [`Self::Transport`] so the UI can show "you appear to be
/// offline" instead of a raw reqwest dump. The split is heuristic — see
/// [`classify_reqwest`].
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("missing {var} (set the environment variable or pass --api-key)")]
    MissingApiKey { var: &'static str },
    /// Could not reach the provider at all — DNS failure, connection refused,
    /// network unreachable. Most often "no internet".
    #[error("offline: {0}")]
    Offline(String),
    /// Connect or read timed out. The server may be slow, blocked by a captive
    /// portal, or routing the connection into a black hole.
    #[error("timeout: {0}")]
    Timeout(String),
    /// TLS handshake or certificate validation failed. Usually a clock skew
    /// problem, an MITM proxy, or a corporate firewall.
    #[error("TLS error: {0}")]
    Tls(String),
    /// Other transport-layer failures (response decode, body stream, builder).
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
    /// OAuth subsystem failure (refresh failed, project missing, store
    /// corrupted). The body carries the [`crate::google_oauth::OAuthError`]
    /// message verbatim so the CLI can show the actionable hint.
    #[error("OAuth error: {0}")]
    OAuth(String),
}

impl From<GeminiError> for ProviderError {
    fn from(e: GeminiError) -> Self {
        match e {
            GeminiError::MissingApiKey => Self::MissingApiKey { var: "GEMINI_API_KEY" },
            GeminiError::Transport(err) => classify_reqwest(&err),
            GeminiError::Api { status, message } => Self::Api { status, message },
            GeminiError::EmptyResponse => Self::EmptyResponse,
            GeminiError::BudgetExceeded { used, budget } => Self::BudgetExceeded { used, budget },
            GeminiError::InvalidResponse(s) => Self::InvalidResponse(s),
            GeminiError::OAuth(s) => Self::OAuth(s),
            // The cache-unavailable case is an *expected* signal in OAuth
            // mode: the resolver swallows it and falls back to inline.
            // Surface it as InvalidResponse so a stray bubble-up still
            // reads sensibly in user logs.
            GeminiError::CacheUnavailableOverOAuth => Self::Unsupported {
                provider: Provider::Gemini,
                feature: "cachedContents over OAuth",
            },
        }
    }
}

impl From<OpenAIError> for ProviderError {
    fn from(e: OpenAIError) -> Self {
        match e {
            OpenAIError::MissingApiKey => Self::MissingApiKey { var: "OPENAI_API_KEY" },
            OpenAIError::Transport(err) => classify_reqwest(&err),
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
            AnthropicError::Transport(err) => classify_reqwest(&err),
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
            OllamaError::Transport(err) => classify_reqwest(&err),
            OllamaError::Api { status, message } => Self::Api { status, message },
            OllamaError::EmptyResponse => Self::EmptyResponse,
            OllamaError::BudgetExceeded { used, budget } => Self::BudgetExceeded { used, budget },
            OllamaError::InvalidResponse(s) => Self::InvalidResponse(s),
        }
    }
}

impl From<FireworksError> for ProviderError {
    fn from(e: FireworksError) -> Self {
        match e {
            FireworksError::MissingApiKey => Self::MissingApiKey { var: "FIREWORKS_API_KEY" },
            FireworksError::Transport(err) => classify_reqwest(&err),
            FireworksError::Api { status, message } => Self::Api { status, message },
            FireworksError::EmptyResponse => Self::EmptyResponse,
            FireworksError::BudgetExceeded { used, budget } => Self::BudgetExceeded { used, budget },
            FireworksError::InvalidResponse(s) => Self::InvalidResponse(s),
        }
    }
}

impl From<ZaiChatError> for ProviderError {
    fn from(e: ZaiChatError) -> Self {
        match e {
            ZaiChatError::MissingApiKey => Self::MissingApiKey { var: "ZAI_API_KEY" },
            ZaiChatError::Transport(err) => classify_reqwest(&err),
            ZaiChatError::Api { status, message } => Self::Api { status, message },
            ZaiChatError::EmptyResponse => Self::EmptyResponse,
            ZaiChatError::BudgetExceeded { used, budget } => Self::BudgetExceeded { used, budget },
            ZaiChatError::InvalidResponse(s) => Self::InvalidResponse(s),
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

/// Bucket a `reqwest::Error` into the appropriate `ProviderError` network
/// variant. The classification is heuristic: `is_timeout` / `is_connect` are
/// authoritative when set, but reqwest collapses a lot of conditions into
/// `is_request`, so we also pattern-match the formatted source chain.
///
/// The buckets exist so the UI can show a short, specific message
/// ("You appear to be offline" vs. "TLS handshake failed") rather than a
/// generic "transport error". The detail string still carries the full chain
/// for callers that want it.
pub(crate) fn classify_reqwest(err: &reqwest::Error) -> ProviderError {
    let detail = format_reqwest(err);
    let lower = detail.to_ascii_lowercase();

    if err.is_timeout() || lower.contains("timed out") || lower.contains("operation timed out") {
        return ProviderError::Timeout(detail);
    }

    // TLS markers come from rustls / native-tls / openssl error strings. Check
    // before the connect bucket — a TLS handshake failure can be reported as
    // either is_connect or is_request depending on where it fired.
    let tls = lower.contains("tls")
        || lower.contains("ssl")
        || lower.contains("certificate")
        || lower.contains("handshake")
        || lower.contains("trust anchor")
        || lower.contains("self-signed")
        || lower.contains("self signed")
        || lower.contains("unknownissuer")
        || lower.contains("invalid peer certificate");
    if tls {
        return ProviderError::Tls(detail);
    }

    let offline = err.is_connect()
        || lower.contains("dns error")
        || lower.contains("failed to lookup address")
        || lower.contains("name or service not known")
        || lower.contains("nodename nor servname")
        || lower.contains("no address associated")
        || lower.contains("temporary failure in name resolution")
        || lower.contains("network is unreachable")
        || lower.contains("network is down")
        || lower.contains("host is unreachable")
        || lower.contains("no route to host")
        || lower.contains("connection refused")
        || lower.contains("connection reset");
    if offline {
        return ProviderError::Offline(detail);
    }

    ProviderError::Transport(detail)
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
    Fireworks(FireworksClient),
    Zai(ZaiChatClient),
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
            Provider::Fireworks => LlmClient::Fireworks(FireworksClient::new(api_key)),
            Provider::Zai => LlmClient::Zai(ZaiChatClient::new(api_key)),
        }
    }

    /// Construct a Gemini client from a resolved [`GoogleCredential`]. The
    /// `mogen` CLI resolver picks the right variant from
    /// flag → env → on-disk OAuth before calling this. API-key callers
    /// continue to use [`Self::new`] / [`Self::from_env`].
    pub fn gemini_from_credential(credential: GoogleCredential) -> Self {
        match credential {
            GoogleCredential::ApiKey(key) => LlmClient::Gemini(GeminiClient::new(key)),
            GoogleCredential::OAuth(bundle) => {
                LlmClient::Gemini(GeminiClient::from_oauth(bundle))
            }
            GoogleCredential::AntigravityOAuth(bundle) => {
                LlmClient::Gemini(GeminiClient::from_antigravity_oauth(bundle))
            }
        }
    }

    /// Construct a client for `provider`, resolving the API key in
    /// precedence order: env var ([`Provider::env_var`]) →
    /// `~/.mogen/settings.json` (shared with Studio) → error. Keyless
    /// providers (Ollama, ClaudeCode) always succeed with a blank key.
    pub fn from_env(provider: Provider) -> Result<Self, ProviderError> {
        if provider.is_keyless() {
            return Ok(Self::new(provider, String::new()));
        }
        let var = provider.env_var();
        let env_key = std::env::var(var).unwrap_or_default();
        if !env_key.trim().is_empty() {
            return Ok(Self::new(provider, env_key));
        }
        if let Some(file_key) = crate::settings_store::read_api_key(provider) {
            return Ok(Self::new(provider, file_key));
        }
        Err(ProviderError::MissingApiKey { var })
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
            Provider::Fireworks => {
                LlmClient::Fireworks(FireworksClient::with_base_url(api_key, base_url))
            }
            Provider::Zai => LlmClient::Zai(ZaiChatClient::with_base_url(api_key, base_url)),
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
            LlmClient::Fireworks(_) => Provider::Fireworks,
            LlmClient::Zai(_) => Provider::Zai,
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
            LlmClient::Fireworks(c) => c.generate(cfg).map_err(Into::into),
            LlmClient::Zai(c) => c.generate(cfg).map_err(Into::into),
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
            Provider::Fireworks,
            Provider::Zai,
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
        assert_eq!(Provider::parse("FirePass"), Some(Provider::Fireworks));
        assert_eq!(Provider::parse("kimi"), Some(Provider::Fireworks));
        assert_eq!(Provider::parse("Z.AI"), Some(Provider::Zai));
        assert_eq!(Provider::parse("zhipu"), Some(Provider::Zai));
        assert_eq!(Provider::parse("glm"), Some(Provider::Zai));
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
    fn classify_reqwest_buckets_closed_port_as_offline() {
        // Hitting a closed local port avoids DNS / network access entirely
        // and reliably produces a connect error on every dev machine.
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(500))
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .unwrap();
        let err = http.get("http://127.0.0.1:1/").send().unwrap_err();
        match classify_reqwest(&err) {
            ProviderError::Offline(_) => {}
            ProviderError::Timeout(_) => {} // some platforms return ETIMEDOUT here
            other => panic!("expected Offline/Timeout, got {other:?}"),
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
