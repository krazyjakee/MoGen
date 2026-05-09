//! Loopback OAuth flow shared between Studio and the `mogen` CLI.
//!
//! The desktop's auth contract: bind a one-shot HTTP listener on
//! `127.0.0.1:<random>`, generate an unguessable nonce, open the user's
//! default browser at `<base_url>/api/auth/desktop/start?return=…&nonce=…`,
//! and wait for GitHub's redirect to land back at the loopback with
//! `?session=<uuid>&nonce=<echoed>`. The matching nonce is what stops
//! a co-tenant local process from snatching the redirect.
//!
//! The flow is synchronous and blocking — Studio dispatches it on a
//! `std::thread` worker via `app/moghub.rs`, the CLI runs it inline in
//! `mogen login`. The function takes no UI handles so both consumers
//! share the same body.

use std::time::{Duration, Instant};

/// Upper bound on how long the loopback listener waits for GitHub to
/// redirect the browser back. Long enough that a user picking a
/// GitHub account or completing 2FA doesn't time out; short enough
/// that an abandoned flow eventually frees the port.
pub const OAUTH_TIMEOUT: Duration = Duration::from_secs(300);

/// Run the loopback OAuth flow against `base_url`, blocking until
/// the flow completes or [`OAUTH_TIMEOUT`] elapses. On success the
/// returned `String` is the session UUID — caller persists it via
/// keyring / settings / wherever, then sends it as
/// `Authorization: Bearer <uuid>` on every authenticated request.
pub fn run_loopback_flow(base_url: &str) -> Result<String, String> {
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
        // Log the URL to stderr too so a user whose browser launcher is
        // broken (e.g. headless dev environment) can copy it manually
        // instead of being stuck on a generic "could not open browser"
        // banner with no URL to paste.
        eprintln!("oauth: could not auto-open browser ({e}). Open this URL manually to continue:\n  {start_url}");
        return Err(format!(
            "could not open browser ({e}). See the terminal for a URL you can paste \
             into a browser manually."
        ));
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
                let echoed_nonce = params
                    .iter()
                    .find(|(k, _)| k == "nonce")
                    .map(|(_, v)| v.as_str());
                if echoed_nonce != Some(nonce.as_str()) {
                    let _ = req.respond(success_response(
                        "Sign-in failed: nonce mismatch. You can close this window and try again.",
                    ));
                    return Err("loopback callback nonce mismatch".to_string());
                }
                let session = params
                    .iter()
                    .find(|(k, _)| k == "session")
                    .map(|(_, v)| v.clone());
                let Some(session) = session else {
                    let _ = req.respond(success_response(
                        "Sign-in failed: server did not return a session token.",
                    ));
                    return Err("server did not return a session token".to_string());
                };
                let _ = req.respond(success_response(
                    "Signed in to MoGHub. You can close this window and return to your terminal.",
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
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MoGHub sign-in</title>\
         <style>body{{font:14px system-ui,sans-serif;padding:48px;color:#222}}\
         .card{{max-width:520px;margin:0 auto;padding:24px;border:1px solid #ddd;border-radius:8px}}</style>\
         </head><body><div class=\"card\"><h1>MoGHub</h1><p>{msg}</p></div></body></html>",
        msg = html_escape(msg),
    );
    tiny_http::Response::from_string(body).with_header(
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .expect("static header"),
    )
}

/// 16 random URL-safe characters. Not cryptographically strong — the
/// nonce only has to be unguessable to a co-tenant local process
/// within the OAUTH_TIMEOUT window. The server validates
/// `[A-Za-z0-9_-]{8,128}`.
fn generate_nonce() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut seed = std::process::id() as u64;
    if let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) {
        seed ^= d.as_nanos() as u64;
    }
    let stack_var = 0u8;
    seed ^= (&stack_var as *const _ as usize) as u64;
    let alphabet: &[u8] =
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
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
        assert!(n
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
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
