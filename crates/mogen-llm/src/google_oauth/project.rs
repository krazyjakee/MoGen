//! Project-id discovery + endpoint failover.
//!
//! After OAuth code exchange we POST `:loadCodeAssist` to find the Google
//! Cloud project tied to the user's Antigravity entitlement. The Antigravity
//! client walks the prod → daily → autopush endpoints until one of them
//! responds — we mirror that on transport-level errors only. A 4xx from prod
//! is propagated as-is (the user has prod auth but a real-world problem;
//! falling through to daily would hide it).

use serde::Deserialize;

use super::client;
use super::OAuthError;

/// Fields lifted out of the `loadCodeAssist` response we actually need.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct LoadCodeAssistResponse {
    #[serde(default)]
    cloudaicompanion_project: Option<String>,
    #[serde(default)]
    managed_project_id: Option<String>,
}

/// Outcome of project discovery: which endpoint won, plus the resolved ids.
#[derive(Debug, Clone)]
pub struct Discovery {
    pub endpoint_base: String,
    pub project_id: Option<String>,
    pub managed_project_id: Option<String>,
}

/// Walk prod → daily → autopush, returning the first endpoint that
/// successfully responds. Test seam: [`discover_against`] takes the host list
/// directly so an integration test can substitute a mock-server URL.
pub fn discover(
    http: &reqwest::blocking::Client,
    access_token: &str,
) -> Result<Discovery, OAuthError> {
    discover_against(http, access_token, &client::ENDPOINT_FALLOVER)
}

/// Same as [`discover`] but with the endpoint list injected.
pub fn discover_against(
    http: &reqwest::blocking::Client,
    access_token: &str,
    endpoints: &[&str],
) -> Result<Discovery, OAuthError> {
    let mut last_err: Option<OAuthError> = None;
    for ep in endpoints {
        match call_load_code_assist(http, access_token, ep) {
            Ok(resp) => {
                return Ok(Discovery {
                    endpoint_base: (*ep).to_string(),
                    project_id: resp.cloudaicompanion_project,
                    managed_project_id: resp.managed_project_id,
                });
            }
            Err(err) => match err {
                // 4xx is a real auth/quota answer — don't paper over with a
                // sandbox endpoint.
                OAuthError::LoadCodeAssist { status, .. } if status >= 400 && status < 500 => {
                    return Err(err);
                }
                // Transport error or 5xx → try the next host.
                _ => last_err = Some(err),
            },
        }
    }
    Err(last_err.unwrap_or(OAuthError::MissingProject))
}

fn call_load_code_assist(
    http: &reqwest::blocking::Client,
    access_token: &str,
    endpoint_base: &str,
) -> Result<LoadCodeAssistResponse, OAuthError> {
    let url = format!("{endpoint_base}/v1internal:loadCodeAssist");
    let body = serde_json::json!({
        "metadata": {
            "ideType": "ANTIGRAVITY",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });

    let resp = http
        .post(&url)
        .bearer_auth(access_token)
        .header("User-Agent", client::USER_AGENT)
        .header("X-Goog-Api-Client", client::X_GOOG_API_CLIENT)
        .header("Client-Metadata", client::CLIENT_METADATA)
        .json(&body)
        .send()?;
    let status = resp.status();
    let bytes = resp.bytes()?;

    if !status.is_success() {
        let message = parse_error_message(&bytes);
        return Err(OAuthError::LoadCodeAssist { status: status.as_u16(), message });
    }

    let parsed: LoadCodeAssistResponse = serde_json::from_slice(&bytes)?;
    Ok(parsed)
}

fn parse_error_message(bytes: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if let Some(msg) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return msg.to_string();
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}
