use std::path::PathBuf;

/// Canonical settings path: `~/.mogen/settings.json`. Shared with the
/// CLI so a key entered in Studio's Preferences also satisfies
/// `mogen generate` etc. without requiring an env var.
///
/// `mode` follows [`mogen_llm::PathMode`]:
///   - `Read` walks legacy locations (old `dirs::config_dir()/mogen/`,
///     `~/.cache/mogen/`, `%LOCALAPPDATA%\mogen\`) before falling back
///     to the canonical target. Lets existing installs keep working
///     after the move.
///   - `Write` always returns the canonical `~/.mogen/` target so new
///     saves migrate forward automatically.
pub(super) fn settings_path(mode: mogen_llm::PathMode) -> Option<PathBuf> {
    if let Some(p) = mogen_llm::settings_store_path(mode) {
        return Some(p);
    }
    // Last-ditch: the legacy `dirs::config_dir()` location. Reached
    // only when neither HOME/USERPROFILE nor LOCALAPPDATA are set,
    // which effectively never happens.
    dirs::config_dir().map(|d| d.join("mogen").join("settings.json"))
}

/// Legacy `dirs::config_dir()/mogen/settings.json` location. Probed
/// during `Settings::load` so Studio installs that pre-date the move
/// to `~/.mogen/` keep their saved keys/preferences on first launch.
pub(super) fn legacy_settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("mogen").join("settings.json"))
}

/// Default MoGHub origin. Production deployment lives here; the field
/// is editable in Preferences (and overridable at process scope via the
/// `MOGHUB_URL` env var honoured by `MoghubClient::from_env`).
pub(super) fn default_moghub_url() -> String {
    "https://moghub.org".to_string()
}
