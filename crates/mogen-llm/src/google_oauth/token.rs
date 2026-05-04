//! Token bundle + refresh logic.
//!
//! Mirrors Antigravity's `accessTokenExpired` 60 s buffer and `refreshAccessToken`
//! against `oauth2.googleapis.com/token` with `grant_type=refresh_token`. On a
//! 4xx response containing `invalid_grant` we surface [`OAuthError::Revoked`]
//! so the CLI can ask the user to re-run `mogen auth login --force`.

use serde::{Deserialize, Serialize};

use super::client::{self, ProviderConfig};
use super::OAuthError;

/// Persisted credential bundle.
///
/// `access_token` / `refresh_token` are obvious; `access_expires_at_unix` is
/// our pre-computed expiry (server returns `expires_in` seconds — we add it
/// to `now` once at exchange time and persist the absolute moment so we can
/// compare without doing arithmetic at every call site).
///
/// `endpoint_base` is sticky: once `loadCodeAssist` succeeds against e.g.
/// `cloudcode-pa.googleapis.com`, every subsequent generate call reuses that
/// host instead of re-walking the prod→daily→autopush list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthBundle {
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_at_unix: u64,
    #[serde(default)]
    pub obtained_at_unix: u64,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub managed_project_id: Option<String>,
    #[serde(default)]
    pub endpoint_base: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl OAuthBundle {
    /// True if the access token is within [`client::TOKEN_EXPIRY_BUFFER_SECS`]
    /// of expiry (or already expired). The buffer absorbs round-trip latency
    /// so a token that's about to flip mid-request gets refreshed first.
    pub fn is_access_expired(&self, now_unix: u64) -> bool {
        now_unix.saturating_add(client::TOKEN_EXPIRY_BUFFER_SECS) >= self.access_expires_at_unix
    }
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: String,
    /// Google may issue a rotated refresh token alongside the new access
    /// token. When present we replace; when absent we keep the existing one.
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    /// Returned but currently unused — left to deserialise so the field is
    /// represented in the wire decode and so we can log it under verbose
    /// debug if needed later.
    #[serde(default)]
    #[allow(dead_code)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// Refresh `bundle.access_token` only if it's within the expiry buffer.
/// Returns `Ok(true)` when a network round-trip happened, `Ok(false)` when
/// the existing token was still good.
pub fn refresh_if_needed(
    http: &reqwest::blocking::Client,
    bundle: &mut OAuthBundle,
    now_unix: u64,
    config: &ProviderConfig,
) -> Result<bool, OAuthError> {
    if !bundle.is_access_expired(now_unix) {
        return Ok(false);
    }
    refresh_now(http, bundle, now_unix, config)?;
    Ok(true)
}

/// Force a refresh round-trip against the token endpoint and update the
/// bundle in place. Used directly by `mogen auth login --force`-adjacent
/// flows and on a 401 retry from Cloud Code Assist.
pub fn refresh_now(
    http: &reqwest::blocking::Client,
    bundle: &mut OAuthBundle,
    now_unix: u64,
    config: &ProviderConfig,
) -> Result<(), OAuthError> {
    refresh_against(http, bundle, now_unix, client::TOKEN_URL, config)
}

/// Test seam — same logic as [`refresh_now`] but the token endpoint URL is
/// injected. Lets the integration tests stand up a mock server.
pub fn refresh_against(
    http: &reqwest::blocking::Client,
    bundle: &mut OAuthBundle,
    now_unix: u64,
    token_url: &str,
    config: &ProviderConfig,
) -> Result<(), OAuthError> {
    let form = [
        ("client_id", config.client_id),
        ("client_secret", config.client_secret),
        ("refresh_token", bundle.refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];

    let resp = http.post(token_url).form(&form).send()?;
    let status = resp.status();
    let bytes = resp.bytes()?;

    if !status.is_success() {
        let raw = String::from_utf8_lossy(&bytes);
        if raw.contains("invalid_grant") {
            return Err(OAuthError::Revoked);
        }
        let message = parse_error(&bytes);
        return Err(OAuthError::TokenExchange { status: status.as_u16(), message });
    }

    let parsed: RefreshResponse = serde_json::from_slice(&bytes)?;
    bundle.access_token = parsed.access_token;
    if let Some(rt) = parsed.refresh_token {
        bundle.refresh_token = rt;
    }
    bundle.access_expires_at_unix = now_unix.saturating_add(parsed.expires_in);
    bundle.obtained_at_unix = now_unix;
    if let Some(scope) = parsed.scope {
        bundle.scope = Some(scope);
    }
    Ok(())
}

fn parse_error(bytes: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if let Some(desc) = v.get("error_description").and_then(|s| s.as_str()) {
            return desc.to_string();
        }
        if let Some(err) = v.get("error").and_then(|s| s.as_str()) {
            return err.to_string();
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(expires_at: u64) -> OAuthBundle {
        OAuthBundle {
            access_token: "old".into(),
            refresh_token: "rt".into(),
            access_expires_at_unix: expires_at,
            obtained_at_unix: 0,
            email: None,
            project_id: None,
            managed_project_id: None,
            endpoint_base: None,
            scope: None,
        }
    }

    #[test]
    fn fresh_token_is_not_expired() {
        let b = fixture(1_000_000);
        assert!(!b.is_access_expired(1_000_000 - 120));
    }

    #[test]
    fn token_inside_buffer_is_expired() {
        let b = fixture(1_000_000);
        // 60 s buffer; sample now=expiry-30 should already trip refresh.
        assert!(b.is_access_expired(1_000_000 - 30));
    }

    #[test]
    fn already_expired_token_is_expired() {
        let b = fixture(100);
        assert!(b.is_access_expired(1_000_000));
    }
}
