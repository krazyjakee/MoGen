//! Google OAuth 2.0 (Antigravity desktop client) for the Cloud Code Assist
//! surface.
//!
//! Reference TS impl: <https://github.com/NoeFabris/opencode-antigravity-auth>.
//! We reuse the same public client_id/secret/scopes/redirect that ship in
//! Antigravity so a Pro account holder can authenticate `mogen` without a
//! billing-enabled API key. The bearer token authorises requests against
//! `cloudcode-pa.googleapis.com/v1internal:generateContent`, which is a
//! distinct API surface from the public `generativelanguage.googleapis.com`
//! key-based one — every URL/body/header is different (see [`cloudcode`]).
//!
//! Layout:
//! - [`client`]    — pinned constants (id/secret/scopes/redirect/endpoints).
//! - [`pkce`]      — PKCE S256 verifier + challenge + base64url helpers.
//! - [`server`]    — loopback HTTP server on fixed port 51121.
//! - [`flow`]      — `run_login_flow`: PKCE → browser → server → exchange.
//! - [`token`]     — `OAuthBundle` + refresh logic (60 s expiry buffer).
//! - [`store`]     — atomic on-disk save/load of `google_auth.json`.
//! - [`project`]   — `loadCodeAssist` project-id discovery + endpoint failover.
//! - [`cloudcode`] — URL/body/header construction for `v1internal`.

pub mod client;
pub mod cloudcode;
pub mod flow;
pub mod pkce;
pub mod project;
pub mod server;
pub mod store;
pub mod token;

use std::io;

use thiserror::Error;

pub use client::{
    ProviderConfig, ANTIGRAVITY_CONFIG, ANTIGRAVITY_SCOPES, GEMINI_CLI_CONFIG,
};
pub use flow::{run_login_flow, LoginOptions, LoginOutcome};
pub use store::{
    all_existing_token_paths, all_existing_token_paths_for, delete_bundle, load_bundle,
    persist_bundle, save_bundle, token_store_path, token_store_path_for,
    token_store_write_path, token_store_write_path_for, TOKEN_STORE_FILENAME,
};
pub use token::{refresh_if_needed, refresh_now, OAuthBundle, RefreshUrlOverrideGuard};

/// Unified error type for the OAuth subsystem. CLI maps to exit codes via
/// matching on the variant; library callers (Studio) use the `Display` impl.
#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("no Google credentials on disk — run `mogen auth login`")]
    MissingToken,
    #[error("loopback port 51121 is already in use; another `mogen auth login` may be running")]
    PortInUse,
    #[error("login timed out waiting for browser callback")]
    Timeout,
    #[error("login cancelled by user")]
    UserCancelled,
    #[error("OAuth state mismatch (possible CSRF) — login aborted")]
    StateMismatch,
    #[error("failed to open browser: {0}")]
    Browser(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("token endpoint returned {status}: {message}")]
    TokenExchange { status: u16, message: String },
    #[error("credentials revoked — run `mogen auth login --force`")]
    Revoked,
    #[error("loadCodeAssist failed ({status}): {message}")]
    LoadCodeAssist { status: u16, message: String },
    #[error("could not determine Google Cloud project id from loadCodeAssist response")]
    MissingProject,
    #[error("invalid callback request: {0}")]
    InvalidCallback(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("token store decode error: {0}")]
    Json(String),
}

impl From<reqwest::Error> for OAuthError {
    fn from(err: reqwest::Error) -> Self {
        OAuthError::Transport(format_source_chain(&err))
    }
}

impl From<io::Error> for OAuthError {
    fn from(err: io::Error) -> Self {
        OAuthError::Io(err.to_string())
    }
}

impl From<serde_json::Error> for OAuthError {
    fn from(err: serde_json::Error) -> Self {
        OAuthError::Json(err.to_string())
    }
}

fn format_source_chain(err: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut out = err.to_string();
    let mut src: Option<&dyn std::error::Error> = err.source();
    while let Some(e) = src {
        out.push_str(": ");
        out.push_str(&e.to_string());
        src = e.source();
    }
    out
}
