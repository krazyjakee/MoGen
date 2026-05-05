//! Integration tests using a tiny in-process HTTP server. Cheaper than
//! standing up a real moghub instance and faster than mocking reqwest;
//! exercises real wire protocol on real TCP.

use std::sync::Arc;
use std::thread;

use mogen_moghub_client::{DiscoverQuery, MoghubClient};
use mogen_registry::refs::RegistryRef;
use mogen_registry::RegistryClient;

struct StubServer {
    base_url: String,
    handle: Option<thread::JoinHandle<()>>,
    server: Arc<tiny_http::Server>,
}

impl StubServer {
    /// Spin up a server on `127.0.0.1:0` (kernel-assigned). The handler
    /// dispatches by path; tests register the responses they need before
    /// constructing the server, then drop the server on test exit.
    fn new<F>(handler: F) -> Self
    where
        F: Fn(&tiny_http::Request) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> + Send + Sync + 'static,
    {
        let server =
            Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind tiny_http"));
        let base_url = format!("http://{}", server.server_addr());
        let server_clone = Arc::clone(&server);
        let handle = thread::spawn(move || {
            for req in server_clone.incoming_requests() {
                let resp = handler(&req);
                let _ = req.respond(resp);
            }
        });
        StubServer {
            base_url,
            handle: Some(handle),
            server,
        }
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        // Unblock the request loop and join the worker.
        self.server.unblock();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn json_response(body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let bytes = body.as_bytes().to_vec();
    let len = bytes.len();
    let mut resp = tiny_http::Response::new(
        tiny_http::StatusCode(200),
        vec![tiny_http::Header::from_bytes(
            &b"Content-Type"[..],
            &b"application/json"[..],
        )
        .unwrap()],
        std::io::Cursor::new(bytes),
        Some(len),
        None,
    );
    let _ = &mut resp;
    resp
}

fn text_response(
    status: u16,
    body: &str,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let bytes = body.as_bytes().to_vec();
    let len = bytes.len();
    tiny_http::Response::new(
        tiny_http::StatusCode(status),
        vec![],
        std::io::Cursor::new(bytes),
        Some(len),
        None,
    )
}

#[test]
fn whoami_anonymous() {
    let server = StubServer::new(|req| {
        assert_eq!(req.url(), "/api/whoami");
        json_response(r#"{"user":null}"#)
    });
    let client = MoghubClient::new(&server.base_url).unwrap();
    let r = client.whoami().unwrap();
    assert!(r.user.is_none());
}

#[test]
fn discover_with_query_serialises_params() {
    let server = StubServer::new(|req| {
        // The push closure builds query strings in the order we set
        // them; "q=chair&kind=module" is the expected shape.
        let url = req.url();
        assert!(url.starts_with("/api/discover?"), "got {url}");
        assert!(url.contains("q=chair"), "got {url}");
        assert!(url.contains("kind=module"), "got {url}");
        json_response(r#"{"featured":null,"items":[]}"#)
    });
    let client = MoghubClient::new(&server.base_url).unwrap();
    let q = DiscoverQuery {
        q: Some("chair".into()),
        kind: Some("module".into()),
        ..Default::default()
    };
    let r = client.discover(q).unwrap();
    assert!(r.items.is_empty());
}

#[test]
fn registry_client_fetches_latest_version_inline() {
    let body = r#"{
        "id":"00000000-0000-0000-0000-000000000001",
        "user":{"id":"u1","handle":"alice","avatar_url":null},
        "slug":"chairs","title":"Chairs","description":"","license":"CC0",
        "kind":"module","tags":[],"like_count":0,"fork_count":0,
        "created_at":"2026-01-01T00:00:00Z",
        "version":{
          "id":"00000000-0000-0000-0000-000000000010",
          "version":3,"publish_message":"v3","thumbnail_url":null,
          "created_at":"2026-01-01T00:00:00Z",
          "files":[{"filename":"main.mog","is_entry":true,"bytes":42,
                    "source":"scene { box \"b\" () }"}]
        },
        "parent":null,"liked_by_me":false,"is_module":true,
        "dependent_count":0,"tombstoned":false
    }"#;
    let body_owned = body.to_string();
    let server = StubServer::new(move |req| {
        assert_eq!(req.url(), "/api/m/alice/chairs");
        json_response(&body_owned)
    });
    let client = MoghubClient::new(&server.base_url).unwrap();
    let spec = RegistryRef {
        user: "alice".into(),
        slug: "chairs".into(),
        version: None,
        raw: "@alice/chairs".into(),
    };
    let fv = client.fetch(&spec).unwrap();
    assert_eq!(fv.version, 3);
    assert_eq!(fv.files.len(), 1);
    assert!(fv.files[0].source.contains("box"));
    assert_eq!(fv.model_id, "00000000-0000-0000-0000-000000000001");
}

#[test]
fn server_error_surfaces_status_and_body() {
    let server = StubServer::new(|_req| text_response(503, "{\"error\":\"down\"}"));
    let client = MoghubClient::new(&server.base_url).unwrap();
    let err = client.whoami().unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("503"), "got: {msg}");
    assert!(msg.contains("down"), "got: {msg}");
}

#[test]
fn unauthorized_maps_to_dedicated_error() {
    let server = StubServer::new(|_req| text_response(401, ""));
    let client = MoghubClient::new(&server.base_url).unwrap();
    let err = client.whoami().unwrap_err();
    assert!(matches!(err, mogen_moghub_client::MoghubError::Unauthorized));
}

#[test]
fn pinned_old_version_resolves_via_version_detail() {
    // P2 added GET /api/m/:user/:slug/versions/:version, so the
    // registry resolver no longer errors on non-latest pins — it
    // hits that endpoint after model_detail tells us latest != pin.
    // Stub returns the latest from /api/m/:user/:slug and the
    // pinned bytes from /api/m/:user/:slug/versions/:version.
    let detail = r#"{
        "id":"00000000-0000-0000-0000-000000000001",
        "user":{"id":"u1","handle":"alice","avatar_url":null},
        "slug":"chairs","title":"Chairs","description":"","license":"CC0",
        "kind":"module","tags":[],"like_count":0,"fork_count":0,
        "created_at":"2026-01-01T00:00:00Z",
        "version":{
          "id":"00000000-0000-0000-0000-000000000010","version":5,
          "publish_message":"v5","thumbnail_url":null,
          "created_at":"2026-01-01T00:00:00Z","files":[]
        },
        "parent":null,"liked_by_me":false,"is_module":true,
        "dependent_count":0,"tombstoned":false
    }"#;
    let pinned = r#"{
        "model_id":"00000000-0000-0000-0000-000000000001",
        "user":{"id":"u1","handle":"alice","avatar_url":null},
        "slug":"chairs","is_module":true,"tombstoned":false,
        "version":{
          "id":"00000000-0000-0000-0000-000000000020","version":2,
          "publish_message":"v2","thumbnail_url":null,
          "created_at":"2026-01-01T00:00:00Z",
          "files":[{
            "filename":"chair.mog","is_entry":true,"bytes":7,
            "source":"node{}\n","dedup_target":null
          }]
        },
        "mog_lock":"{}"
    }"#;
    let detail_owned = detail.to_string();
    let pinned_owned = pinned.to_string();
    let server = StubServer::new(move |req| {
        let url = req.url();
        if url.contains("/versions/") {
            json_response(&pinned_owned)
        } else {
            json_response(&detail_owned)
        }
    });
    let client = MoghubClient::new(&server.base_url).unwrap();
    let spec = RegistryRef {
        user: "alice".into(),
        slug: "chairs".into(),
        version: Some(2),
        raw: "@alice/chairs@2".into(),
    };
    let fetched = client.fetch(&spec).expect("non-latest pin should resolve");
    assert_eq!(fetched.version, 2);
    assert_eq!(fetched.files.len(), 1);
    assert_eq!(fetched.files[0].filename, "chair.mog");
}
