//! HTTP client for MoGHub. Used by MoGen Studio and the `mogen` CLI to
//! browse the discover feed, fetch model sources, resolve registry refs,
//! and (P2+) publish + perform social actions.
//!
//! The client is **synchronous** — `reqwest::blocking` — because the
//! desktop callers (Studio's egui main loop, `mogen build`) are sync and
//! shouldn't have to bring up a tokio runtime just to talk to the
//! registry. Long-running operations are dispatched to `std::thread`
//! workers in Studio (mirroring `app/llm.rs`); the client itself stays
//! free of async plumbing.
//!
//! Auth is bearer-token based (server-side support added in P2). The
//! same UUID that the cookie session uses is sent as
//! `Authorization: Bearer <uuid>` so existing browser sessions and
//! desktop sessions share one storage table.

pub mod dtos;
mod error;
#[cfg(feature = "oauth")]
pub mod oauth;
pub mod registry;

use std::time::Duration;

use anyhow::Result;
use reqwest::blocking::Client as HttpClient;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use url::Url;

pub use dtos::*;
pub use error::MoghubError;

/// Default base URL — points at production. Can be overridden per-call
/// via [`MoghubClient::with_base_url`] or via `MOGHUB_URL` for the
/// `from_env` constructor.
pub const DEFAULT_BASE_URL: &str = "https://moghub.org";

const USER_AGENT_VALUE: &str = concat!("mogen-moghub-client/", env!("CARGO_PKG_VERSION"));

/// HTTP client for the MoGHub JSON API.
#[derive(Debug, Clone)]
pub struct MoghubClient {
    base_url: Url,
    token: Option<String>,
    http: HttpClient,
}

impl MoghubClient {
    /// Construct a client pointing at `base_url`. Token is unset — call
    /// [`Self::with_token`] after auth completes.
    pub fn new(base_url: &str) -> Result<Self, MoghubError> {
        let base_url = Url::parse(base_url).map_err(|e| MoghubError::Decode(e.to_string()))?;
        let http = HttpClient::builder()
            .user_agent(USER_AGENT_VALUE)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(MoghubError::network)?;
        Ok(Self {
            base_url,
            token: None,
            http,
        })
    }

    /// Construct a client honouring `MOGHUB_URL` if set, otherwise
    /// [`DEFAULT_BASE_URL`]. Convenience for binaries that read config
    /// from the environment.
    pub fn from_env() -> Result<Self, MoghubError> {
        let url = std::env::var("MOGHUB_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self::new(&url)
    }

    /// Replace the bearer token. Pass `None` to clear (sign-out).
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }

