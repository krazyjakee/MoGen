//! HTTP worker that runs MoGHub calls off the egui main loop.
//!
//! Mirrors `app/llm.rs`: the UI snapshots the request inputs, spawns a
//! `std::thread`, and the worker posts results through an `mpsc` channel.
//! Each frame the app polls every active receiver via [`poll`] and
//! transitions any completed call's state. No tokio, no async fn —
//! `reqwest::blocking` is fine for the volumes Studio sends.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use mogen_moghub_client::{
    DiscoverQuery, DiscoverResponse, ModelDetail, MoghubClient, MoghubError, WhoAmI,
};

/// Outcome posted back to the UI when a call completes. One variant per
/// supported call kind. Add cases here as the Community window grows
/// (publish, comments, notifications).
pub(super) enum MoghubMessage {
    Discover(Result<DiscoverResponse, MoghubError>),
    /// `(user, slug)` echoed back so the UI can route the result to the
    /// detail panel even if the user has clicked elsewhere by the time
    /// it arrives.
    ModelDetail {
        user: String,
        slug: String,
        result: Result<ModelDetail, MoghubError>,
    },
    /// Returned source of a single file. `(user, slug, filename, body)`.
    /// Used by "Open in editor" — the body lands in a new untitled tab.
    FileSource {
        user: String,
        slug: String,
        filename: String,
        result: Result<String, MoghubError>,
    },
    /// `GET /api/whoami` result. Used after a fresh sign-in to populate
    /// the session chip and on app start to validate a stored token.
    WhoAmI(Result<WhoAmI, MoghubError>),
    /// Loopback OAuth completed. `Ok(uuid)` is the session token to
    /// store; `Err` is the textual reason the flow failed (browser
    /// closed, listener died, server returned an error).
    SignedIn(Result<String, String>),
}

/// Async handle for one in-flight call. Drop it to abandon the receiver
/// (the worker thread keeps running but nothing reads its result — fine,
/// reqwest::blocking has bounded resource use).
pub(super) struct InFlight {
    pub(super) rx: Receiver<MoghubMessage>,
}

