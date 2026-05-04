//! End-to-end test of `mogen build` against a fake MoGHub. Verifies
//! that a `.mog` file containing `use "@user/slug"` resolves correctly:
//!   - First build fetches the source from the stub server, writes the
//!     cache, writes `mog.lock`, and produces a GLB.
//!   - `--offline` reruns succeed against the cache + lockfile alone.
//!   - `--frozen` against an empty lockfile fails clearly.
//!
//! The fake server mounts on `127.0.0.1:0`; we point `MOGHUB_URL` at
//! it via the test binary's environment.

use std::sync::Arc;
use std::thread;

use assert_cmd::Command;

const MOGEN_BIN: &str = env!("CARGO_BIN_EXE_mogen");

struct StubServer {
    base_url: String,
    handle: Option<thread::JoinHandle<()>>,
    server: Arc<tiny_http::Server>,
}

impl StubServer {
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
        self.server.unblock();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn json_response(body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let bytes = body.as_bytes().to_vec();
    let len = bytes.len();
    tiny_http::Response::new(
        tiny_http::StatusCode(200),
        vec![tiny_http::Header::from_bytes(
            &b"Content-Type"[..],
            &b"application/json"[..],
        )
        .unwrap()],
        std::io::Cursor::new(bytes),
        Some(len),
        None,
    )
}

const ALICE_CHAIRS_DETAIL: &str = r#"{
    "id":"00000000-0000-0000-0000-000000000001",
    "user":{"id":"u1","handle":"alice","avatar_url":null},
    "slug":"chairs","title":"Chairs","description":"","license":"CC0",
    "kind":"module","tags":[],"like_count":0,"fork_count":0,
    "created_at":"2026-01-01T00:00:00Z",
    "version":{
      "id":"00000000-0000-0000-0000-000000000010",
      "version":1,"publish_message":"v1","thumbnail_url":null,
      "created_at":"2026-01-01T00:00:00Z",
      "files":[{"filename":"main.mog","is_entry":true,"bytes":42,
                "source":"scene { box \"seat\" (size=[1, 0.1, 1]) }"}]
    },
    "parent":null,"liked_by_me":false,"is_module":true,
    "dependent_count":0,"tombstoned":false
}"#;

#[test]
fn build_resolves_registry_ref_writes_lock_and_glb() {
    let scratch = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    let server = StubServer::new(|req| {
        if req.url() == "/api/m/alice/chairs" {
            json_response(ALICE_CHAIRS_DETAIL)
        } else {
            tiny_http::Response::new(
                tiny_http::StatusCode(404),
                vec![],
                std::io::Cursor::new(Vec::new()),
                Some(0),
                None,
            )
        }
    });

    let mog_path = scratch.path().join("scene.mog");
    std::fs::write(
        &mog_path,
        r#"scene { use "@alice/chairs" () }"#,
    )
    .unwrap();

    // First build: fetch + cache + lock.
    let out = scratch.path().join("scene.glb");
    Command::new(MOGEN_BIN)
        .args(["build"])
        .arg(&mog_path)
        .args(["--out"])
        .arg(&out)
        .env("MOGHUB_URL", &server.base_url)
        .env("MOGEN_CACHE_DIR", cache.path())
        .assert()
        .success();
    assert!(out.is_file(), "expected GLB at {}", out.display());
    let lock_path = scratch.path().join("mog.lock");
    assert!(lock_path.is_file(), "expected mog.lock at {}", lock_path.display());
    let lock_body = std::fs::read_to_string(&lock_path).unwrap();
    assert!(
        lock_body.contains("@alice/chairs"),
        "lock should pin @alice/chairs: {lock_body}"
    );
    assert!(lock_body.contains("\"version\": 1"));

    // Second build: offline. Cache + lock cover the ref so this
    // should succeed without any network.
    std::fs::remove_file(&out).unwrap();
    Command::new(MOGEN_BIN)
        .args(["build"])
        .arg(&mog_path)
        .args(["--out"])
        .arg(&out)
        .args(["--offline"])
        // Point MOGHUB_URL at a closed loopback so any accidental
        // network call would fail loudly.
        .env("MOGHUB_URL", "http://127.0.0.1:1")
        .env("MOGEN_CACHE_DIR", cache.path())
        .assert()
        .success();
    assert!(out.is_file(), "offline build must produce GLB");
}

#[test]
fn frozen_build_without_lock_fails_clearly() {
    let scratch = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mog_path = scratch.path().join("scene.mog");
    std::fs::write(
        &mog_path,
        r#"scene { use "@alice/chairs" () }"#,
    )
    .unwrap();

    Command::new(MOGEN_BIN)
        .args(["build"])
        .arg(&mog_path)
        .args(["--frozen"])
        .env("MOGHUB_URL", "http://127.0.0.1:1")
        .env("MOGEN_CACHE_DIR", cache.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("not pinned"));
}
