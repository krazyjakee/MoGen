//! Remote-control web UI server.
//!
//! When enabled in Preferences › Remote, Studio runs a tiny embedded HTTP
//! server (tiny_http, one background thread) that serves a self-contained
//! browser dashboard mirroring the live session — open tabs, active source,
//! diagnostics, scene stats, and a periodically refreshed viewport preview —
//! and accepts a small command set (activate tab / edit source / save /
//! build / recompile) that the app drains on its UI thread once per frame.
//!
//! Threading model: the HTTP thread never touches app state directly. All
//! communication goes through [`Shared`] behind one mutex — the UI thread
//! *publishes* a pre-serialized JSON snapshot whenever the observable state
//! changes, and *drains* the command queue the HTTP thread appends to. The
//! HTTP thread nudges `egui::Context::request_repaint` after enqueueing a
//! command so the app reacts promptly even when idle.
//!
//! Security: the server binds loopback by default; binding `0.0.0.0` (so
//! phones/tablets on the LAN can drive Studio) is a separate opt-in in
//! Preferences, because anyone who can reach the port can edit and save the
//! open files. No TLS, no auth — this is a local-tooling convenience in the
//! same spirit as every dev-server, not an internet-facing surface.

use std::collections::VecDeque;
use std::io::Read;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};

/// The dashboard, embedded so the binary stays self-contained (same pattern
/// as the splash image and fonts).
const REMOTE_UI_HTML: &str = include_str!("../assets/remote/index.html");

/// Hard cap on POST bodies. A `.mog` source is KBs; 4 MiB leaves generous
/// headroom while keeping a hostile client from ballooning memory.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// How long after the last `/api/preview.png` hit the app keeps rendering
/// fresh preview captures. Browsers poll every ~1.5s while the page is
/// visible, so this comfortably covers a live viewer and stops capture work
/// shortly after the last tab closes.
const PREVIEW_WANTED_WINDOW: Duration = Duration::from_secs(10);

/// One remote command, appended by the HTTP thread and applied by the app
/// on its UI thread. Tab indices are validated at apply time — tabs can
/// open/close between enqueue and drain.
pub enum RemoteCommand {
    ActivateTab(usize),
    /// Replace a tab's source buffer (recompiles via the normal debounce
    /// path and records an undo entry, exactly like an in-app edit).
    SetSource { tab: usize, source: String },
    Save { tab: usize },
    /// Export the tab's scene to a GLB next to the file, using the tab's
    /// remembered export options.
    Build { tab: usize },
    Recompile { tab: usize },
}

/// Wire format for `POST /api/command`.
#[derive(Deserialize)]
struct CommandBody {
    cmd: String,
    #[serde(default)]
    tab: Option<usize>,
    #[serde(default)]
    source: Option<String>,
}

/// One open tab as shown in the dashboard's tab strip.
#[derive(Serialize, Clone, PartialEq)]
pub struct TabInfo {
    pub name: String,
    pub path: Option<String>,
    pub dirty: bool,
}

/// One diagnostic row. `line` is 1-based, derived from the span so the web
/// editor can badge the offending line.
#[derive(Serialize, Clone, PartialEq)]
pub struct DiagInfo {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub line: Option<usize>,
}

/// Everything the dashboard renders, published as one JSON blob. The app
/// rebuilds this each frame and re-serializes only when it actually changed
/// (`PartialEq` against the previous snapshot), so idle frames cost one
/// cheap comparison.
#[derive(Serialize, Clone, PartialEq)]
pub struct Snapshot {
    pub app_version: String,
    pub tabs: Vec<TabInfo>,
    pub active: usize,
    /// Active tab's full source. The dashboard editor is view + replace —
    /// fine for `.mog` files, which are hand-sized by design.
    pub source: String,
    pub status: String,
    /// Compile stage of the active tab: `ok`, `parse`, `validate-ast`,
    /// `lower`, `validate-graph`, or `none` before the first compile.
    pub stage: String,
    pub diagnostics: Vec<DiagInfo>,
    pub nodes: usize,
    pub triangles: usize,
    pub building: bool,
    pub build_stage: String,
    /// Label of the in-flight LLM call on the active tab, if any.
    pub llm: Option<String>,
}

