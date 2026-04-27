use std::fs;
use std::path::PathBuf;

use mogen_llm::gemini::{DEFAULT_FAST_MODEL, DEFAULT_MODEL, DEFAULT_TEMPERATURE};
use mogen_llm::{Provider, ThinkingLevel};
use serde::{Deserialize, Serialize};

use crate::preview_shader::{
    parse_preview_shader, preview_shader_key, PreviewShader, DEFAULT_PREVIEW_SHADER,
};
use crate::theme::{parse_theme, theme_key, Theme, DEFAULT_THEME};

/// Library default for the text-LLM repair budget. Matches
/// [`mogen_llm::RepairConfig::default`].
pub const DEFAULT_MAX_REPAIR_ITERS: u32 = 2;

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub gemini_api_key: String,
    /// Persisted as a lowercase label (`low` | `medium` | `high` | `xhigh`) so
    /// new `ThinkingLevel` variants can be added without a migration. Empty /
    /// unknown falls back to the library default at read time.
    #[serde(default)]
    pub thinking_level: String,
    /// Absolute path of the last `.mog` opened in the GUI. Used at startup to
    /// reopen the previous file. With [`Self::open_tabs`] populated this also
    /// names which tab to activate after the strip is restored.
    #[serde(default)]
    pub last_opened: Option<String>,
    /// Absolute paths of every titled tab open in the studio at last persist
    /// time, in tab-strip order. Untitled buffers are skipped (no path to key
    /// off). Empty after upgrade-from-old-settings, in which case startup
    /// falls back to opening just [`Self::last_opened`].
    #[serde(default)]
    pub open_tabs: Vec<String>,
    /// Most-recently-opened `.mog` files, newest first. Capped at
    /// [`Self::MAX_RECENT`] entries. Drives the File → Open Recent menu.
    #[serde(default)]
    pub recent_files: Vec<String>,
    /// Persisted as a lowercase label (see `theme_key`) so new `Theme` variants
    /// can be added without a migration. Empty / unknown falls back to
    /// `DEFAULT_THEME` at read time.
    #[serde(default)]
    pub theme: String,
    /// Persisted as a lowercase label (see `preview_shader_key`). Empty /
    /// unknown falls back to `DEFAULT_PREVIEW_SHADER`.
    #[serde(default)]
    pub preview_shader: String,

    /// "Thinking" model id used for the heavy text paths (generate, modify,
    /// animate and their repair loops) when the active provider is Gemini.
    /// Empty -> library default (`gemini-pro-latest`). Exposed in
    /// Options → Models.
    #[serde(default)]
    pub gemini_model: String,

    /// "Fast" model id used for low-stakes rewrites like the Prompt Enhancer
    /// when the active provider is Gemini. Empty -> [`DEFAULT_FAST_MODEL`]
    /// (`gemini-flash-latest`). Kept separate from `gemini_model` so users
    /// can pay Flash rates for prompt polish while still running Pro for the
    /// actual DSL generation.
    #[serde(default)]
    pub gemini_fast_model: String,

    /// OpenAI thinking-model override. Empty -> [`mogen_llm::openai::DEFAULT_MODEL`].
    #[serde(default)]
    pub openai_model: String,

    /// OpenAI fast-model override. Empty -> [`mogen_llm::openai::DEFAULT_FAST_MODEL`].
    #[serde(default)]
    pub openai_fast_model: String,

    /// Anthropic thinking-model override. Empty -> [`mogen_llm::anthropic::DEFAULT_MODEL`].
    #[serde(default)]
    pub anthropic_model: String,

    /// Anthropic fast-model override. Empty -> [`mogen_llm::anthropic::DEFAULT_FAST_MODEL`].
    #[serde(default)]
    pub anthropic_fast_model: String,

    /// Ollama thinking-model override. Empty -> [`mogen_llm::ollama::DEFAULT_MODEL`].
    /// Ollama only ships one default model id; the "fast" slot reuses the
    /// same string unless the user overrides it explicitly below.
    #[serde(default)]
    pub ollama_model: String,

    /// Ollama fast-model override. Empty -> falls back to [`Self::ollama_model`]
    /// (or the library default when both are empty).
    #[serde(default)]
    pub ollama_fast_model: String,

    /// Sampling temperature. `None` uses the library default (0.3).
    /// Serialised as f32 so downgrades to older binaries don't crash on
    /// missing fields — deserialises to `None` via `serde(default)`.
    #[serde(default)]
    pub gemini_temperature: Option<f32>,

    /// Max repair iterations. `None` uses the library default (2). A higher
    /// value lets the model self-correct longer at the cost of extra calls.
    #[serde(default)]
    pub max_repair_iters: Option<u32>,

    /// User-chosen deterministic seed. `None` → derive from the DSL header if
    /// present, else random per call (what the CLI does). Exposed so users
    /// can reproduce a prior generation when they saw one they liked.
    #[serde(default)]
    pub seed_override: Option<u64>,

    /// Set once the first-launch onboarding modal has been dismissed (whether
    /// the user pasted a key or skipped). False on a fresh install or after
    /// the settings file is wiped, which is the signal to show the welcome
    /// flow again.
    #[serde(default)]
    pub onboarded: bool,

    /// Selected LLM provider, persisted as a lowercase [`Provider::key`]
    /// (`"gemini"`, `"openai"`, `"anthropic"`, `"ollama"`). Empty / unknown
    /// falls back to [`Provider::default`] at read time so adding new
    /// providers later doesn't invalidate old settings files.
    #[serde(default)]
    pub provider: String,

    /// API key for the OpenAI provider. Stored alongside the Gemini key so
    /// switching providers in Options doesn't require re-pasting credentials.
    #[serde(default)]
    pub openai_api_key: String,

    /// API key for the Anthropic (Claude) provider.
    #[serde(default)]
    pub anthropic_api_key: String,

    /// Optional bearer token for an Ollama endpoint sitting behind an
    /// authenticating reverse proxy. Usually empty — local Ollama is keyless.
    #[serde(default)]
    pub ollama_api_key: String,

    /// Optional override for the Ollama base URL. Empty → library default
    /// (`http://localhost:11434`). Set this to point at a self-hosted
    /// instance.
    #[serde(default)]
    pub ollama_base_url: String,

    /// Optional override for the Claude Code binary path. Empty → resolve
    /// `claude` from `PATH`. Set this when the user's install lives outside
    /// `PATH` (e.g. a `~/.local/bin/claude` they haven't shimmed in yet).
    #[serde(default)]
    pub claude_code_path: String,

    /// Persisted 3D viewport background colour, as `[r, g, b]` 0..=255. `None`
    /// falls back to [`DEFAULT_VIEWER_BG_RGB`] — a neutral charcoal that
    /// matches the look of every major DCC app. Stored as bytes so the JSON
    /// stays readable; alpha is implied 255.
    #[serde(default)]
    pub viewer_bg_rgb: Option<[u8; 3]>,

    /// Whether the ground-plane reference grid is drawn in the 3D viewport.
    /// `None` falls back to `true` so existing settings files keep the grid
    /// visible after upgrade.
    #[serde(default)]
    pub show_grid: Option<bool>,

    /// User decision on sending crash reports to MoGen's self-hosted
    /// GlitchTip endpoint. `None` means undecided — the first-launch privacy
    /// prompt asks the user, then latches `Some(true)` (allow) or
    /// `Some(false)` (decline). The `MOGEN_DISABLE_TELEMETRY` and
    /// `DO_NOT_TRACK` env vars short-circuit to disabled regardless of the
    /// saved value, so users can opt out without touching this file.
    #[serde(default)]
    pub crash_reports_enabled: Option<bool>,
}