impl InFlight {
    /// Try to drain a single completed message. `None` while the worker
    /// is still in flight; `Some(Err(_))` only if the channel closed
    /// unexpectedly (worker panicked).
    pub(super) fn try_recv(&self) -> Option<MoghubMessage> {
        match self.rx.try_recv() {
            Ok(m) => Some(m),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }
}

/// Build a fresh client for a single call. The HTTP `Client` itself is
/// cheap (it shares a connection pool internally), but constructing a
/// new one per call avoids any state we'd need to thread through the
/// worker. `base_url` comes from `Settings::moghub_url`. `token` is the
/// persisted session UUID (empty when signed-out); when present it
/// rides on every request as `Authorization: Bearer <uuid>`.
fn build_client(base_url: &str, token: &str) -> Result<MoghubClient, MoghubError> {
    let client = MoghubClient::new(base_url)?;
    if token.is_empty() {
        Ok(client)
    } else {
        Ok(client.with_token(Some(token.to_string())))
    }
}

/// Spawn a `GET /api/discover` worker. Cheap-to-construct so the UI can
/// fire a fresh one every time the user changes search/filter.
pub(super) fn fetch_discover(
    base_url: String,
    token: String,
    ctx: egui::Context,
    query: DiscoverQuery,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.discover(query));
        let _ = tx.send(MoghubMessage::Discover(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/m/:user/:slug` worker.
pub(super) fn fetch_model_detail(
    base_url: String,
    token: String,
    ctx: egui::Context,
    user: String,
    slug: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let user_for_msg = user.clone();
    let slug_for_msg = slug.clone();
    thread::spawn(move || {
        let result =
            build_client(&base_url, &token).and_then(|c| c.model_detail(&user, &slug));
        let _ = tx.send(MoghubMessage::ModelDetail {
            user: user_for_msg,
            slug: slug_for_msg,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/m/:user/:slug/files/:filename` worker. Used by the
/// "Open in editor" action to load a published source into a fresh tab.
pub(super) fn fetch_file_source(
    base_url: String,
    token: String,
    ctx: egui::Context,
    user: String,
    slug: String,
    filename: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let user_for_msg = user.clone();
    let slug_for_msg = slug.clone();
    let filename_for_msg = filename.clone();
    thread::spawn(move || {
        let result = build_client(&base_url, &token)
            .and_then(|c| c.file_raw(&user, &slug, &filename));
        let _ = tx.send(MoghubMessage::FileSource {
            user: user_for_msg,
            slug: slug_for_msg,
            filename: filename_for_msg,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/whoami` worker. Used on startup to validate a
/// stored session and after a fresh sign-in to populate the chip.
pub(super) fn fetch_whoami(base_url: String, token: String, ctx: egui::Context) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.whoami());
        let _ = tx.send(MoghubMessage::WhoAmI(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn the loopback OAuth flow. Binds a one-shot HTTP listener on
/// `127.0.0.1:0` (OS-assigned port), opens the user's default browser
/// at `<base_url>/api/auth/desktop/start?return=…&nonce=…`, then waits
/// up to [`OAUTH_TIMEOUT`] for GitHub's callback to redirect back to
/// `http://127.0.0.1:<port>/callback?session=<uuid>&nonce=<echoed>`.
/// On success [`MoghubMessage::SignedIn`] carries the session UUID; on
/// failure it carries a human-readable reason.
pub(super) fn start_signin(base_url: String, ctx: egui::Context) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = run_signin_flow(&base_url);
        let _ = tx.send(MoghubMessage::SignedIn(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Upper bound on how long the loopback listener waits for GitHub to
/// redirect the browser back. Longer than the typical OAuth round-trip
/// (the user might have to log in to GitHub or pick an account), short
/// enough that an abandoned flow eventually frees the port.
const OAUTH_TIMEOUT: Duration = Duration::from_secs(300);

/// Drive the loopback OAuth flow synchronously inside the worker
/// thread. Returns the session UUID on success, or a textual reason
/// the flow failed.
fn run_signin_flow(base_url: &str) -> Result<String, String> {
    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|e| format!("could not bind loopback listener: {e}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| "loopback listener missing port".to_string())?
        .port();
    let nonce = generate_nonce();
    let return_url = format!("http://127.0.0.1:{port}/callback");
    let start_url = format!(
        "{}/api/auth/desktop/start?return={}&nonce={}",
        base_url.trim_end_matches('/'),
        urlencode(&return_url),
        urlencode(&nonce),
    );

    if let Err(e) = webbrowser::open(&start_url) {
        return Err(format!("could not open browser: {e}"));
    }

    // Poll the listener so a user who closes the browser tab eventually
    // hits OAUTH_TIMEOUT instead of blocking forever. Spurious requests
    // (favicon.ico, second tab refreshes) get a 404 and the loop keeps
    // waiting for a /callback with the matching nonce.
    let started = Instant::now();
    while started.elapsed() < OAUTH_TIMEOUT {
        match server.recv_timeout(Duration::from_millis(500)) {
            Ok(Some(req)) => {
                let url = req.url().to_string();
                let path = url.split('?').next().unwrap_or("");
                if path != "/callback" {
                    let _ = req.respond(tiny_http::Response::empty(404));
                    continue;
                }
                let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
                let params = parse_query(query);
                let echoed_nonce = params.iter().find(|(k, _)| k == "nonce").map(|(_, v)| v.as_str());
                if echoed_nonce != Some(nonce.as_str()) {
                    let _ = req.respond(success_response(
                        "Sign-in failed: nonce mismatch. You can close this window and try again.",
                    ));
                    return Err("loopback callback nonce mismatch".to_string());
                }
                let session = params.iter().find(|(k, _)| k == "session").map(|(_, v)| v.clone());
                let Some(session) = session else {
                    let _ = req.respond(success_response(
                        "Sign-in failed: server did not return a session token.",
                    ));
                    return Err("server did not return a session token".to_string());
                };
                let _ = req.respond(success_response(
                    "Signed in to MoGHub. You can close this window and return to MoGen Studio.",
                ));
                return Ok(session);
            }
            Ok(None) => continue,
            Err(e) => return Err(format!("loopback listener error: {e}")),
        }
    }
    Err("sign-in timed out waiting for the browser callback".to_string())
}

fn success_response(msg: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MoGen Studio sign-in</title>\
         <style>body{{font:14px system-ui,sans-serif;padding:48px;color:#222}}\
         .card{{max-width:520px;margin:0 auto;padding:24px;border:1px solid #ddd;border-radius:8px}}</style>\
         </head><body><div class=\"card\"><h1>MoGen Studio</h1><p>{msg}</p></div></body></html>",
        msg = html_escape(msg),
    );
    tiny_http::Response::from_string(body)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .expect("static header"),
        )
}

/// 16 random URL-safe characters, enough entropy for the nonce's only
/// job (matching the listener to the GitHub callback that came back).
fn generate_nonce() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Mix process id, time-since-epoch nanoseconds, and a stack address
    // hash. Not cryptographically strong — that's fine: the nonce only
    // has to be unguessable to a co-tenant local process within the
    // OAUTH_TIMEOUT window. The server validates `[A-Za-z0-9_-]{8,128}`.
    let mut seed = std::process::id() as u64;
    if let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) {
        seed ^= d.as_nanos() as u64;
    }
    let stack_var = 0u8;
    seed ^= (&stack_var as *const _ as usize) as u64;
    let alphabet: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
    let mut out = String::with_capacity(16);
    let mut state = seed.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1);
    for _ in 0..16 {
        // splitmix64
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        out.push(alphabet[(z as usize) % alphabet.len()] as char);
    }
    out
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (urldecode(k), urldecode(v)))
        .collect()
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_nibble(bytes[i + 1]);
                let lo = hex_nibble(bytes[i + 2]);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h << 4) | l);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_is_safe_alphabet_and_length() {
        let n = generate_nonce();
        assert_eq!(n.len(), 16);
        assert!(n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }

    #[test]
    fn parse_query_handles_session_and_nonce() {
        let q = "session=abc-123&nonce=Z9_q";
        let kv = parse_query(q);
        assert_eq!(kv.len(), 2);
        assert_eq!(kv[0], ("session".into(), "abc-123".into()));
        assert_eq!(kv[1], ("nonce".into(), "Z9_q".into()));
    }

    #[test]
    fn urlencode_round_trips_safe_chars() {
        let original = "abc-_.~";
        assert_eq!(urlencode(original), original);
        assert_eq!(urldecode(original), original);
    }

    #[test]
    fn urlencode_escapes_unsafe_chars() {
        assert_eq!(urlencode("a b/c"), "a%20b%2Fc");
        assert_eq!(urldecode("a%20b%2Fc"), "a b/c");
    }

    #[test]
    fn urldecode_handles_plus_as_space() {
        assert_eq!(urldecode("a+b"), "a b");
    }

    #[test]
    fn html_escape_blocks_tag_injection() {
        assert_eq!(
            html_escape("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
    }
}
