//! Shared API-key store at `~/.mogen/settings.json`.
//!
//! Both the CLI and Studio read keys from the same file so a user only
//! has to enter their key in one place. Studio writes the file as part
//! of its larger settings struct (theme, model picks, panel widths,
//! …). The CLI only deserializes the API-key fields it cares about —
//! every other field is silently ignored, so the two sides stay in
//! sync without sharing a struct definition.
//!
//! Key resolution precedence used by `mogen` CLI helpers:
//!     1. explicit `--api-key` flag
//!     2. provider env var (`GEMINI_API_KEY`, `OPENAI_API_KEY`, …)
//!     3. `~/.mogen/settings.json` (this module)
//!     4. error (or, for Gemini, fall through to OAuth bundle on disk)
//!
//! Path resolution honours `MOGEN_SETTINGS` (full file path),
//! `MOGEN_CACHE_DIR` (parent dir), and the legacy `~/.cache/mogen/` /
//! `%LOCALAPPDATA%\mogen\` locations for read access — same scheme as
//! the OAuth token store.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::google_oauth::client::{resolve_user_path, PathMode};
use crate::Provider;

/// Filename inside the resolved mogen-owned directory.
const FILENAME: &str = "settings.json";
/// Env var that, when set, overrides the entire settings-file path.
const ENV_OVERRIDE: &str = "MOGEN_SETTINGS";

/// Subset of the on-disk settings file that the CLI cares about. All
/// fields are optional; serde defaults blank strings for missing keys
/// and silently ignores unknown ones (Studio writes many extras).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiKeys {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub gemini_api_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub openai_api_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub anthropic_api_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ollama_api_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fireworks_api_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub zai_api_key: String,
    /// When `Some(true)` (or absent — defaults to true), Z.ai chat calls
    /// route through the dedicated GLM Coding Plan endpoint
    /// (`/api/coding/paas/v4`). When `Some(false)`, use the general PaaS
    /// endpoint (`/api/paas/v4`). The coding endpoint is rate-limited
    /// less aggressively for keys carrying heavy system instructions
    /// (the MoGen DSL prompt) and avoids the `os error 10054` peer
    /// resets users on the coding plan see on the general endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zai_use_coding_plan: Option<bool>,
}

/// Resolve the canonical `~/.mogen/settings.json` path. Returns `None`
/// only when the platform exposes neither `HOME`/`USERPROFILE` nor
/// `LOCALAPPDATA` (effectively never on a real install).
pub fn settings_path(mode: PathMode) -> Option<PathBuf> {
    resolve_user_path(FILENAME, ENV_OVERRIDE, mode)
}

/// Load the on-disk API keys. Returns `None` when the file is missing
/// or unreadable; returns `Some(ApiKeys::default())` when the file
/// parses but contains no key fields.
pub fn load_api_keys() -> Option<ApiKeys> {
    let path = settings_path(PathMode::Read)?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice::<ApiKeys>(&bytes).ok()
}

/// Read the API key for `provider` from `~/.mogen/settings.json`.
/// Returns `None` for keyless providers and when the field is missing
/// or whitespace-only. The returned string is trimmed.
pub fn read_api_key(provider: Provider) -> Option<String> {
    if provider.is_keyless() {
        return None;
    }
    let keys = load_api_keys()?;
    let raw = match provider {
        Provider::Gemini => &keys.gemini_api_key,
        Provider::OpenAI => &keys.openai_api_key,
        Provider::Anthropic => &keys.anthropic_api_key,
        Provider::Ollama => &keys.ollama_api_key,
        Provider::Fireworks => &keys.fireworks_api_key,
        Provider::Zai => &keys.zai_api_key,
        Provider::ClaudeCode => return None,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Resolve the Z.ai chat-completions base URL according to the persisted
/// `zai_use_coding_plan` toggle. Returns the dedicated GLM Coding Plan
/// endpoint when the toggle is `Some(true)` or absent (default-on); the
/// general PaaS endpoint when the toggle is `Some(false)`.
///
/// Used by the CLI's [`crate::Provider::Zai`] client construction so a
/// single user file controls both surfaces without each call site
/// re-implementing the toggle.
pub fn zai_base_url() -> &'static str {
    let on = load_api_keys()
        .and_then(|k| k.zai_use_coding_plan)
        .unwrap_or(true);
    if on {
        crate::ZAI_CODING_PLAN_BASE_URL
    } else {
        crate::ZAI_DEFAULT_BASE_URL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_file() {
        let json = r#"{"gemini_api_key": "abc123"}"#;
        let keys: ApiKeys = serde_json::from_str(json).unwrap();
        assert_eq!(keys.gemini_api_key, "abc123");
        assert_eq!(keys.openai_api_key, "");
        assert_eq!(keys.zai_api_key, "");
    }

    #[test]
    fn parses_all_fields() {
        let json = r#"{
            "gemini_api_key": "g",
            "openai_api_key": "o",
            "anthropic_api_key": "a",
            "ollama_api_key": "ol",
            "fireworks_api_key": "f",
            "zai_api_key": "z"
        }"#;
        let keys: ApiKeys = serde_json::from_str(json).unwrap();
        assert_eq!(keys.gemini_api_key, "g");
        assert_eq!(keys.openai_api_key, "o");
        assert_eq!(keys.anthropic_api_key, "a");
        assert_eq!(keys.ollama_api_key, "ol");
        assert_eq!(keys.fireworks_api_key, "f");
        assert_eq!(keys.zai_api_key, "z");
    }

    #[test]
    fn ignores_unknown_studio_fields() {
        // Studio writes many fields beyond the API keys. The CLI must
        // tolerate them — otherwise the shared file breaks one side.
        let json = r#"{
            "gemini_api_key": "abc",
            "openai_api_key": "def",
            "theme_label": "Dark",
            "panel_width": 320,
            "thinking_level": "High",
            "recent_files": ["a.mog", "b.mog"]
        }"#;
        let keys: ApiKeys = serde_json::from_str(json).unwrap();
        assert_eq!(keys.gemini_api_key, "abc");
        assert_eq!(keys.openai_api_key, "def");
    }

    #[test]
    fn empty_file_yields_blank_keys() {
        let json = r#"{}"#;
        let keys: ApiKeys = serde_json::from_str(json).unwrap();
        assert_eq!(keys.gemini_api_key, "");
    }

    #[test]
    fn round_trips_only_set_fields() {
        // Default ApiKeys round-trips to `{}` because every field
        // skips on empty. This keeps the file from growing useless
        // empty-string entries when the CLI rewrites it (currently it
        // doesn't, but we want the option open).
        let keys = ApiKeys::default();
        let json = serde_json::to_string(&keys).unwrap();
        assert_eq!(json, "{}");
    }
}