/// State shared between the UI thread and the HTTP thread.
struct Shared {
    snapshot_json: String,
    rev: u64,
    preview_png: Vec<u8>,
    preview_rev: u64,
    /// Last time a client asked for the preview image. Drives the app-side
    /// decision to keep submitting viewport captures.
    preview_wanted_at: Option<Instant>,
    commands: VecDeque<RemoteCommand>,
}

/// Handle owned by the app. Dropping it shuts the server down (the worker
/// notices `shutdown` within one accept-timeout slice and exits).
pub struct RemoteServer {
    shared: Arc<Mutex<Shared>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    port: u16,
    allow_lan: bool,
}

impl RemoteServer {
    /// Bind and start serving. Returns a human-readable error when the port
    /// is taken (the common failure) so Preferences can show it verbatim.
    pub fn start(port: u16, allow_lan: bool, egui_ctx: egui::Context) -> Result<Self, String> {
        let host = if allow_lan { "0.0.0.0" } else { "127.0.0.1" };
        let listener = TcpListener::bind((host, port)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("port {port} is already in use — pick another port")
            } else {
                format!("bind {host}:{port}: {e}")
            }
        })?;
        let server = tiny_http::Server::from_listener(listener, None)
            .map_err(|e| format!("start server on {host}:{port}: {e}"))?;

        let shared = Arc::new(Mutex::new(Shared {
            snapshot_json: "null".to_string(),
            rev: 0,
            preview_png: Vec::new(),
            preview_rev: 0,
            preview_wanted_at: None,
            commands: VecDeque::new(),
        }));
        let shutdown = Arc::new(AtomicBool::new(false));

        let worker_shared = Arc::clone(&shared);
        let worker_shutdown = Arc::clone(&shutdown);
        let handle = std::thread::Builder::new()
            .name("mogen-remote-http".into())
            .spawn(move || {
                serve_loop(server, worker_shared, worker_shutdown, egui_ctx);
            })
            .map_err(|e| format!("spawn remote server thread: {e}"))?;

        Ok(Self {
            shared,
            shutdown,
            handle: Some(handle),
            port,
            allow_lan,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn allow_lan(&self) -> bool {
        self.allow_lan
    }

    /// Browser-reachable URL for the local machine.
    pub fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    /// Replace the published snapshot. Callers should only invoke this when
    /// the snapshot actually changed — the revision bump is what tells
    /// polling clients to re-render.
    pub fn publish_state(&self, snapshot: &Snapshot) {
        let json = serde_json::to_string(snapshot)
            .unwrap_or_else(|e| format!("{{\"error\":\"serialize: {e}\"}}"));
        let mut sh = self.shared.lock().unwrap();
        sh.snapshot_json = json;
        sh.rev = sh.rev.wrapping_add(1);
    }

    /// Replace the preview PNG served at `/api/preview.png`.
    pub fn publish_preview(&self, png: Vec<u8>) {
        let mut sh = self.shared.lock().unwrap();
        sh.preview_png = png;
        sh.preview_rev = sh.preview_rev.wrapping_add(1);
    }

    /// Whether any browser asked for the preview image recently. The app
    /// only spends GL time on remote captures while this is true.
    pub fn preview_watchers_active(&self) -> bool {
        let sh = self.shared.lock().unwrap();
        sh.preview_wanted_at
            .map(|t| t.elapsed() < PREVIEW_WANTED_WINDOW)
            .unwrap_or(false)
    }

    /// Drain every queued command, oldest first.
    pub fn take_commands(&self) -> Vec<RemoteCommand> {
        let mut sh = self.shared.lock().unwrap();
        sh.commands.drain(..).collect()
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            // Worker wakes from `recv_timeout` within one slice and sees the
            // flag; join is bounded in practice.
            let _ = handle.join();
        }
    }
}

