//! OAuth client configuration.
//!
//! `mogen` ships with the public OAuth `client_id` / `client_secret` Google
//! publishes for the open-source Gemini CLI. Same values baked into
//! <https://github.com/google-gemini/gemini-cli> and reused by community
//! projects like <https://github.com/jenslys/opencode-gemini-auth>. They
//! authenticate the CLI itself, not the user — every login still goes
//! through Google's consent screen for the actual account.
//!
//! Out of the box, `mogen auth login` works with no setup. We do not read
//! these credentials from disk and there is no override mechanism — the
//! token bundle (`google_auth.json`) is the only OAuth artefact we
//! persist.
//!
//! Everything below (scopes, redirect URI, endpoint hosts, telemetry
//! headers) is non-secret and stays as `pub const`.
//!
//! Path-resolution helpers live here too so the token store
//! (`google_auth.json`) ends up in the canonical `~/.mogen/` location with
//! `~/.cache/mogen/` and `%LOCALAPPDATA%\mogen\` honoured as legacy
//! fallbacks for older installs.

use std::path::PathBuf;

/// Whitespace-separated OAuth scope list. Matches Google's Gemini CLI:
/// three scopes, no telemetry/experiments.
pub const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
                          https://www.googleapis.com/auth/userinfo.email \
                          https://www.googleapis.com/auth/userinfo.profile";

/// Public OAuth `client_id` for Google's Gemini CLI desktop client.
/// Same value baked into <https://github.com/google-gemini/gemini-cli>.
pub const CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";

/// Public OAuth `client_secret` paired with [`CLIENT_ID`]. Google ships
/// it in plain text in the open-source Gemini CLI; not a real secret in
/// the security sense, just paired identifier material.
pub const CLIENT_SECRET: &str = "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";

/// Loopback redirect URI port. Fixed by Google's OAuth client registration —
/// you cannot pick another port.
pub const REDIRECT_PORT: u16 = 51121;

/// Path component of the loopback redirect URI.
pub const REDIRECT_PATH: &str = "/oauth-callback";

/// Full redirect URI as it appears in the authorize request.
pub const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";

/// Authorization endpoint (consent screen).
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Token endpoint (code exchange + refresh).
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Production Cloud Code Assist endpoint base.
pub const ENDPOINT_PROD: &str = "https://cloudcode-pa.googleapis.com";

/// Daily Cloud Code Assist endpoint base (sandbox failover).
pub const ENDPOINT_DAILY: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";

/// Autopush Cloud Code Assist endpoint base (sandbox failover).
pub const ENDPOINT_AUTOPUSH: &str = "https://autopush-cloudcode-pa.sandbox.googleapis.com";

/// Endpoint walk order for `loadCodeAssist` discovery.
pub const ENDPOINT_FALLOVER: [&str; 3] = [ENDPOINT_PROD, ENDPOINT_DAILY, ENDPOINT_AUTOPUSH];

/// `User-Agent` string the Antigravity client uses on Cloud Code Assist
/// requests. We append `mogen-llm` so server-side telemetry can distinguish
/// our calls from the official client.
pub const USER_AGENT: &str = "gemini-cli/0.0.0 mogen-llm";

/// `X-Goog-Api-Client` header value for Cloud Code Assist requests.
pub const X_GOOG_API_CLIENT: &str = "gemini-cli/0.0.0";

/// `Client-Metadata` header value for Cloud Code Assist requests. The format
/// is `key=value,key=value`. We mirror Antigravity's `ideType=ANTIGRAVITY`
/// signal — the server expects this on `v1internal` to route the call.
pub const CLIENT_METADATA: &str =
    "ideType=ANTIGRAVITY,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI";

/// Minimum slack between `now` and `expires_at_unix` before we eagerly
/// refresh. Matches the Antigravity client's 60 s buffer.
pub const TOKEN_EXPIRY_BUFFER_SECS: u64 = 60;

/// Whether [`resolve_user_path`] is being asked for a path to *read* an
/// existing file (try legacy locations) or to *write* a new file (always
/// use the canonical `~/.mogen/` location, regardless of what's on disk).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathMode {
    Read,
    Write,
}

/// Walk all candidate `mogen`-owned directories, returning every existing
/// `filename` location. Used by `mogen auth logout` to clean up legacy
/// installs that wrote tokens to `~/.cache/mogen/` or `%LOCALAPPDATA%`
/// before the move to `~/.mogen/`.
pub fn all_existing_user_paths(filename: &str, file_override_var: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var(file_override_var) {
        if !p.trim().is_empty() {
            let path = PathBuf::from(p);
            if path.exists() {
                out.push(path);
            }
        }
    }
    if let Ok(dir) = std::env::var("MOGEN_CACHE_DIR") {
        if !dir.trim().is_empty() {
            let path = PathBuf::from(dir).join(filename);
            if path.exists() && !out.contains(&path) {
                out.push(path);
            }
        }
    }
    let home_candidate = std::env::var("HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    if let Some(home) = home_candidate.as_deref() {
        for dir in [".mogen", ".cache/mogen"] {
            let mut p = PathBuf::from(home);
            for seg in dir.split('/') {
                p.push(seg);
            }
            p.push(filename);
            if p.exists() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    if let Ok(localapp) = std::env::var("LOCALAPPDATA") {
        if !localapp.trim().is_empty() {
            let p = PathBuf::from(localapp).join("mogen").join(filename);
            if p.exists() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Resolve a `mogen`-owned file path. In [`PathMode::Read`] mode, prefers
/// the canonical `~/.mogen/{filename}` but falls back to legacy
/// `~/.cache/mogen/` and `%LOCALAPPDATA%\mogen\` if the canonical file
/// doesn't exist yet (lets existing installs keep working after the move
/// to `~/.mogen/`). In [`PathMode::Write`] mode, skips the existence
/// checks and goes straight to the canonical target so new logins always
/// land in `~/.mogen/`.
///
/// `file_override_var` (e.g. `MOGEN_TOKEN_STORE`) supplies an absolute
/// path override; `MOGEN_CACHE_DIR` overrides the directory. Both are
/// honoured in either mode.
pub fn resolve_user_path(
    filename: &str,
    file_override_var: &str,
    mode: PathMode,
) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(file_override_var) {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if let Ok(dir) = std::env::var("MOGEN_CACHE_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join(filename));
        }
    }
    let home_candidate = std::env::var("HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    if mode == PathMode::Read {
        if let Some(home) = home_candidate.as_deref() {
            let dotdir = PathBuf::from(home).join(".mogen").join(filename);
            if dotdir.exists() {
                return Some(dotdir);
            }
            let legacy = PathBuf::from(home).join(".cache").join("mogen").join(filename);
            if legacy.exists() {
                return Some(legacy);
            }
        }
        if let Ok(localapp) = std::env::var("LOCALAPPDATA") {
            if !localapp.trim().is_empty() {
                let legacy = PathBuf::from(localapp).join("mogen").join(filename);
                if legacy.exists() {
                    return Some(legacy);
                }
            }
        }
    }
    // Canonical write target (also the read-mode fallback when nothing is
    // on disk yet): `~/.mogen/{filename}`.
    if let Some(home) = home_candidate {
        return Some(PathBuf::from(home).join(".mogen").join(filename));
    }
    if let Ok(localapp) = std::env::var("LOCALAPPDATA") {
        if !localapp.trim().is_empty() {
            return Some(PathBuf::from(localapp).join("mogen").join(filename));
        }
    }
    None
}
