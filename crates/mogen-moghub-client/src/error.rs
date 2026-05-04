use thiserror::Error;

/// All errors the client can produce. Maps cleanly to UI banners in
/// Studio (network failure → "couldn't reach moghub", `Unauthorized` →
/// "sign in to continue", `Status` → server-side error with body).
#[derive(Debug, Error)]
pub enum MoghubError {
    /// Transport / TLS / DNS failure. The user is offline or the
    /// configured `moghub_url` is wrong.
    #[error("network error: {0}")]
    Network(String),

    /// HTTP 4xx/5xx with a server-supplied body. Surface the body to
    /// the UI verbatim; the moghub server returns useful structured
    /// messages on validation failures.
    #[error("server returned {code}: {body}")]
    Status { code: u16, body: String },

    /// HTTP 401 specifically — token missing or expired. UI flips the
    /// sign-in chip back to "sign in" and clears the keychain entry.
    #[error("authentication required")]
    Unauthorized,

    /// JSON parse failure or URL-construction error. Should be rare;
    /// when it fires, it's almost always a moghub schema mismatch (the
    /// server shipped a new field the desktop client doesn't know
    /// about).
    #[error("decode error: {0}")]
    Decode(String),
}

impl MoghubError {
    pub(crate) fn network(e: reqwest::Error) -> Self {
        MoghubError::Network(e.to_string())
    }

    pub(crate) fn status(code: u16, body: String) -> Self {
        if code == 401 {
            MoghubError::Unauthorized
        } else {
            MoghubError::Status { code, body }
        }
    }
}
