//! End-to-end OAuth login flow.
//!
//! Order of operations:
//! 1. Bind 127.0.0.1:51121 *before* opening the browser. If the port is
//!    busy we surface [`OAuthError::PortInUse`] instantly instead of
//!    leaving the user staring at a Google consent screen that nothing is
//!    listening for.
//! 2. Generate PKCE pair + state nonce.
//! 3. Build the authorize URL and open it in the user's default browser
//!    (or just print it when `open_browser=false`).
//! 4. Wait for the callback, validate `state`, exchange `code` for tokens.
//! 5. Hit `/userinfo` for the email, then `loadCodeAssist` for the project id.

use std::time::Duration;

use serde::Deserialize;

use super::client;
use super::pkce::PkcePair;
use super::project;
use super::server;
use super::token::OAuthBundle;
use super::OAuthError;

/// Caller-controlled knobs for [`run_login_flow`].
pub struct LoginOptions {
    /// When true, attempt to open the authorize URL in the user's default
    /// browser. When false (the `--no-browser` flag), the caller is
    /// expected to print `authorize_url` themselves.
    pub open_browser: bool,
    /// Hard deadline on the entire flow, applied while the loopback server
    /// is waiting for the callback. Default 5 minutes.
    pub timeout: Duration,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self { open_browser: true, timeout: Duration::from_secs(300) }
    }
}

/// Result of a successful login: the populated bundle plus a copy of the
/// authorize URL that was opened (handy for the CLI's `--no-browser` path).
pub struct LoginOutcome {
    pub bundle: OAuthBundle,
    pub authorize_url: String,
}

/// Drive the full login flow. Side effects: binds a port, opens a browser
/// (optional), makes 3 HTTPS calls (token exchange, userinfo,
/// loadCodeAssist). Does *not* persist the bundle — the caller chooses
/// where to write it.
pub fn run_login_flow(opts: LoginOptions) -> Result<LoginOutcome, OAuthError> {
    let pkce = PkcePair::generate();
    let server = server::bind()?;
    let authorize_url = build_authorize_url(&pkce);

    if opts.open_browser {
        if let Err(err) = webbrowser::open(&authorize_url) {
            // Non-fatal — we still wait for the callback in case the user
            // pastes the URL manually. CLI surfaces this to stderr.
            eprintln!(
                "warning: failed to open browser ({err}); paste this URL into a browser:\n{authorize_url}",
            );
        }
    }

    let cb = server::wait_for_callback(server, opts.timeout)?;
    if cb.state != pkce.state {
        return Err(OAuthError::StateMismatch);
    }

    let http = build_http_client()?;
    let now = now_unix();
    let token = exchange_code(&http, &cb.code, &pkce.verifier, now)?;

    let email = fetch_email(&http, &token.access_token).ok().flatten();
    let discovery = project::discover(&http, &token.access_token)?;

    let bundle = OAuthBundle {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        access_expires_at_unix: now.saturating_add(token.expires_in),
        obtained_at_unix: now,
        email,
        project_id: discovery.project_id,
        managed_project_id: discovery.managed_project_id,
        endpoint_base: Some(discovery.endpoint_base),
        scope: token.scope,
    };

    Ok(LoginOutcome { bundle, authorize_url })
}

fn build_http_client() -> Result<reqwest::blocking::Client, OAuthError> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(OAuthError::from)
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build the authorize URL. We hard-code `access_type=offline` so Google
/// returns a refresh token, and `prompt=consent` to be sure the consent
/// screen runs even on second logins (otherwise refresh tokens may be
/// withheld for already-consented apps).
fn build_authorize_url(pkce: &PkcePair) -> String {
    let mut url = String::with_capacity(512);
    url.push_str(client::AUTH_URL);
    url.push('?');
    append_param(&mut url, "client_id", client::CLIENT_ID, true);
    append_param(&mut url, "redirect_uri", client::REDIRECT_URI, false);
    append_param(&mut url, "response_type", "code", false);
    append_param(&mut url, "scope", client::SCOPES, false);
    append_param(&mut url, "code_challenge", &pkce.challenge, false);
    append_param(&mut url, "code_challenge_method", "S256", false);
    append_param(&mut url, "state", &pkce.state, false);
    append_param(&mut url, "access_type", "offline", false);
    append_param(&mut url, "prompt", "consent", false);
    append_param(&mut url, "include_granted_scopes", "true", false);
    url
}