/// Accept loop. Runs until the shutdown flag flips.
fn serve_loop(
    server: tiny_http::Server,
    shared: Arc<Mutex<Shared>>,
    shutdown: Arc<AtomicBool>,
    egui_ctx: egui::Context,
) {
    while !shutdown.load(Ordering::SeqCst) {
        match server.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(req)) => handle_request(req, &shared, &egui_ctx),
            Ok(None) => continue,
            Err(_) => break,
        }
    }
}

fn handle_request(
    mut req: tiny_http::Request,
    shared: &Arc<Mutex<Shared>>,
    egui_ctx: &egui::Context,
) {
    let url = req.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url.as_str(), ""),
    };
    let method = req.method().clone();

    match (method, path) {
        (tiny_http::Method::Get, "/") | (tiny_http::Method::Get, "/index.html") => {
            respond(req, 200, "text/html; charset=utf-8", REMOTE_UI_HTML.as_bytes());
        }
        (tiny_http::Method::Get, "/api/state") => {
            let since = query_u64(query, "rev");
            let (rev, preview_rev, body) = {
                let sh = shared.lock().unwrap();
                (sh.rev, sh.preview_rev, sh.snapshot_json.clone())
            };
            // Skip re-sending an unchanged snapshot: the client passes the
            // rev it already has and we answer with a slim marker. The
            // preview rev always rides along so the <img> refresh loop can
            // key off it without a second endpoint.
            let json = if since == Some(rev) {
                format!("{{\"rev\":{rev},\"preview\":{preview_rev},\"unchanged\":true}}")
            } else {
                format!("{{\"rev\":{rev},\"preview\":{preview_rev},\"state\":{body}}}")
            };
            respond(req, 200, "application/json", json.as_bytes());
        }
        (tiny_http::Method::Get, "/api/preview.png") => {
            let png = {
                let mut sh = shared.lock().unwrap();
                sh.preview_wanted_at = Some(Instant::now());
                sh.preview_png.clone()
            };
            // Nudge the app so the capture loop starts promptly on the very
            // first request instead of waiting for the next natural repaint.
            egui_ctx.request_repaint();
            if png.is_empty() {
                respond(req, 404, "text/plain", b"no preview rendered yet");
            } else {
                respond(req, 200, "image/png", &png);
            }
        }
        (tiny_http::Method::Post, "/api/command") => {
            let mut body = Vec::new();
            let read = req
                .as_reader()
                .take(MAX_BODY_BYTES as u64 + 1)
                .read_to_end(&mut body);
            if read.is_err() || body.len() > MAX_BODY_BYTES {
                respond(req, 413, "application/json", b"{\"error\":\"body too large\"}");
                return;
            }
            match parse_command(&body) {
                Ok(cmd) => {
                    {
                        let mut sh = shared.lock().unwrap();
                        sh.commands.push_back(cmd);
                    }
                    egui_ctx.request_repaint();
                    respond(req, 200, "application/json", b"{\"ok\":true}");
                }
                Err(msg) => {
                    let json = serde_json::json!({ "error": msg }).to_string();
                    respond(req, 400, "application/json", json.as_bytes());
                }
            }
        }
        _ => respond(req, 404, "text/plain", b"not found"),
    }
}