    /// Replace the base URL. Useful in tests + when the user changes
    /// `moghub_url` in Studio Settings.
    pub fn with_base_url(mut self, base_url: &str) -> Result<Self, MoghubError> {
        self.base_url = Url::parse(base_url).map_err(|e| MoghubError::Decode(e.to_string()))?;
        Ok(self)
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    fn url_for(&self, path: &str) -> Result<Url, MoghubError> {
        // The base may or may not end in `/`; urls join cleanly when the
        // base ends in `/` and the path doesn't start with `/`. Tolerate
        // both: trim leading `/` from `path` so `https://host/api` and
        // `https://host/api/` both work the same way.
        let trimmed = path.trim_start_matches('/');
        let mut joined = self.base_url.clone();
        if !joined.path().ends_with('/') {
            // Force a trailing slash so url::Url::join treats the base as
            // a directory rather than a file.
            joined
                .path_segments_mut()
                .map_err(|_| MoghubError::Decode("invalid base url".into()))?
                .push("");
        }
        joined
            .join(trimmed)
            .map_err(|e| MoghubError::Decode(e.to_string()))
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, MoghubError> {
        let url = self.url_for(path)?;
        let mut req = self
            .http
            .get(url)
            .header(ACCEPT, "application/json")
            .header(USER_AGENT, USER_AGENT_VALUE);
        if let Some(t) = &self.token {
            req = req.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = req.send().map_err(MoghubError::network)?;
        decode_json(resp)
    }

    fn get_bytes(&self, path: &str) -> Result<Vec<u8>, MoghubError> {
        let url = self.url_for(path)?;
        let mut req = self.http.get(url).header(USER_AGENT, USER_AGENT_VALUE);
        if let Some(t) = &self.token {
            req = req.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = req.send().map_err(MoghubError::network)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(MoghubError::status(status.as_u16(), body));
        }
        resp.bytes()
            .map(|b| b.to_vec())
            .map_err(MoghubError::network)
    }

    fn get_text(&self, path: &str) -> Result<String, MoghubError> {
        let bytes = self.get_bytes(path)?;
        String::from_utf8(bytes).map_err(|e| MoghubError::Decode(e.to_string()))
    }

    /// POST a JSON body and decode the JSON response. Used for
    /// authenticated mutations (likes, comments, publish, …). Bearer
    /// is attached when set.
    fn post_json<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, MoghubError> {
        let url = self.url_for(path)?;
        let mut req = self
            .http
            .post(url)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .header(USER_AGENT, USER_AGENT_VALUE);
        if let Some(t) = &self.token {
            req = req.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = req
            .json(body)
            .send()
            .map_err(MoghubError::network)?;
        decode_json(resp)
    }

    /// POST with no body, decode JSON. Distinct from [`Self::post_json`]
    /// because reqwest's `.json(())` serialises `null`, which some
    /// handlers (looking at you, `axum::extract::Json`) reject as a
    /// missing field.
    fn post_empty<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, MoghubError> {
        let url = self.url_for(path)?;
        let mut req = self
            .http
            .post(url)
            .header(ACCEPT, "application/json")
            .header(USER_AGENT, USER_AGENT_VALUE);
        if let Some(t) = &self.token {
            req = req.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = req.send().map_err(MoghubError::network)?;
        decode_json(resp)
    }

    /// DELETE, decode JSON. Used for like-undo.
    fn delete_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, MoghubError> {
        let url = self.url_for(path)?;
        let mut req = self
            .http
            .delete(url)
            .header(ACCEPT, "application/json")
            .header(USER_AGENT, USER_AGENT_VALUE);
        if let Some(t) = &self.token {
            req = req.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = req.send().map_err(MoghubError::network)?;
        decode_json(resp)
    }

    /// Fetch raw bytes from either an absolute URL (e.g. a GitHub avatar
    /// CDN) or a moghub-relative path (e.g. `/api/m/.../thumbnail.png`).
    /// The bearer token is only attached for relative paths so we don't
    /// leak it to third-party origins.
    pub fn fetch_image_bytes(&self, url: &str) -> Result<Vec<u8>, MoghubError> {
        if url.starts_with("http://") || url.starts_with("https://") {
            let resp = self
                .http
                .get(url)
                .header(USER_AGENT, USER_AGENT_VALUE)
                .send()
                .map_err(MoghubError::network)?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().unwrap_or_default();
                return Err(MoghubError::status(status.as_u16(), body));
            }
            resp.bytes()
                .map(|b| b.to_vec())
                .map_err(MoghubError::network)
        } else {
            self.get_bytes(url)
        }
    }

    // --- public API endpoints (no auth required) ----------------------

    /// `GET /api/whoami` — returns the signed-in user, or `None` for
    /// anonymous requests. Used to gate publish/like/comment UI in
    /// Studio.
    pub fn whoami(&self) -> Result<WhoAmI, MoghubError> {
        self.get("/api/whoami")
    }

    /// `GET /api/discover` — public listing for the Community window's
    /// front page.
    pub fn discover(&self, q: DiscoverQuery) -> Result<DiscoverResponse, MoghubError> {
        let mut path = String::from("/api/discover");
        let mut sep = '?';
        let push = |k: &str, v: &str, p: &mut String, s: &mut char| {
            p.push(*s);
            p.push_str(k);
            p.push('=');
            p.push_str(&urlencode(v));
            *s = '&';
        };
        if let Some(s) = &q.q {
            push("q", s, &mut path, &mut sep);
        }
        if let Some(k) = &q.kind {
            push("kind", k, &mut path, &mut sep);
        }
        if let Some(t) = &q.tag {
            push("tag", t, &mut path, &mut sep);
        }
        if let Some(l) = q.limit {
            push("limit", &l.to_string(), &mut path, &mut sep);
        }
        if let Some(o) = q.offset {
            push("offset", &o.to_string(), &mut path, &mut sep);
        }
        self.get(&path)
    }

    /// `GET /api/m/:user/:slug` — full model detail.
    pub fn model_detail(&self, user: &str, slug: &str) -> Result<ModelDetail, MoghubError> {
        let path = format!(
            "/api/m/{}/{}",
            urlencode_segment(user),
            urlencode_segment(slug)
        );
        self.get(&path)
    }

    /// `GET /api/m/:user/:slug/versions/:version` — full source bodies for
    /// one specific version. Use this whenever you have a pinned version
    /// number; `model_detail` only returns the latest.
    pub fn version_detail(
        &self,
        user: &str,
        slug: &str,
        version: i32,
    ) -> Result<ModelVersionDetail, MoghubError> {
        let path = format!(
            "/api/m/{}/{}/versions/{}",
            urlencode_segment(user),
            urlencode_segment(slug),
            version,
        );
        self.get(&path)
    }

    /// `GET /api/m/:user/:slug/files/:filename` — raw `.mog` source for
    /// the latest version.
    pub fn file_raw(
        &self,
        user: &str,
        slug: &str,
        filename: &str,
    ) -> Result<String, MoghubError> {
        let path = format!(
            "/api/m/{}/{}/files/{}",
            urlencode_segment(user),
            urlencode_segment(slug),
            urlencode_segment(filename)
        );
        self.get_text(&path)
    }

    /// `GET /api/m/:user/:slug/versions/:version_id/thumbnail.png` —
    /// the published thumbnail.
    pub fn thumbnail_png(
        &self,
        user: &str,
        slug: &str,
        version_id: &str,
    ) -> Result<Vec<u8>, MoghubError> {
        let path = format!(
            "/api/m/{}/{}/versions/{}/thumbnail.png",
            urlencode_segment(user),
            urlencode_segment(slug),
            urlencode_segment(version_id)
        );
        self.get_bytes(&path)
    }

    /// `GET /api/m/:user/:slug/deps` — dependency graph (both directions).
    pub fn deps(&self, user: &str, slug: &str) -> Result<DependencyList, MoghubError> {
        let path = format!(
            "/api/m/{}/{}/deps",
            urlencode_segment(user),
            urlencode_segment(slug)
        );
        self.get(&path)
    }

    /// `GET /api/m/:user/:slug/updates` — outdated-pin banner data.
    pub fn updates(&self, user: &str, slug: &str) -> Result<UpdatesAvailable, MoghubError> {
        let path = format!(
            "/api/m/{}/{}/updates",
            urlencode_segment(user),
            urlencode_segment(slug)
        );
        self.get(&path)
    }

    /// `GET /api/registry/suggest` — autocomplete entries for
    /// `use "@user/slug…"` typeahead.
    pub fn registry_suggest(
        &self,
        q: &str,
        limit: Option<u32>,
    ) -> Result<Vec<ModuleSuggestion>, MoghubError> {
        let mut path = format!("/api/registry/suggest?q={}", urlencode(q));
        if let Some(l) = limit {
            path.push_str(&format!("&limit={l}"));
        }
        self.get(&path)
    }

    /// `GET /api/m/:user/:slug/comments` — public comment thread.
    pub fn comments(&self, user: &str, slug: &str) -> Result<CommentList, MoghubError> {
        let path = format!(
            "/api/m/{}/{}/comments",
            urlencode_segment(user),
            urlencode_segment(slug)
        );
        self.get(&path)
    }

    /// `GET /api/m/:user/:slug/forks` — fork lineage.
    pub fn forks(&self, user: &str, slug: &str) -> Result<ForkList, MoghubError> {
        let path = format!(
            "/api/m/{}/{}/forks",
            urlencode_segment(user),
            urlencode_segment(slug)
        );
        self.get(&path)
    }

    // --- mutations (require a bearer token) ---------------------------

    /// `POST /api/m/:user/:slug/like` — idempotent. Returns the
    /// canonical liked-state + count.
    pub fn like(&self, user: &str, slug: &str) -> Result<LikeResponse, MoghubError> {
        let path = format!(
            "/api/m/{}/{}/like",
            urlencode_segment(user),
            urlencode_segment(slug)
        );
        self.post_empty(&path)
    }

    /// `DELETE /api/m/:user/:slug/like`.
    pub fn unlike(&self, user: &str, slug: &str) -> Result<LikeResponse, MoghubError> {
        let path = format!(
            "/api/m/{}/{}/like",
            urlencode_segment(user),
            urlencode_segment(slug)
        );
        self.delete_json(&path)
    }

    /// `POST /api/m/:user/:slug/comments` — bbcode body. Returns the
    /// newly-inserted comment with `body_html` already rendered.
    pub fn post_comment(
        &self,
        user: &str,
        slug: &str,
        body: &str,
    ) -> Result<Comment, MoghubError> {
        let path = format!(
            "/api/m/{}/{}/comments",
            urlencode_segment(user),
            urlencode_segment(slug)
        );
        self.post_json(
            &path,
            &CreateCommentRequest {
                body: body.to_string(),
            },
        )
    }

    /// `GET /api/notifications` — inbox + unread count for the
    /// signed-in user.
    pub fn notifications(&self) -> Result<NotificationList, MoghubError> {
        self.get("/api/notifications")
    }

    /// `POST /api/notifications` — mark every notification read.
    pub fn mark_notifications_read(&self) -> Result<NotificationList, MoghubError> {
        self.post_empty("/api/notifications")
    }

    /// `POST /api/models` — create a new model version.
    pub fn publish(&self, req: &PublishRequest) -> Result<PublishResponse, MoghubError> {
        self.post_json("/api/models", req)
    }
}

/// Filter set for the `discover` endpoint. Empty defaults are the
/// public front page.
#[derive(Debug, Default, Clone)]
pub struct DiscoverQuery {
    pub q: Option<String>,
    /// "all" | "scene" | "model" | "module".
    pub kind: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

fn decode_json<T: serde::de::DeserializeOwned>(
    resp: reqwest::blocking::Response,
) -> Result<T, MoghubError> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(MoghubError::status(status.as_u16(), body));
    }
    resp.json::<T>().map_err(|e| MoghubError::Decode(e.to_string()))
}

/// Minimal percent-encoder for query-string values. Keeps unreserved
/// characters and `-_.~`; everything else becomes `%XX`. Sufficient for
/// what the API accepts and avoids pulling in `percent-encoding` for
/// such a small surface.
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

/// Same as [`urlencode`] but also escapes `/` — used for path segments
/// where a slug or filename could contain unexpected characters.
fn urlencode_segment(s: &str) -> String {
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
