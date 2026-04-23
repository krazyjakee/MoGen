use std::fs;
use std::path::PathBuf;

use mgen_llm::ThinkingLevel;
use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub gemini_api_key: String,
    /// Persisted as a lowercase label (`low` | `medium` | `high` | `xhigh`) so
    /// new `ThinkingLevel` variants can be added without a migration. Empty /
    /// unknown falls back to the library default at read time.
    #[serde(default)]
    pub thinking_level: String,
}

impl Settings {
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
    dirs::config_dir().map(|d| d.join("mgen").join("settings.json"))
}
