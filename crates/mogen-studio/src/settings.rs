use std::fs;
use std::path::PathBuf;

use mogen_llm::gemini::{DEFAULT_FAST_MODEL, DEFAULT_MODEL, DEFAULT_TEMPERATURE};
use mogen_llm::ThinkingLevel;
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
    /// reopen the previous file.
    #[serde(default)]
    pub last_opened: Option<String>,
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
    /// animate and their repair loops). Empty -> library default
    /// (`gemini-pro-latest`). Exposed in Options → Models.
    #[serde(default)]
    pub gemini_model: String,

    /// "Fast" model id used for low-stakes rewrites like the Prompt Enhancer.
    /// Empty -> [`DEFAULT_FAST_MODEL`] (`gemini-flash-latest`). Kept separate
    /// from `gemini_model` so users can pay Flash rates for prompt polish
    /// while still running Pro for the actual DSL generation.
    #[serde(default)]
    pub gemini_fast_model: String,

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
}

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

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("mogen").join("settings.json"))
}
