//! OAuth client configuration.
//!
//! `mogen` ships with default OAuth credentials that point at Google's
//! published Gemini CLI desktop client (the same `client_id` /
//! `client_secret` baked into <https://github.com/google-gemini/gemini-cli>
//! and other open-source plugins like
//! <https://github.com/jenslys/opencode-gemini-auth>). Out of the box,
//! `mogen auth login` works with no setup: the user clicks through the
//! standard Google consent screen and we get a token back.
//!
//! Users who want to point `mogen` at their own OAuth client (custom
//! Cloud Console desktop client, the Antigravity client, etc.) can drop a
//! populated `oauth_client.json` somewhere in the resolution chain below
//! and that file overrides the compiled-in defaults.
//!
//! Path resolution (first hit wins):
//!
//! 1. `$MOGEN_OAUTH_CLIENT` (full path to a JSON file).
//! 2. `$MOGEN_CACHE_DIR/oauth_client.json`.
//! 3. `$HOME/.mogen/oauth_client.json` (primary default on Unix and Windows).
//! 4. `%USERPROFILE%\.mogen\oauth_client.json` (Windows when `$HOME` is unset).
//! 5. `$HOME/.cache/mogen/oauth_client.json` (legacy fallback for older installs).
//! 6. `%LOCALAPPDATA%\mogen\oauth_client.json` (legacy fallback on Windows).
//!
//! File schema (extra fields tolerated):
//!
//! ```json
//! { "client_id": "...", "client_secret": "..." }
//! ```
//!
//! Everything else (scopes, redirect URI, endpoint hosts, telemetry headers)
//! is non-secret and stays as `pub const` below.

use std::path::PathBuf;

use serde::Deserialize;

use super::OAuthError;

/// Whitespace-separated OAuth scope list. Matches Google's Gemini CLI
/// (the default client we ship): three scopes, no telemetry/experiments.
/// If a user supplies their own OAuth client via `oauth_client.json`,
/// the consent screen will still ask for these three scopes — register
/// them when creating the client.
pub const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
                          https://www.googleapis.com/auth/userinfo.email \
                          https://www.googleapis.com/auth/userinfo.profile";

/// Default `client_id` — Google's published Gemini CLI desktop client.
/// Same value baked into <https://github.com/google-gemini/gemini-cli>.
/// Overridden by a populated `oauth_client.json` if one is present.
pub const DEFAULT_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";

/// Default `client_secret` paired with [`DEFAULT_CLIENT_ID`]. Public
/// (Google's open-source Gemini CLI ships it in plain text); not a real
/// secret in the security sense, just paired identifier material.
pub const DEFAULT_CLIENT_SECRET: &str = "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";

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

/// Filename of the OAuth client secrets JSON in cache/config dirs.
pub const CLIENT_SECRETS_FILENAME: &str = "oauth_client.json";

/// On-disk OAuth client secrets. Extra fields in the JSON are tolerated so
/// the file can grow keys (e.g. mirroring Google's `client_secret.json`
/// download format) without breaking older `mogen` builds.
#[derive(Debug, Clone, Deserialize)]
pub struct ClientSecrets {
    pub client_id: String,
    pub client_secret: String,
}

/// Whether [`resolve_user_path`] is being asked for a path to *read* an
/// existing file (try legacy locations) or to *write* a new file (always
/// use the canonical `~/.mogen/` location, regardless of what's on disk).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathMode {
    Read,
    Write,
}

/// Resolve the JSON path used by [`load_client_secrets`]. Mirrors
/// [`super::store::token_store_path`] but for the client-secrets file.
pub fn client_secrets_path() -> Option<PathBuf> {
    resolve_user_path(CLIENT_SECRETS_FILENAME, "MOGEN_OAUTH_CLIENT", PathMode::Read)
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
/// `file_override_var` (e.g. `MOGEN_OAUTH_CLIENT`) supplies an absolute
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

/// Resolve the OAuth client credentials. Tries the user-supplied
/// `oauth_client.json` first (path resolution above); when no usable file
/// is present, falls back to the compiled-in [`DEFAULT_CLIENT_ID`] /
/// [`DEFAULT_CLIENT_SECRET`] (Google's public Gemini CLI client).
///
/// "Usable" means: file exists, parses as JSON, and both fields are
/// non-empty and not still set to the `REPLACE_ME` placeholders shipped
/// in `oauth_client.example.json`. A file that exists but parses badly
/// (or has the wrong type) errors hard so the user notices — silently
/// dropping back to defaults would mask their override attempt.
pub fn load_client_secrets() -> Result<ClientSecrets, OAuthError> {
    if let Some(path) = client_secrets_path() {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let parsed: ClientSecrets = serde_json::from_slice(&bytes).map_err(|e| {
                    OAuthError::MissingClientSecrets {
                        path: Some(path.clone()),
                        reason: format!("invalid JSON: {e}"),
                    }
                })?;
                let id = parsed.client_id.trim();
                let secret = parsed.client_secret.trim();
                if !id.is_empty()
                    && !secret.is_empty()
                    && !id.contains("REPLACE_ME")
                    && !secret.contains("REPLACE_ME")
                {
                    return Ok(parsed);
                }
                // File is a placeholder copy of `oauth_client.example.json`;
                // fall through to compiled-in defaults so the user still gets
                // a working login flow.
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // No override file — fall through to defaults.
            }
            Err(err) => {
                return Err(OAuthError::MissingClientSecrets {
                    path: Some(path),
                    reason: format!("read failed: {err}"),
                });
            }
        }
    }
    Ok(ClientSecrets {
        client_id: DEFAULT_CLIENT_ID.to_string(),
        client_secret: DEFAULT_CLIENT_SECRET.to_string(),
    })
}
