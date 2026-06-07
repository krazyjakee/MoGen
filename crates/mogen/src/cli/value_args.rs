//! CLI-facing value enums that mirror types from [`mogen_llm`] and
//! [`crate::commands::build`]. Kept separate from the library types so we
//! don't leak [`clap::ValueEnum`] derives into the library crates, and
//! kept separate from the subcommand definitions in [`super::cmd`] so
//! every clap-derived flag enum lives in one place.

use clap::ValueEnum;

use mogen_llm::{Provider, Style, ThinkingLevel};

use crate::commands;

/// CLI-facing mirror of [`ThinkingLevel`]. Kept separate so we don't leak
/// `clap::ValueEnum` into the `mogen-llm` library crate.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ThinkingArg {
    Low,
    Medium,
    High,
    Xhigh,
}

impl From<ThinkingArg> for ThinkingLevel {
    fn from(a: ThinkingArg) -> Self {
        match a {
            ThinkingArg::Low => ThinkingLevel::Low,
            ThinkingArg::Medium => ThinkingLevel::Medium,
            ThinkingArg::High => ThinkingLevel::High,
            ThinkingArg::Xhigh => ThinkingLevel::XHigh,
        }
    }
}

/// CLI-facing mirror of [`Style`]. Same separation as `ThinkingArg` — keeps
/// `clap::ValueEnum` out of `mogen-llm`. Variants render as kebab-case
/// slugs by clap's default rename (e.g. `LowPoly` → `low-poly`); the
/// `From` impl maps each variant to the snake-case [`Style::key`] used
/// inside `meta(style=…)`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum StyleArg {
    Ps1,
    N64,
    LowPoly,
    HighDetail,
    Arcade,
    Voxel,
    CelShaded,
    StylizedFantasy,
    Cyberpunk,
    PixelArt,
}

impl From<StyleArg> for Style {
    fn from(a: StyleArg) -> Self {
        match a {
            StyleArg::Ps1 => Style::Ps1,
            StyleArg::N64 => Style::N64,
            StyleArg::LowPoly => Style::LowPoly,
            StyleArg::HighDetail => Style::HighDetail,
            StyleArg::Arcade => Style::Arcade,
            StyleArg::Voxel => Style::Voxel,
            StyleArg::CelShaded => Style::CelShaded,
            StyleArg::StylizedFantasy => Style::StylizedFantasy,
            StyleArg::Cyberpunk => Style::Cyberpunk,
            StyleArg::PixelArt => Style::PixelArt,
        }
    }
}

/// CLI-facing mirror of [`Provider`] **plus** the Gemini auth-mode flag.
/// The four Gemini-flavored variants (`Auto`, `Gemini`, `GeminiOauth`,
/// `Antigravity`) all map to [`Provider::Gemini`] and disambiguate the
/// credential path via [`GeminiAuthMode`]; non-Gemini variants ignore the
/// auth mode entirely (their credential path is API-key-only).
///
/// Folding auth into `--provider` keeps the user-facing surface small —
/// `mogen generate "…" --provider antigravity` is one flag, not two —
/// at the cost of one extra layer of indirection internally.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ProviderArg {
    /// Auto-detect Gemini credentials: flag → env → settings → gemini-cli
    /// OAuth → Antigravity OAuth. Default. Recommended for most users.
    Auto,
    /// Gemini via API key only (`GEMINI_API_KEY` env or settings.json).
    /// Skips OAuth entirely; errors if no key is available.
    Gemini,
    /// Gemini via the gemini-cli OAuth bundle written by `mogen auth login`.
    /// Errors if the bundle is missing or unreadable.
    GeminiOauth,
    /// Gemini via the Antigravity OAuth bundle written by
    /// `mogen auth login --antigravity`. Required for image generation;
    /// also valid for text gen and survives gemini-cli 403s.
    Antigravity,
    Openai,
    Anthropic,
    Ollama,
    /// Local `claude` CLI (Claude Code subscription). Auth is handled by
    /// the user's `claude /login`; no API key flag is required.
    ClaudeCode,
    /// Fireworks AI's OpenAI-compatible Chat Completions surface. Default
    /// model is the Fire Pass `kimi-k2p6` router; set `FIREWORKS_API_KEY`.
    Fireworks,
    /// Z.ai (Zhipu) GLM family via the OpenAI-compatible chat endpoint.
    /// Default model is `glm-5.1`; set `ZAI_API_KEY`.
    Zai,
    /// Xiaomi MiMo Open Platform via its OpenAI-compatible chat endpoint.
    /// Default model is `mimo-v2.5-pro`; set `XIAOMI_API_KEY`.
    Xiaomi,
}

impl From<ProviderArg> for Provider {
    fn from(p: ProviderArg) -> Self {
        match p {
            // Every Gemini auth flavor routes to the same library provider —
            // the difference is the credential, not the wire protocol.
            ProviderArg::Auto
            | ProviderArg::Gemini
            | ProviderArg::GeminiOauth
            | ProviderArg::Antigravity => Provider::Gemini,
            ProviderArg::Openai => Provider::OpenAI,
            ProviderArg::Anthropic => Provider::Anthropic,
            ProviderArg::Ollama => Provider::Ollama,
            ProviderArg::ClaudeCode => Provider::ClaudeCode,
            ProviderArg::Fireworks => Provider::Fireworks,
            ProviderArg::Zai => Provider::Zai,
            ProviderArg::Xiaomi => Provider::Xiaomi,
        }
    }
}

impl From<ProviderArg> for crate::common::GeminiAuthMode {
    fn from(p: ProviderArg) -> Self {
        use crate::common::GeminiAuthMode;
        match p {
            ProviderArg::Auto => GeminiAuthMode::Auto,
            ProviderArg::Gemini => GeminiAuthMode::ApiKey,
            ProviderArg::GeminiOauth => GeminiAuthMode::GeminiOauth,
            ProviderArg::Antigravity => GeminiAuthMode::Antigravity,
            // Non-Gemini providers ignore this; Auto is the harmless default
            // that `build_llm_client` accepts unconditionally for them.
            ProviderArg::Openai
            | ProviderArg::Anthropic
            | ProviderArg::Ollama
            | ProviderArg::ClaudeCode
            | ProviderArg::Fireworks
            | ProviderArg::Zai
            | ProviderArg::Xiaomi => GeminiAuthMode::Auto,
        }
    }
}

/// CLI-facing mirror of [`commands::build::BuildFormat`]. Kept here so
/// `clap::ValueEnum` doesn't leak into the command module.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum BuildFormatArg {
    /// Binary glTF 2.0 (default).
    Glb,
    /// Autodesk FBX 7.4 binary.
    Fbx,
}

impl From<BuildFormatArg> for commands::build::BuildFormat {
    fn from(f: BuildFormatArg) -> Self {
        match f {
            BuildFormatArg::Glb => commands::build::BuildFormat::Glb,
            BuildFormatArg::Fbx => commands::build::BuildFormat::Fbx,
        }
    }
}
