//! OAuth client configuration.
//!
//! The Google OAuth `client_id` / `client_secret` are **not** baked into the
//! source tree — they're loaded from a small JSON file at runtime. This keeps
//! third-party-extracted Antigravity credentials out of the repo (and out of
//! GitHub's secret-scanning grasp) while still letting users wire `mogen` up
//! to any Google OAuth client they like.
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
//!
//! See `oauth_client.example.json` and the README's "Sign in with a paid
//! Gemini account" section for setup instructions.

use std::path::PathBuf;

use serde::Deserialize;

use super::OAuthError;

/// Whitespace-separated OAuth scope list. Same order as Antigravity requests
/// so the consent screen looks identical.
pub const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
                          https://www.googleapis.com/auth/userinfo.email \
                          https://www.googleapis.com/auth/userinfo.profile \
                          https://www.googleapis.com/auth/cclog \
                          https://www.googleapis.com/auth/experimentsandconfigs";

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

/// Resolve the JSON path used by [`load_client_secrets`]. Mirrors
/// [`super::store::token_store_path`] but for the client-secrets file.
pub fn client_secrets_path() -> Option<PathBuf> {
    resolve_user_path(CLIENT_SECRETS_FILENAME, "MOGEN_OAUTH_CLIENT")
}

/// Walk the standard `mogen`-owned directory chain, returning the first
/// candidate path for `filename`. Used for both the OAuth client secrets
/// file and the token store so they end up next to each other in
/// `~/.mogen/`.
///
/// `file_override_var` (e.g. `MOGEN_OAUTH_CLIENT`) supplies an absolute
/// path override; `MOGEN_CACHE_DIR` overrides the directory. Otherwise the
/// chain is `~/.mogen/`, then `~/.cache/mogen/` and `%LOCALAPPDATA%\mogen\`
/// as legacy fallbacks (so existing installs keep working after the move
/// to `~/.mogen/`).
pub fn resolve_user_path(filename: &str, file_override_var: &str) -> Option<PathBuf> {
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
    // Nothing on disk yet — return the canonical write target so callers
    // that *create* the file (token-store save) land in `~/.mogen/`.
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

/// Read and parse the client secrets JSON. Re-reads on every call — the file
/// is small and the call sites (login, refresh, code exchange) run at most
/// hourly. Errors are surfaced as [`OAuthError::MissingClientSecrets`] with
/// enough detail for the CLI / Studio to point users at the README.
pub fn load_client_secrets() -> Result<ClientSecrets, OAuthError> {
    let path = client_secrets_path().ok_or_else(|| OAuthError::MissingClientSecrets {
        path: None,
        reason: "no MOGEN_OAUTH_CLIENT, MOGEN_CACHE_DIR, HOME, LOCALAPPDATA, or \
                USERPROFILE set; cannot locate oauth_client.json"
            .into(),
    })?;
    let bytes = std::fs::read(&path).map_err(|e| OAuthError::MissingClientSecrets {
        path: Some(path.clone()),
        reason: format!("read failed: {e}"),
    })?;
    let parsed: ClientSecrets =
        serde_json::from_slice(&bytes).map_err(|e| OAuthError::MissingClientSecrets {
            path: Some(path.clone()),
            reason: format!("invalid JSON: {e}"),
        })?;
    if parsed.client_id.trim().is_empty() || parsed.client_secret.trim().is_empty() {
        return Err(OAuthError::MissingClientSecrets {
            path: Some(path),
            reason: "client_id or client_secret is empty".into(),
        });
    }
    Ok(parsed)
}
