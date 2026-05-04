//! Mock-server tests for `refresh_against` — the token-endpoint round-trip
//! that turns an `OAuthBundle`'s refresh token into a fresh access token.
//!
//! `refresh_against` is the test seam exposed by `google_oauth::token`; it
//! takes a `token_url` so the test can stand up a `tiny_http` server in
//! place of `https://oauth2.googleapis.com/token`. The cases below cover
//! the two outcomes the CLI cares about:
//!
//! 1. 200 OK with `{access_token, expires_in, refresh_token, scope}` →
//!    bundle is updated in place; rotated refresh token replaces the old
//!    one when the server provides one.
//! 2. 4xx with `error=invalid_grant` → mapped to `OAuthError::Revoked` so
//!    the CLI surfaces a "credentials revoked, run `mogen auth login
//!    --force`" message.
//!
//! `refresh_against` uses the bundled
//! `google_oauth::client::CLIENT_ID`/`CLIENT_SECRET` constants directly, so
//! tests don't need to stage an override file or touch env vars — the mock
//! server doesn't validate form-field values, only that the right keys are
//! present.

use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::thread;

use mogen_llm::google_oauth::token::refresh_against;
use mogen_llm::{OAuthBundle, OAuthError};

struct TokenServer {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    _handle: thread::JoinHandle<()>,
}

impl TokenServer {
    fn start(status: u16, body: &'static str) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
        let port = server.server_addr().to_ip().expect("ipv4").port();
        let base = format!("http://127.0.0.1:{port}/token");

        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let requests_clone = requests.clone();

        let handle = thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut buf = String::new();
                req.as_reader().read_to_string(&mut buf).ok();
                requests_clone.lock().unwrap().push(buf);

                let resp = tiny_http::Response::from_string(body)
                    .with_status_code(status)
                    .with_header(
                        "Content-Type: application/json"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    );
                let _ = req.respond(resp);
            }
        });

        Self { base, requests, _handle: handle }
    }
}

fn fixture(refresh_token: &str) -> OAuthBundle {
    OAuthBundle {
        access_token: "old-access".into(),
        refresh_token: refresh_token.into(),
        access_expires_at_unix: 0,
        obtained_at_unix: 0,
        email: None,
        project_id: None,
        managed_project_id: None,
        endpoint_base: None,
        scope: None,
    }
}

#[test]
fn test_refresh_against_200_response_updates_access_token_and_expiry() {
    // Arrange: server returns a fresh access token + 3600s lifetime + a
    // rotated refresh token + scope echo.
    let body = r#"{
        "access_token": "ya29.NEW",
        "refresh_token": "1//ROTATED",
        "expires_in": 3600,
        "token_type": "Bearer",
        "scope": "https://www.googleapis.com/auth/cloud-platform"
    }"#;
    let server = TokenServer::start(200, body);
    let http = reqwest::blocking::Client::new();
    let mut bundle = fixture("1//ORIGINAL");
    let now: u64 = 1_700_000_000;

    // Act
    refresh_against(&http, &mut bundle, now, &server.base).expect("refresh ok");

    // Assert: bundle mutated in place — new access token, server-rotated
    // refresh token, expiry computed as now + expires_in, scope echoed,
    // obtained_at stamped.
    assert_eq!(bundle.access_token, "ya29.NEW");
    assert_eq!(bundle.refresh_token, "1//ROTATED");
    assert_eq!(bundle.access_expires_at_unix, now + 3600);
    assert_eq!(bundle.obtained_at_unix, now);
    assert_eq!(
        bundle.scope.as_deref(),
        Some("https://www.googleapis.com/auth/cloud-platform")
    );

    // The form body must carry grant_type=refresh_token + client creds + the
    // existing refresh token — Google's token endpoint requires all four.
    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let form = &reqs[0];
    assert!(form.contains("grant_type=refresh_token"), "got: {form}");
    assert!(form.contains("refresh_token=1%2F%2FORIGINAL"), "got: {form}");
    assert!(form.contains("client_id="), "got: {form}");
    assert!(form.contains("client_secret="), "got: {form}");
}

#[test]
fn test_refresh_against_200_response_keeps_existing_refresh_token_when_omitted() {
    // Some 200 responses do not rotate the refresh token. The bundle's
    // existing `refresh_token` must survive the refresh.
    let body = r#"{
        "access_token": "ya29.NEW",
        "expires_in": 3600,
        "token_type": "Bearer"
    }"#;
    let server = TokenServer::start(200, body);
    let http = reqwest::blocking::Client::new();
    let mut bundle = fixture("1//KEEP");

    refresh_against(&http, &mut bundle, 1_700_000_000, &server.base)
        .expect("refresh ok");

    assert_eq!(bundle.access_token, "ya29.NEW");
    assert_eq!(bundle.refresh_token, "1//KEEP", "refresh token must be preserved");
}

#[test]
fn test_refresh_against_invalid_grant_response_maps_to_revoked() {
    // Arrange: server returns the canonical revoked-credential body.
    let body = r#"{
        "error": "invalid_grant",
        "error_description": "Token has been expired or revoked."
    }"#;
    let server = TokenServer::start(400, body);
    let http = reqwest::blocking::Client::new();
    let mut bundle = fixture("1//REVOKED");

    // Act
    let err = refresh_against(&http, &mut bundle, 1_700_000_000, &server.base)
        .expect_err("revoked refresh must fail");

    // Assert: maps to OAuthError::Revoked so the CLI surfaces "run mogen
    // auth login --force" rather than a generic transport error.
    assert!(matches!(err, OAuthError::Revoked), "got: {err:?}");
    // The bundle must NOT be partially mutated when refresh fails.
    assert_eq!(bundle.access_token, "old-access");
}

#[test]
fn test_refresh_against_other_4xx_response_propagates_status_and_message() {
    // Arrange: a non-`invalid_grant` 4xx (e.g. quota exhaustion or rate
    // limit) must propagate as TokenExchange so the CLI shows the upstream
    // status + message instead of misleadingly claiming "revoked".
    let body = r#"{"error":"rate_limit_exceeded","error_description":"slow down"}"#;
    let server = TokenServer::start(429, body);
    let http = reqwest::blocking::Client::new();
    let mut bundle = fixture("1//RT");

    let err = refresh_against(&http, &mut bundle, 0, &server.base)
        .expect_err("4xx must error");

    match err {
        OAuthError::TokenExchange { status, message } => {
            assert_eq!(status, 429);
            assert!(message.contains("slow down"), "got: {message}");
        }
        other => panic!("expected TokenExchange, got {other:?}"),
    }
}