/// Default viewer background. Independent of the UI theme so the model's
/// colours read consistently regardless of the panel scheme. Tuned to match
/// Blender / Maya / Modo defaults.
pub const DEFAULT_VIEWER_BG_RGB: [u8; 3] = [54, 58, 64];

impl Settings {
    /// Maximum number of entries kept in [`Self::recent_files`].
    pub const MAX_RECENT: usize = 12;

    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        let Ok(bytes) = fs::read(&path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path().ok_or_else(|| "no config directory available".to_string())?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(())
    }

    pub fn gemini_api_key(&self) -> Option<&str> {
        let k = self.gemini_api_key.trim();
        if k.is_empty() {
            None
        } else {
            Some(k)
        }
    }

    /// Resolve the persisted label to a `ThinkingLevel`, falling back to the
    /// library default (`High`) when the field is empty or unknown.
    pub fn thinking_level(&self) -> ThinkingLevel {
        ThinkingLevel::parse(&self.thinking_level).unwrap_or(ThinkingLevel::High)
    }

    /// Resolve the persisted label to a `Theme`, falling back to `DEFAULT_THEME`
    /// when the field is empty or unknown.
    pub fn theme(&self) -> Theme {
        parse_theme(&self.theme).unwrap_or(DEFAULT_THEME)
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme_key(theme).to_string();
    }

