//! Loopback callback server.
//!
//! Binds 127.0.0.1:51121 (port fixed by Google's OAuth client config),
//! waits for one request to `/oauth-callback?code=...&state=...`, returns a
//! plain-HTML success page, and shuts the server down. Errors and the
//! `?error=...` consent-denied case are surfaced via `OAuthError` so the
//! caller can exit with a meaningful code.

use std::io;
use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use tiny_http::{Header, Response, Server};

use super::client;
use super::OAuthError;

/// Result of a successful callback.
#[derive(Debug, Clone)]
pub struct Callback {
    pub code: String,
    pub state: String,
}

/// HTML returned to the browser once the callback fires successfully.
const SUCCESS_HTML: &str = "<!doctype html>\n\
<html><head><meta charset=\"utf-8\"><title>MoGen — signed in</title>\n\
<style>body{background:#0e1116;color:#e6edf3;font-family:system-ui,sans-serif;\
display:flex;align-items:center;justify-content:center;height:100vh;margin:0;}\
.card{padding:2rem 3rem;border:1px solid #30363d;border-radius:8px;\
background:#161b22;text-align:center;}h1{margin:0 0 .5rem 0;font-size:1.4rem;}\
p{margin:0;color:#8b949e;font-size:.95rem;}</style></head>\n\
<body><div class=\"card\"><h1>Signed in to MoGen</h1>\
<p>You can close this tab and return to the terminal.</p></div></body></html>";

/// HTML returned when the consent flow is denied or carries an error param.
const ERROR_HTML: &str = "<!doctype html>\n\
<html><head><meta charset=\"utf-8\"><title>MoGen — sign-in cancelled</title>\n\
<style>body{background:#0e1116;color:#e6edf3;font-family:system-ui,sans-serif;\
display:flex;align-items:center;justify-content:center;height:100vh;margin:0;}\
.card{padding:2rem 3rem;border:1px solid #30363d;border-radius:8px;\
background:#161b22;text-align:center;}h1{margin:0 0 .5rem 0;font-size:1.4rem;}\
p{margin:0;color:#8b949e;font-size:.95rem;}</style></head>\n\
<body><div class=\"card\"><h1>Sign-in cancelled</h1>\
<p>You can close this tab and re-run <code>mogen auth login</code>.</p></div></body></html>";

/// Bind the loopback server. Returns `OAuthError::PortInUse` if the fixed
/// port is taken — there is nothing the caller can do but ask the user to
/// release it.
pub fn bind() -> Result<Server, OAuthError> {
    let addr: SocketAddr = ([127u8, 0, 0, 1], client::REDIRECT_PORT).into();
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => return Err(OAuthError::PortInUse),
        Err(err) => return Err(OAuthError::Io(err.to_string())),
    };
    Server::from_listener(listener, None)
        .map_err(|err| OAuthError::Io(format!("failed to start callback server: {err}")))
}

/// Wait for the OAuth callback. Polls the server until either a callback
/// arrives, the client disconnects with an `error=` param, or `timeout`
/// elapses. The server is consumed (dropped on return), releasing the port.
pub fn wait_for_callback(server: Server, timeout: Duration) -> Result<Callback, OAuthError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.checked_duration_since(std::time::Instant::now());
        let Some(remaining) = remaining else {
            return Err(OAuthError::Timeout);
        };
        // Poll in slices so we can cleanly bail on overall deadline.
        let slice = remaining.min(Duration::from_secs(1));
        match server.recv_timeout(slice) {
            Ok(Some(req)) => {
                let url = req.url().to_string();
                let outcome = parse_callback_url(&url);
                match outcome {
                    CallbackParse::Hit(cb) => {
                        respond_html(req, 200, SUCCESS_HTML);
                        return Ok(cb);
                    }
                    CallbackParse::Error(reason) => {
                        respond_html(req, 200, ERROR_HTML);
                        eprintln!("login cancelled by Google: {reason}");
                        return Err(OAuthError::UserCancelled);
                    }
                    CallbackParse::NotCallback => {
                        // Browsers occasionally fetch /favicon.ico — ignore
                        // anything that isn't our redirect path.
                        let _ = req.respond(Response::empty(404));
                    }
                    CallbackParse::Malformed(reason) => {
                        respond_html(req, 400, ERROR_HTML);
                        return Err(OAuthError::InvalidCallback(reason));
                    }
                }
            }
            Ok(None) => continue, // slice elapsed without a request
            Err(err) => return Err(OAuthError::Io(format!("callback recv: {err}"))),
        }
    }
}

fn respond_html(req: tiny_http::Request, status: u16, body: &str) {
    let header = Header::from_bytes(b"Content-Type"[..].to_vec(), b"text/html; charset=utf-8"[..].to_vec())
        .expect("static header");
    let resp = Response::from_string(body)
        .with_status_code(status)
        .with_header(header);
    let _ = req.respond(resp);
}

#[derive(Debug)]
enum CallbackParse {
    Hit(Callback),
    Error(String),
    NotCallback,
    Malformed(String),
}

/// Parse a relative request URL like `/oauth-callback?code=...&state=...`.
/// Returns `NotCallback` for non-matching paths (so the server can keep
/// listening for the real one) and `Malformed` for missing required params.
fn parse_callback_url(url: &str) -> CallbackParse {
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    };
    if path != client::REDIRECT_PATH {
        return CallbackParse::NotCallback;
    }
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut error: Option<String> = None;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => (pair, ""),
        };
        let v = percent_decode(v);
        match k {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "error" => error = Some(v),
            _ => {}
        }
    }
    if let Some(err) = error {
        return CallbackParse::Error(err);
    }
    match (code, state) {
        (Some(code), Some(state)) => CallbackParse::Hit(Callback { code, state }),
        (None, _) => CallbackParse::Malformed("missing `code` parameter".into()),
        (_, None) => CallbackParse::Malformed("missing `state` parameter".into()),
    }
}

/// Minimal percent-decoder: handles `%XX` escapes and `+ → space`. The
/// callback values are short and ASCII-flavoured (Google ids and base64url
/// state) so we keep this self-contained instead of pulling in a percent
/// crate.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
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

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + b - b'a'),
        b'A'..=b'F' => Some(10 + b - b'A'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_happy_callback() {
        let url = "/oauth-callback?code=abc123&state=ff00";
        match parse_callback_url(url) {
            CallbackParse::Hit(cb) => {
                assert_eq!(cb.code, "abc123");
                assert_eq!(cb.state, "ff00");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_error_param() {
        let url = "/oauth-callback?error=access_denied";
        match parse_callback_url(url) {
            CallbackParse::Error(e) => assert_eq!(e, "access_denied"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_other_paths_as_not_callback() {
        match parse_callback_url("/favicon.ico") {
            CallbackParse::NotCallback => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn missing_code_is_malformed() {
        match parse_callback_url("/oauth-callback?state=x") {
            CallbackParse::Malformed(_) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn percent_decodes_state_with_escaped_chars() {
        let url = "/oauth-callback?code=a%2Fb&state=hello%20world";
        match parse_callback_url(url) {
            CallbackParse::Hit(cb) => {
                assert_eq!(cb.code, "a/b");
                assert_eq!(cb.state, "hello world");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