fn append_param(url: &mut String, key: &str, value: &str, first: bool) {
    if !first {
        url.push('&');
    }
    url.push_str(key);
    url.push('=');
    url.push_str(&percent_encode(value));
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let unreserved = matches!(
            *byte,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~'
        );
        if unreserved {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    token_type: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    id_token: Option<String>,
}

fn exchange_code(
    http: &reqwest::blocking::Client,
    code: &str,
    verifier: &str,
    _now_unix: u64,
) -> Result<TokenExchangeResponse, OAuthError> {
    let form = [
        ("client_id", client::CLIENT_ID),
        ("client_secret", client::CLIENT_SECRET),
        ("code", code),
        ("code_verifier", verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", client::REDIRECT_URI),
    ];
    let resp = http.post(client::TOKEN_URL).form(&form).send()?;
    let status = resp.status();
    let bytes = resp.bytes()?;
    if !status.is_success() {
        let message = parse_error(&bytes);
        return Err(OAuthError::TokenExchange { status: status.as_u16(), message });
    }
    let parsed: TokenExchangeResponse = serde_json::from_slice(&bytes)?;
    Ok(parsed)
}

#[derive(Debug, Deserialize)]
struct UserInfo {
    #[serde(default)]
    email: Option<String>,
}

/// Best-effort email lookup. Failures are not fatal — we just store `None`
/// and let `mogen auth status` say "Logged in." without a name.
fn fetch_email(
    http: &reqwest::blocking::Client,
    access_token: &str,
) -> Result<Option<String>, OAuthError> {
    let resp = http
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .bearer_auth(access_token)
        .send()?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let info: UserInfo = resp.json()?;
    Ok(info.email)
}

fn parse_error(bytes: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if let Some(d) = v.get("error_description").and_then(|s| s.as_str()) {
            return d.to_string();
        }
        if let Some(e) = v.get("error").and_then(|s| s.as_str()) {
            return e.to_string();
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

// --- Test seam (used by integration tests) ----------------------------------

/// Test-only: drive the code exchange against an injected token URL.
#[doc(hidden)]
pub fn exchange_code_against(
    http: &reqwest::blocking::Client,
    code: &str,
    verifier: &str,
    token_url: &str,
) -> Result<(String, String, u64, Option<String>), OAuthError> {
    let form = [
        ("client_id", client::CLIENT_ID),
        ("client_secret", client::CLIENT_SECRET),
        ("code", code),
        ("code_verifier", verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", client::REDIRECT_URI),
    ];
    let resp = http.post(token_url).form(&form).send()?;
    let status = resp.status();
    let bytes = resp.bytes()?;
    if !status.is_success() {
        let message = parse_error(&bytes);
        return Err(OAuthError::TokenExchange { status: status.as_u16(), message });
    }
    let parsed: TokenExchangeResponse = serde_json::from_slice(&bytes)?;
    Ok((parsed.access_token, parsed.refresh_token, parsed.expires_in, parsed.scope))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_contains_required_params() {
        let pkce = PkcePair::generate();
        let url = build_authorize_url(&pkce);
        assert!(url.starts_with(client::AUTH_URL));
        // CLIENT_ID is percent-encoded in the URL but contains only
        // unreserved characters (digits + `-` + `.`), so the raw value
        // appears verbatim.
        assert!(
            url.contains(&format!("client_id={}", client::CLIENT_ID)),
            "got: {url}"
        );
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge)));
        assert!(url.contains(&format!("state={}", pkce.state)));
        assert!(url.contains("access_type=offline"));
    }

    #[test]
    fn percent_encode_handles_reserved_bytes() {
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
        assert_eq!(percent_encode("safe-_.~"), "safe-_.~");
    }
}