    /// Resolve the persisted label to a `PreviewShader`, falling back to
    /// `DEFAULT_PREVIEW_SHADER` when the field is empty or unknown.
    pub fn preview_shader(&self) -> PreviewShader {
        parse_preview_shader(&self.preview_shader).unwrap_or(DEFAULT_PREVIEW_SHADER)
    }

    pub fn set_preview_shader(&mut self, shader: PreviewShader) {
        self.preview_shader = preview_shader_key(shader).to_string();
    }

    /// Current Gemini text model, falling back to the library default when
    /// the setting is empty.
    pub fn gemini_model(&self) -> String {
        let m = self.gemini_model.trim();
        if m.is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            m.to_string()
        }
    }

    /// Current "fast" Gemini model id, falling back to
    /// [`DEFAULT_FAST_MODEL`] when the setting is empty.
    pub fn gemini_fast_model(&self) -> String {
        let m = self.gemini_fast_model.trim();
        if m.is_empty() {
            DEFAULT_FAST_MODEL.to_string()
        } else {
            m.to_string()
        }
    }

    /// Sampling temperature, clamped to a sane range so a corrupted file
    /// can't feed nonsense into the API.
    pub fn temperature(&self) -> f32 {
        self.gemini_temperature
            .unwrap_or(DEFAULT_TEMPERATURE)
            .clamp(0.0, 2.0)
    }

    /// Max repair iterations, clamped to [0, 5]. Zero disables the loop.
    pub fn max_repair_iters(&self) -> u32 {
        self.max_repair_iters
            .unwrap_or(DEFAULT_MAX_REPAIR_ITERS)
            .min(5)
    }

    pub fn seed_override(&self) -> Option<u64> {
        self.seed_override
    }

    /// Resolve the persisted provider key to a [`Provider`], falling back to
    /// [`Provider::default`] when the field is empty or unknown. Stable
    /// across upgrades — old settings files (pre-multi-provider) read as the
    /// default Gemini.
    pub fn provider(&self) -> Provider {
        Provider::parse(&self.provider).unwrap_or_default()
    }

    /// API key for the currently-selected provider. Returns `None` for an
    /// empty value (including for Ollama — callers that need a keyless
    /// Ollama client construct one directly with an empty string).
    /// Claude Code is keyless (auth lives in the user's `claude` install)
    /// and always returns `None` here.
    pub fn provider_api_key(&self) -> Option<&str> {
        let raw = match self.provider() {
            Provider::Gemini => self.gemini_api_key.as_str(),
            Provider::OpenAI => self.openai_api_key.as_str(),
            Provider::Anthropic => self.anthropic_api_key.as_str(),
            Provider::Ollama => self.ollama_api_key.as_str(),
            Provider::ClaudeCode => "",
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    /// Path to the Claude Code binary, falling back to `claude` (resolved
    /// against `PATH`) when unset.
    pub fn claude_code_path(&self) -> String {
        let p = self.claude_code_path.trim();
        if p.is_empty() {
            mogen_llm::claude_code::DEFAULT_BINARY.to_string()
        } else {
            p.to_string()
        }
    }

    /// Heavy / "thinking" model id for the active provider. Reads the
    /// per-provider override; falls back to [`Provider::default_model`]
    /// when the override is empty.
    pub fn provider_model(&self) -> String {
        let provider = self.provider();
        let override_str = self.thinking_model_field(provider).trim();
        if override_str.is_empty() {
            provider.default_model().to_string()
        } else {
            override_str.to_string()
        }
    }

    /// Fast / cheap model id for the active provider. Symmetric with
    /// [`Self::provider_model`].
    pub fn provider_fast_model(&self) -> String {
        let provider = self.provider();
        let override_str = self.fast_model_field(provider).trim();
        if !override_str.is_empty() {
            return override_str.to_string();
        }
        // Ollama is the only provider whose library default for fast == thinking,
        // so let the user's thinking override apply to fast too when fast is blank.
        if matches!(provider, Provider::Ollama) {
            let thinking = self.ollama_model.trim();
            if !thinking.is_empty() {
                return thinking.to_string();
            }
        }
        provider.default_fast_model().to_string()
    }

    /// Borrow the per-provider thinking-model setting field. Used by the
    /// Preferences UI so the same combobox can read/write whichever provider
    /// is currently selected. Returns `""` for providers that don't yet have
    /// a dedicated override slot (e.g. Claude Code).
    pub fn thinking_model_field(&self, provider: Provider) -> &str {
        match provider {
            Provider::Gemini => &self.gemini_model,
            Provider::OpenAI => &self.openai_model,
            Provider::Anthropic => &self.anthropic_model,
            Provider::Ollama => &self.ollama_model,
            _ => "",
        }
    }

    /// Borrow the per-provider fast-model setting field.
    pub fn fast_model_field(&self, provider: Provider) -> &str {
        match provider {
            Provider::Gemini => &self.gemini_fast_model,
            Provider::OpenAI => &self.openai_fast_model,
            Provider::Anthropic => &self.anthropic_fast_model,
            Provider::Ollama => &self.ollama_fast_model,
            _ => "",
        }
    }

    /// Mutably borrow the per-provider thinking-model field for editing.
    /// Returns `None` for providers without a dedicated override slot.
    pub fn thinking_model_field_mut(&mut self, provider: Provider) -> Option<&mut String> {
        match provider {
            Provider::Gemini => Some(&mut self.gemini_model),
            Provider::OpenAI => Some(&mut self.openai_model),
            Provider::Anthropic => Some(&mut self.anthropic_model),
            Provider::Ollama => Some(&mut self.ollama_model),
            _ => None,
        }
    }

    /// Mutably borrow the per-provider fast-model field for editing.
    pub fn fast_model_field_mut(&mut self, provider: Provider) -> Option<&mut String> {
        match provider {
            Provider::Gemini => Some(&mut self.gemini_fast_model),
            Provider::OpenAI => Some(&mut self.openai_fast_model),
            Provider::Anthropic => Some(&mut self.anthropic_fast_model),
            Provider::Ollama => Some(&mut self.ollama_fast_model),
            _ => None,
        }
    }

    /// Persist a fresh provider selection.
    pub fn set_provider(&mut self, p: Provider) {
        self.provider = p.key().to_string();
    }

    /// Persisted viewport background as raw `[r, g, b]`, falling back to
    /// [`DEFAULT_VIEWER_BG_RGB`] when unset.
    pub fn viewer_bg_rgb(&self) -> [u8; 3] {
        self.viewer_bg_rgb.unwrap_or(DEFAULT_VIEWER_BG_RGB)
    }

    /// Replace the viewport background. Pass [`DEFAULT_VIEWER_BG_RGB`] to
    /// clear back to the default — we still persist it explicitly so the
    /// chosen colour is what survives a downgrade-then-upgrade.
    pub fn set_viewer_bg_rgb(&mut self, rgb: [u8; 3]) {
        self.viewer_bg_rgb = Some(rgb);
    }

    /// Whether the viewport grid is currently visible. Defaults to `true`
    /// when unset.
    pub fn show_grid(&self) -> bool {
        self.show_grid.unwrap_or(true)
    }

    pub fn set_show_grid(&mut self, on: bool) {
        self.show_grid = Some(on);
    }

    /// Promote `path` to the front of [`Self::recent_files`], dedup'ing any
    /// previous occurrence and trimming the list to [`Self::MAX_RECENT`].
    pub fn push_recent(&mut self, path: &str) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_string());
        self.recent_files.truncate(Self::MAX_RECENT);
    }

    /// Drop `path` from [`Self::recent_files`] if present.
    pub fn forget_recent(&mut self, path: &str) {
        self.recent_files.retain(|p| p != path);
    }
}

/// Lowercase label matching what `ThinkingLevel::parse` accepts.
pub fn thinking_level_key(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
    }
}

/// Human-facing label for the combobox; includes the token budget so users can
/// see why one setting is slower than another.
pub fn thinking_level_label(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Low => "Low (512 tok — fast)",
        ThinkingLevel::Medium => "Medium (2048 tok)",
        ThinkingLevel::High => "High (8192 tok — default)",
        ThinkingLevel::XHigh => "XHigh (24576 tok — slow)",
    }
}

pub const THINKING_LEVELS: [ThinkingLevel; 4] = [
    ThinkingLevel::Low,
    ThinkingLevel::Medium,
    ThinkingLevel::High,
    ThinkingLevel::XHigh,
];

/// Order in which providers appear in the Options dropdown. Gemini is first
/// because it's the historical default and the only image-capable backend.
pub const PROVIDERS: [Provider; 5] = [
    Provider::Gemini,
    Provider::OpenAI,
    Provider::Anthropic,
    Provider::Ollama,
    Provider::ClaudeCode,
];

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("mogen").join("settings.json"))
}