fn parse_command(body: &[u8]) -> Result<RemoteCommand, String> {
    let parsed: CommandBody =
        serde_json::from_slice(body).map_err(|e| format!("bad command JSON: {e}"))?;
    let tab = || parsed.tab.ok_or_else(|| "missing `tab`".to_string());
    match parsed.cmd.as_str() {
        "activate" => Ok(RemoteCommand::ActivateTab(tab()?)),
        "set_source" => Ok(RemoteCommand::SetSource {
            tab: tab()?,
            source: parsed.source.ok_or_else(|| "missing `source`".to_string())?,
        }),
        "save" => Ok(RemoteCommand::Save { tab: tab()? }),
        "build" => Ok(RemoteCommand::Build { tab: tab()? }),
        "recompile" => Ok(RemoteCommand::Recompile { tab: tab()? }),
        other => Err(format!("unknown command `{other}`")),
    }
}

fn respond(req: tiny_http::Request, status: u16, content_type: &str, body: &[u8]) {
    let ct = tiny_http::Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
        .expect("static header");
    // Everything here is live state — a cached snapshot or preview is
    // strictly worse than a re-fetch on a loopback link.
    let cc = tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..])
        .expect("static header");
    let resp = tiny_http::Response::from_data(body)
        .with_status_code(status)
        .with_header(ct)
        .with_header(cc);
    let _ = req.respond(resp);
}

/// Pull a `u64` query value (`rev=42`) out of a raw query string.
fn query_u64(query: &str, key: &str) -> Option<u64> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return v.parse().ok();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_set_source_command() {
        let body = br#"{"cmd":"set_source","tab":1,"source":"scene {}"}"#;
        match parse_command(body).unwrap() {
            RemoteCommand::SetSource { tab, source } => {
                assert_eq!(tab, 1);
                assert_eq!(source, "scene {}");
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn rejects_unknown_command() {
        assert!(parse_command(br#"{"cmd":"reboot"}"#).is_err());
    }

    #[test]
    fn rejects_missing_tab() {
        assert!(parse_command(br#"{"cmd":"save"}"#).is_err());
    }

    #[test]
    fn query_parses_rev() {
        assert_eq!(query_u64("rev=42&x=1", "rev"), Some(42));
        assert_eq!(query_u64("x=1", "rev"), None);
        assert_eq!(query_u64("rev=abc", "rev"), None);
    }

    #[test]
    fn server_roundtrip_state_and_commands() {
        // Bind an ephemeral port, publish a snapshot, fetch it over HTTP,
        // post a command, and confirm the app-side drain sees it.
        let ctx = egui::Context::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let server = RemoteServer::start(port, false, ctx).expect("start");

        let snap = Snapshot {
            app_version: "test".into(),
            tabs: vec![TabInfo { name: "a.mog".into(), path: None, dirty: false }],
            active: 0,
            source: "scene {}".into(),
            status: "ok".into(),
            stage: "ok".into(),
            diagnostics: vec![],
            nodes: 1,
            triangles: 12,
            building: false,
            build_stage: String::new(),
            llm: None,
        };
        server.publish_state(&snap);

        let get = |path: &str| -> (u16, String) {
            use std::io::Write;
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(stream, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
            let mut buf = String::new();
            stream.read_to_string(&mut buf).unwrap();
            let status: u16 = buf
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            (status, body)
        };

        let (status, body) = get("/api/state");
        assert_eq!(status, 200);
        assert!(body.contains("\"source\":\"scene {}\""), "body: {body}");

        // Unchanged marker when the client already has this rev.
        let (_, body2) = get("/api/state?rev=1");
        assert!(body2.contains("\"unchanged\":true"), "body: {body2}");

        // Post a command and drain it app-side.
        {
            use std::io::Write;
            let payload = r#"{"cmd":"activate","tab":0}"#;
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                stream,
                "POST /api/command HTTP/1.0\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
            let mut buf = String::new();
            stream.read_to_string(&mut buf).unwrap();
            assert!(buf.contains("\"ok\":true"), "resp: {buf}");
        }
        let cmds = server.take_commands();
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], RemoteCommand::ActivateTab(0)));

        // The dashboard itself serves at /.
        let (status, body) = get("/");
        assert_eq!(status, 200);
        assert!(body.contains("MoGen"), "dashboard should carry branding");
    }
}
