//! Hand-rolled Gemini REST client.
//!
//! SDK crates lag the spec; we need `cachedContents` and structured errors.
//! Blocking HTTP keeps the `mogen` binary synchronous — one request per repair
//! iteration is not worth a tokio runtime.
//!
//! Authentication is dual-mode: either a public-API key (legacy / free-tier
//! Studio path) or an OAuth bundle (Antigravity Pro plan via the Cloud Code
//! Assist `v1internal` surface). The mode is picked at construction time and
//! drives URL/body/header shape inside [`Self::generate`] — see
//! [`crate::google_oauth`] for the bundle layout, refresh logic, and the
//! cloudcode envelope helpers.

use std::sync::Mutex;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// Re-export the shared types so existing callers using `mogen_llm::gemini::*`
// keep compiling after the multi-provider refactor. New code should import
// these directly from `mogen_llm`.
pub use crate::types::{
    GenerateConfig, GenerateResponse, ImageInput, Role, ThinkingLevel, Turn, Usage,
    DEFAULT_TEMPERATURE,
};

use crate::google_oauth::{self, OAuthBundle, ProviderConfig};

/// Default model for `mogen generate`. Alias auto-rolls to the newest Pro tier.
pub const DEFAULT_MODEL: &str = "gemini-pro-latest";

/// Default model for fast / low-stakes text calls (prompt enhancement, small
/// rewrites). ~4× cheaper than the Pro tier and fast enough for interactive
/// use — callers that need the Pro tier can still override.
pub const DEFAULT_FAST_MODEL: &str = "gemini-flash-latest";

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

#[derive(Debug, Error)]
pub enum GeminiError {
    #[error("missing GEMINI_API_KEY (set env var or pass --api-key)")]
    MissingApiKey,
    #[error("transport error: {}", format_source_chain(.0))]
    Transport(#[from] reqwest::Error),
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("empty response: no candidates or text parts")]
    EmptyResponse,
    #[error("budget exceeded: {used} input+output tokens exceeds --budget-tokens={budget}")]
    BudgetExceeded { used: u32, budget: u32 },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    /// OAuth-side failure: refresh failed, project id missing, token store
    /// invalid. Carries the [`OAuthError`] message verbatim.
    #[error("oauth error: {0}")]
    OAuth(String),
    /// Surfaces in OAuth mode when a caller asks for `cachedContents`. The
    /// `v1internal` surface does not expose that resource family — the
    /// caller should fall back to inline system-instruction.
    #[error("cachedContents is not available over the Antigravity OAuth surface; falling back to inline")]
    CacheUnavailableOverOAuth,
}

/// Authentication mode. `ApiKey` uses the public `generativelanguage`
/// surface; the OAuth variants swap to the Cloud Code Assist `v1internal`
/// shape — `OAuth` is the gemini-cli client (text gen), `AntigravityOAuth`
/// is the Antigravity desktop client (required for image gen). Both
/// surface types accept either OAuth client for text gen, but the image
/// surface 403s the gemini-cli client. The `Mutex` lets the auto-refresh
/// logic mutate the bundle through `&self`-borrowing methods.
pub enum GeminiAuth {
    ApiKey(String),
    OAuth(Mutex<OAuthBundle>),
    AntigravityOAuth(Mutex<OAuthBundle>),
}

/// Handle returned after creating a `cachedContents` resource. The resource
/// name is what you pass to [`GenerateConfig::cached_content`] on subsequent
/// calls; `expires_at_unix` lets callers persist cache state locally and
/// re-use the resource until it expires server-side.
#[derive(Debug, Clone)]
pub struct CachedContent {
    pub name: String,
    pub expires_at_unix: u64,
    pub token_count: Option<u32>,
}

pub struct GeminiClient {
    http: reqwest::blocking::Client,
    auth: GeminiAuth,
    base_url: String,
    /// Lazily-filled list of `(model_id, in_catalog)` tuples used to drive
    /// image-gen retry logic. The flag distinguishes catalog hits (retry
    /// 404 with backoff — transient capacity) from static-list fallbacks
    /// (walk past 404 immediately — model isn't routed for this bundle).
    /// Filled on first image-gen call via `:fetchAvailableModels` (mirrors
    /// McKrei's `opencode-antigravity-nano-banana` runtime detection);
    /// falls back to the static [`google_oauth::client::ANTIGRAVITY_IMAGE_MODELS`]
    /// list with all entries flagged `false` if the probe itself errors.
    /// `None` = "not yet probed".
    image_model_catalog: Mutex<Option<Vec<(String, bool)>>>,
}

impl GeminiClient {
    /// API-key client targeting the public `generativelanguage` surface.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let http = build_http();
        Self {
            http,
            auth: GeminiAuth::ApiKey(api_key.into()),
            base_url: base_url.into(),
            image_model_catalog: Mutex::new(None),
        }
    }

    /// OAuth-mode client targeting the Cloud Code Assist `v1internal`
    /// surface using the **gemini-cli** OAuth client. The bundle's
    /// `endpoint_base` is used as the base URL — fall back to prod when
    /// the bundle was written by a path that didn't run discovery yet
    /// (legacy tokens). For image generation use
    /// [`Self::from_antigravity_oauth`] instead — the image surface 403s
    /// the gemini-cli client.
    pub fn from_oauth(bundle: OAuthBundle) -> Self {
        let base_url = bundle
            .endpoint_base
            .clone()
            .unwrap_or_else(|| google_oauth::client::ENDPOINT_PROD.to_string());
        let http = build_http();
        Self {
            http,
            auth: GeminiAuth::OAuth(Mutex::new(bundle)),
            base_url,
            image_model_catalog: Mutex::new(None),
        }
    }

    /// OAuth-mode client using the **Antigravity** OAuth client. Required
    /// for image generation (textures); also works for text gen when a
    /// caller wants a single bundle for both surfaces.
    pub fn from_antigravity_oauth(bundle: OAuthBundle) -> Self {
        let base_url = bundle
            .endpoint_base
            .clone()
            .unwrap_or_else(|| google_oauth::client::ENDPOINT_PROD.to_string());
        let http = build_http();
        Self {
            http,
            auth: GeminiAuth::AntigravityOAuth(Mutex::new(bundle)),
            base_url,
            image_model_catalog: Mutex::new(None),
        }
    }

    /// Inject a custom base URL for OAuth-mode tests. The mock server
    /// stands in for `cloudcode-pa.googleapis.com` so the URL/body/header
    /// shape can be verified without hitting Google. Defaults to the
    /// gemini-cli OAuth variant.
    pub fn from_oauth_with_base_url(
        bundle: OAuthBundle,
        base_url: impl Into<String>,
    ) -> Self {
        let http = build_http();
        Self {
            http,
            auth: GeminiAuth::OAuth(Mutex::new(bundle)),
            base_url: base_url.into(),
            image_model_catalog: Mutex::new(None),
        }
    }

    /// Inject a custom base URL for Antigravity-OAuth-mode tests.
    pub fn from_antigravity_oauth_with_base_url(
        bundle: OAuthBundle,
        base_url: impl Into<String>,
    ) -> Self {
        let http = build_http();
        Self {
            http,
            auth: GeminiAuth::AntigravityOAuth(Mutex::new(bundle)),
            base_url: base_url.into(),
            image_model_catalog: Mutex::new(None),
        }
    }

    pub fn from_env() -> Result<Self, GeminiError> {
        let key = std::env::var("GEMINI_API_KEY").map_err(|_| GeminiError::MissingApiKey)?;
        if key.trim().is_empty() {
            return Err(GeminiError::MissingApiKey);
        }
        Ok(Self::new(key))
    }

    pub(crate) fn http(&self) -> &reqwest::blocking::Client {
        &self.http
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn auth(&self) -> &GeminiAuth {
        &self.auth
    }

    /// True when this client authenticates via either OAuth provider.
    pub fn is_oauth(&self) -> bool {
        matches!(
            self.auth,
            GeminiAuth::OAuth(_) | GeminiAuth::AntigravityOAuth(_)
        )
    }

    /// True specifically when this client uses the Antigravity OAuth
    /// client (the only one accepted by the image surface).
    pub fn is_antigravity_oauth(&self) -> bool {
        matches!(self.auth, GeminiAuth::AntigravityOAuth(_))
    }

    /// Snapshot of the OAuth bundle (cloned, so it's safe to inspect across
    /// refresh boundaries). Returns `None` for API-key clients.
    pub fn oauth_snapshot(&self) -> Option<OAuthBundle> {
        match &self.auth {
            GeminiAuth::OAuth(mu) | GeminiAuth::AntigravityOAuth(mu) => {
                mu.lock().ok().map(|b| b.clone())
            }
            GeminiAuth::ApiKey(_) => None,
        }
    }

    /// Currently bound project id (OAuth only).
    pub(crate) fn oauth_project_id(&self) -> Option<String> {
        match &self.auth {
            GeminiAuth::OAuth(mu) | GeminiAuth::AntigravityOAuth(mu) => {
                mu.lock().ok().and_then(|b| b.project_id.clone())
            }
            GeminiAuth::ApiKey(_) => None,
        }
    }

    /// Pick the matching `(bundle mutex, ProviderConfig)` pair for the
    /// active OAuth variant — `None` for API-key clients. Used by
    /// [`Self::oauth_post_inner`] to refresh against the right token
    /// endpoint with the right `client_id`/`client_secret`.
    fn oauth_state(&self) -> Option<(&Mutex<OAuthBundle>, &'static ProviderConfig)> {
        match &self.auth {
            GeminiAuth::OAuth(mu) => Some((mu, &google_oauth::GEMINI_CLI_CONFIG)),
            GeminiAuth::AntigravityOAuth(mu) => Some((mu, &google_oauth::ANTIGRAVITY_CONFIG)),
            GeminiAuth::ApiKey(_) => None,
        }
    }

    /// Cloud Code Assist POST with auto-refresh + one 401 retry.
    /// Returns response body bytes on 2xx; `Api` on any non-success
    /// (including the post-refresh retry's failure). Uses the text-surface
    /// header set (gemini-cli UA).
    pub(crate) fn oauth_post_with_retry(
        &self,
        cloudcode_url: &str,
        body: &serde_json::Value,
    ) -> Result<Vec<u8>, GeminiError> {
        self.oauth_post_inner(
            cloudcode_url,
            body,
            google_oauth::cloudcode::apply_headers,
        )
    }

    /// Same as [`oauth_post_with_retry`] but applies the image-surface
    /// headers (Antigravity UA, `Accept: text/event-stream`, JSON
    /// `Client-Metadata` with `ideType: "ANTIGRAVITY"`). The image host on
    /// `daily-cloudcode-pa.googleapis.com` 404s with the text-surface header
    /// set, so the image path needs its own applier.
    pub(crate) fn oauth_post_image_with_retry(
        &self,
        cloudcode_url: &str,
        body: &serde_json::Value,
    ) -> Result<Vec<u8>, GeminiError> {
        self.oauth_post_inner(
            cloudcode_url,
            body,
            google_oauth::cloudcode::apply_image_headers,
        )
    }

    /// Lazily fill and return the image-model candidate list for this
    /// client, paired with an `in_catalog` flag.
    ///
    /// Calls `:fetchAvailableModels` on `endpoint_base` once per client,
    /// then unions catalog hits (flagged `true`) with the static
    /// [`ANTIGRAVITY_IMAGE_MODELS`][m] preference list (catalog-misses
    /// flagged `false`). Catalog hits come first so callers retry
    /// transient failures on known-good names before paying 404 walks
    /// for fallbacks.
    ///
    /// The flag drives the caller's 404 retry policy: in-catalog 404 is
    /// treated as transient capacity (backoff retry); not-in-catalog 404
    /// is treated as "model not available for this bundle" (walk
    /// immediately, no backoff).
    ///
    /// On any probe failure (network, 404, parse), every entry falls
    /// back to flagged `false` — we have no signal which names route, so
    /// each gets a fast 404-walk policy.
    ///
    /// [m]: crate::google_oauth::client::ANTIGRAVITY_IMAGE_MODELS
    pub(crate) fn ensure_image_catalog(
        &self,
        project: &str,
        endpoint_base: &str,
    ) -> Result<Vec<(String, bool)>, GeminiError> {
        {
            let lock = self
                .image_model_catalog
                .lock()
                .expect("image_model_catalog mutex poisoned");
            if let Some(cached) = lock.as_ref() {
                return Ok(cached.clone());
            }
        }

        let url = google_oauth::cloudcode::fetch_available_models_url(endpoint_base);
        let body = google_oauth::cloudcode::fetch_available_models_body(project);
        let result = self.oauth_post_inner(
            &url,
            &body,
            google_oauth::cloudcode::apply_image_headers,
        );

        let static_list: Vec<&'static str> =
            google_oauth::client::ANTIGRAVITY_IMAGE_MODELS.to_vec();

        let live: Vec<String> = match result.as_ref() {
            Ok(bytes) => google_oauth::cloudcode::parse_available_image_models(bytes),
            Err(_) => Vec::new(),
        };

        // Union: catalog entries first (in_catalog=true), then static-list
        // entries not already in the catalog (in_catalog=false). The
        // caller retries in-catalog 404s with backoff and walks past
        // not-in-catalog 404s immediately.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut combined: Vec<(String, bool)> = Vec::new();
        for n in &live {
            if seen.insert(n.clone()) {
                combined.push((n.clone(), true));
            }
        }
        for n in &static_list {
            if seen.insert(n.to_string()) {
                combined.push((n.to_string(), false));
            }
        }

        if std::env::var("MOGEN_DEBUG_HTTP").ok().as_deref() == Some("1") {
            eprintln!(
                "[mogen image catalog] endpoint={} project={} live={:?} candidates={:?}",
                endpoint_base, project, live, combined
            );
        }

        let mut lock = self
            .image_model_catalog
            .lock()
            .expect("image_model_catalog mutex poisoned");
        *lock = Some(combined.clone());
        Ok(combined)
    }

    /// Shared retry/refresh body for both the text and image OAuth POST
    /// paths. Parameterised on a header applier so each surface can pin
    /// its own UA / `X-Goog-Api-Client` / `Accept` set without duplicating
    /// the auth state machine.
    fn oauth_post_inner(
        &self,
        cloudcode_url: &str,
        body: &serde_json::Value,
        apply: fn(
            reqwest::blocking::RequestBuilder,
            &str,
        ) -> reqwest::blocking::RequestBuilder,
    ) -> Result<Vec<u8>, GeminiError> {
        let (mu, config) = self.oauth_state().ok_or_else(|| {
            GeminiError::OAuth("oauth_post_with_retry called on API-key client".into())
        })?;

        // Eager refresh if we're inside the expiry buffer. Persist the
        // updated bundle on success so a rotated token survives a process
        // restart — best-effort; a non-writable store warns to stderr but
        // doesn't fail the call.
        {
            let mut bundle = mu.lock().expect("oauth bundle mutex poisoned");
            let refreshed =
                google_oauth::refresh_if_needed(&self.http, &mut bundle, now_unix(), config)
                    .map_err(|e| GeminiError::OAuth(e.to_string()))?;
            if refreshed {
                if let Err(e) = google_oauth::persist_bundle(&bundle, config) {
                    eprintln!("mogen: failed to persist refreshed OAuth bundle: {e}");
                }
            }
        }
        let token = mu.lock().expect("poison").access_token.clone();

        let resp = apply(self.http.post(cloudcode_url).json(body), &token).send()?;
        let status = resp.status();

        if status.as_u16() == 401 {
            // Refresh + retry once. If the refresh itself fails (e.g.
            // refresh token revoked), surface that — the user has to
            // re-login. If the retry also 401s, it's not a token issue;
            // propagate the API error.
            {
                let mut bundle = mu.lock().expect("poison");
                google_oauth::refresh_now(&self.http, &mut bundle, now_unix(), config)
                    .map_err(|e| GeminiError::OAuth(e.to_string()))?;
                if let Err(e) = google_oauth::persist_bundle(&bundle, config) {
                    eprintln!("mogen: failed to persist refreshed OAuth bundle: {e}");
                }
            }
            let token2 = mu.lock().expect("poison").access_token.clone();
            let resp2 = apply(self.http.post(cloudcode_url).json(body), &token2).send()?;
            let status2 = resp2.status();
            let bytes2 = resp2.bytes()?;
            if !status2.is_success() {
                return Err(GeminiError::Api {
                    status: status2.as_u16(),
                    message: parse_error_message(&bytes2),
                });
            }
            return Ok(bytes2.to_vec());
        }

        let bytes = resp.bytes()?;
        if !status.is_success() {
            return Err(GeminiError::Api {
                status: status.as_u16(),
                message: parse_error_message(&bytes),
            });
        }
        Ok(bytes.to_vec())
    }

    /// Streaming variant of [`Self::generate`]. Hits
    /// `:streamGenerateContent?alt=sse` and invokes `on_chunk(delta,
    /// cumulative)` once per SSE frame, where `delta` is the new text
    /// just received and `cumulative` is the full response text so far.
    /// Returns the same [`GenerateResponse`] shape as `generate` at the
    /// end of the stream so callers can treat the two interchangeably.
    ///
    /// OAuth (cloudcode) clients transparently fall back to the
    /// non-streaming [`Self::generate`] path — `v1internal` exposes a
    /// `:streamGenerateContent` endpoint but the response envelope
    /// differs and we don't have a tested mock for it yet. The
    /// callback simply never fires in that case; the headline status
    /// remains the coarse "calling Gemini…" the worker already emits.
    pub fn stream_generate(
        &self,
        cfg: &GenerateConfig,
        on_chunk: &mut dyn FnMut(&str, &str),
    ) -> Result<GenerateResponse, GeminiError> {
        let key = match &self.auth {
            GeminiAuth::ApiKey(k) => k.clone(),
            GeminiAuth::OAuth(_) | GeminiAuth::AntigravityOAuth(_) => {
                return self.generate(cfg);
            }
        };

        let model = if cfg.model.is_empty() {
            DEFAULT_MODEL
        } else {
            cfg.model.as_str()
        };
        let inner = build_request(cfg);

        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url, model, key,
        );
        let resp = self.http.post(&url).json(&inner).send()?;
        let status = resp.status();
        if !status.is_success() {
            let bytes = resp.bytes()?;
            return Err(GeminiError::Api {
                status: status.as_u16(),
                message: parse_error_message(&bytes),
            });
        }

        let mut cumulative = String::new();
        let mut last_usage: Option<RawUsageMetadata> = None;
        let mut sse_err: Option<GeminiError> = None;

        crate::stream_sse::for_each_sse_data(resp, |payload: &str| {
            let parsed: RawGenerateResponse = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(e) => {
                    sse_err = Some(GeminiError::InvalidResponse(e.to_string()));
                    return false;
                }
            };
            if let Some(delta_text) = parsed
                .candidates
                .first()
                .and_then(|c| c.content.parts.first())
                .map(|p| p.text.as_str())
            {
                if !delta_text.is_empty() {
                    cumulative.push_str(delta_text);
                    on_chunk(delta_text, cumulative.as_str());
                }
            }
            if let Some(u) = parsed.usage_metadata {
                last_usage = Some(u);
            }
            true
        })
        .map_err(|e| GeminiError::InvalidResponse(format!("SSE read error: {e}")))?;

        if let Some(e) = sse_err {
            return Err(e);
        }
        // Mirror the OpenAI streaming path: a whitespace-only response
        // is just as useless as an empty one (no parser anchors land,
        // the repair loop has nothing to feed back), so treat them the
        // same.
        if cumulative.trim().is_empty() {
            return Err(GeminiError::EmptyResponse);
        }

        let usage = last_usage
            .map(|u| Usage {
                prompt_tokens: u.prompt_token_count,
                response_tokens: u.candidates_token_count,
                total_tokens: u.total_token_count,
                cached_tokens: u.cached_content_token_count,
            })
            .unwrap_or_default();

        if let Some(budget) = cfg.budget_tokens {
            if usage.total_tokens > budget {
                return Err(GeminiError::BudgetExceeded {
                    used: usage.total_tokens,
                    budget,
                });
            }
        }

        Ok(GenerateResponse {
            text: cumulative,
            usage,
        })
    }

    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, GeminiError> {
        let model = if cfg.model.is_empty() {
            DEFAULT_MODEL
        } else {
            cfg.model.as_str()
        };

        let inner = build_request(cfg);

        let parsed: RawGenerateResponse = match &self.auth {
            GeminiAuth::ApiKey(key) => {
                let url = format!(
                    "{}/models/{}:generateContent?key={}",
                    self.base_url, model, key
                );
                let resp = self.http.post(&url).json(&inner).send()?;
                let status = resp.status();
                let bytes = resp.bytes()?;
                if !status.is_success() {
                    return Err(GeminiError::Api {
                        status: status.as_u16(),
                        message: parse_error_message(&bytes),
                    });
                }
                serde_json::from_slice(&bytes)
                    .map_err(|e| GeminiError::InvalidResponse(e.to_string()))?
            }
            GeminiAuth::OAuth(_) | GeminiAuth::AntigravityOAuth(_) => {
                let project = self
                    .oauth_project_id()
                    .ok_or_else(|| GeminiError::OAuth("missing project id in token bundle".into()))?;
                // Cloud Code Assist takes BARE model names ("gemini-3-pro-preview"),
                // never "models/...". `wrap_body` strips the prefix defensively
                // but pass it bare here too so the URL/body shape matches what
                // the Antigravity desktop client sends.
                let url = google_oauth::cloudcode::generate_content_url(&self.base_url);
                let body = google_oauth::cloudcode::wrap_body(&project, model, inner);
                let bytes = self.oauth_post_with_retry(&url, &body)?;

                // Cloud Code Assist wraps the response in `{ response: {...} }`.
                let envelope: RawGenerateEnvelope = serde_json::from_slice(&bytes)
                    .map_err(|e| GeminiError::InvalidResponse(e.to_string()))?;
                envelope
                    .response
                    .ok_or_else(|| GeminiError::InvalidResponse(
                        "v1internal response missing `response` field".into(),
                    ))?
            }
        };

        let text = parsed
            .candidates
            .first()
            .and_then(|c| c.content.parts.first())
            .map(|p| p.text.clone())
            .ok_or(GeminiError::EmptyResponse)?;

        let usage = parsed
            .usage_metadata
            .map(|u| Usage {
                prompt_tokens: u.prompt_token_count,
                response_tokens: u.candidates_token_count,
                total_tokens: u.total_token_count,
                cached_tokens: u.cached_content_token_count,
            })
            .unwrap_or_default();

        if let Some(budget) = cfg.budget_tokens {
            if usage.total_tokens > budget {
                return Err(GeminiError::BudgetExceeded {
                    used: usage.total_tokens,
                    budget,
                });
            }
        }

        Ok(GenerateResponse { text, usage })
    }

    /// Create a `cachedContents` resource holding `system_instruction`. The
    /// returned resource name can then be passed via
    /// [`GenerateConfig::cached_content`] on subsequent `generateContent`
    /// calls so Gemini bills cached input tokens at a reduced rate.
    ///
    /// In OAuth mode this returns [`GeminiError::CacheUnavailableOverOAuth`]
    /// — `v1internal` does not expose `cachedContents`, so the caller falls
    /// back to inline system-instruction (the existing fallback path).
    pub fn create_cached_content(
        &self,
        model: &str,
        system_instruction: &str,
        ttl_seconds: u64,
    ) -> Result<CachedContent, GeminiError> {
        let key = match &self.auth {
            GeminiAuth::ApiKey(k) => k,
            GeminiAuth::OAuth(_) | GeminiAuth::AntigravityOAuth(_) => {
                return Err(GeminiError::CacheUnavailableOverOAuth)
            }
        };

        let url = format!("{}/cachedContents?key={}", self.base_url, key);
        let body = serde_json::json!({
            "model": format!("models/{}", model.strip_prefix("models/").unwrap_or(model)),
            "contents": [ { "role": "user", "parts": [{ "text": "." }] } ],
            "systemInstruction": { "parts": [{ "text": system_instruction }] },
            "ttl": format!("{ttl_seconds}s"),
        });

        let resp = self.http.post(&url).json(&body).send()?;
        let status = resp.status();
        let bytes = resp.bytes()?;

        if !status.is_success() {
            let message = parse_error_message(&bytes);
            return Err(GeminiError::Api { status: status.as_u16(), message });
        }

        let parsed: RawCachedContent = serde_json::from_slice(&bytes)
            .map_err(|e| GeminiError::InvalidResponse(e.to_string()))?;

        let name = parsed
            .name
            .ok_or_else(|| GeminiError::InvalidResponse("cachedContents: missing name".into()))?;

        let now = now_unix();

        Ok(CachedContent {
            name,
            expires_at_unix: now.saturating_add(ttl_seconds),
            token_count: parsed.usage_metadata.and_then(|u| u.total_token_count),
        })
    }
}

fn build_http() -> reqwest::blocking::Client {
    // `gemini-pro-latest` with a large system instruction or thinking
    // enabled can easily push past a couple of minutes end-to-end. The
    // short connect_timeout fails fast when the user is offline so they
    // don't sit through the 600s overall budget.
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(600))
        .build()
        .expect("reqwest client")
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// -- request/response wire types ---------------------------------------------

fn build_request(cfg: &GenerateConfig) -> serde_json::Value {
    let mut contents: Vec<serde_json::Value> = Vec::new();

    // Gemini rejects requests that set `systemInstruction` alongside
    // `cachedContent` ("CachedContent can not be used with GenerateContent
    // request setting system_instruction, tools or tool_config"). The cache
    // resource carries the stable reference block (grammar, kinds, attribute
    // allowlist); the inline dynamic prefix (preamble, conventions, fewshots,
    // stdlib summary, output contract) still has to reach the model fresh
    // per call. When both are present, inject the inline portion as a
    // synthetic user/model preamble at the head of `contents` so the model
    // sees the same total instruction without tripping the API constraint.
    let inline_in_contents = cfg.cached_content.is_some() && cfg.system_instruction.is_some();
    if inline_in_contents {
        let sys = cfg.system_instruction.as_deref().unwrap_or("");
        contents.push(serde_json::json!({
            "role": "user",
            "parts": [{ "text": sys }],
        }));
        contents.push(serde_json::json!({
            "role": "model",
            "parts": [{ "text": "Understood." }],
        }));
    }

    for turn in &cfg.history {
        contents.push(serde_json::json!({
            "role": turn.role.gemini_str(),
            "parts": [{ "text": turn.text }],
        }));
    }

    // Build the user turn's parts list: optional images first (Gemini's docs
    // recommend image-before-text ordering for vision prompts), then the text.
    // The text part is always present even when empty, so the request is
    // well-formed when only images were supplied.
    let mut user_parts: Vec<serde_json::Value> = Vec::with_capacity(cfg.user_images.len() + 1);
    for img in &cfg.user_images {
        user_parts.push(serde_json::json!({
            "inline_data": {
                "mime_type": img.mime_type,
                "data": STANDARD.encode(&img.data),
            }
        }));
    }
    user_parts.push(serde_json::json!({ "text": cfg.user_prompt }));
    contents.push(serde_json::json!({
        "role": "user",
        "parts": user_parts,
    }));

    let mut req = serde_json::json!({ "contents": contents });

    if let Some(cached) = &cfg.cached_content {
        req["cachedContent"] = serde_json::Value::String(cached.clone());
    }
    // Only emit `systemInstruction` when there is no cached resource; with a
    // cache, the inline portion was already moved into `contents` above.
    if !inline_in_contents {
        if let Some(sys) = &cfg.system_instruction {
            req["systemInstruction"] = serde_json::json!({
                "parts": [{ "text": sys }],
            });
        }
    }

    let mut gen_cfg = serde_json::Map::new();
    if let Some(t) = cfg.temperature {
        gen_cfg.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(s) = cfg.seed {
        // Gemini accepts `seed` as an i32 — saturate to the valid range.
        let clipped = (s as i64) & 0x7FFF_FFFF;
        gen_cfg.insert("seed".into(), serde_json::json!(clipped));
    }
    if let Some(level) = cfg.thinking_level {
        gen_cfg.insert(
            "thinkingConfig".into(),
            serde_json::json!({ "thinkingBudget": level.budget() }),
        );
    }
    if !gen_cfg.is_empty() {
        req["generationConfig"] = serde_json::Value::Object(gen_cfg);
    }

    req
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

pub(crate) fn parse_error_message(bytes: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if let Some(msg) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return msg.to_string();
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawGenerateResponse {
    #[serde(default)]
    candidates: Vec<RawCandidate>,
    #[serde(default)]
    usage_metadata: Option<RawUsageMetadata>,
}

/// Cloud Code Assist `:generateContent` wraps the public API's response
/// under a top-level `response` key. API-key callers parse
/// `RawGenerateResponse` directly; OAuth callers go through this envelope
/// first and unwrap.
#[derive(Debug, Deserialize)]
struct RawGenerateEnvelope {
    #[serde(default)]
    response: Option<RawGenerateResponse>,
}

#[derive(Debug, Deserialize)]
struct RawCandidate {
    content: RawContent,
}

#[derive(Debug, Deserialize, Default)]
struct RawContent {
    #[serde(default)]
    parts: Vec<RawPart>,
}

#[derive(Debug, Deserialize)]
struct RawPart {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RawUsageMetadata {
    #[serde(default)]
    prompt_token_count: u32,
    #[serde(default)]
    candidates_token_count: u32,
    #[serde(default)]
    total_token_count: u32,
    #[serde(default)]
    cached_content_token_count: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCachedContent {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    usage_metadata: Option<RawCachedMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCachedMetadata {
    #[serde(default)]
    total_token_count: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_includes_system_instruction() {
        let mut cfg = GenerateConfig::new("hello");
        cfg.system_instruction = Some("be helpful".into());
        let body = build_request(&cfg);
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be helpful");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hello");
    }

    #[test]
    fn cached_content_moves_inline_block_into_contents() {
        // Gemini rejects `systemInstruction` + `cachedContent` on the same
        // request. The wire format must therefore drop `systemInstruction`
        // and instead seed `contents` with a synthetic user/model preamble
        // carrying the inline portion. The cache holds the stable block;
        // contents carry the dynamic prefix followed by the actual user turn.
        let mut cfg = GenerateConfig::new("real prompt");
        cfg.system_instruction = Some("inline portion".into());
        cfg.cached_content = Some("cachedContents/abc123".into());
        let body = build_request(&cfg);
        assert_eq!(body["cachedContent"], "cachedContents/abc123");
        assert!(
            body.get("systemInstruction").is_none(),
            "systemInstruction must not be sent alongside cachedContent",
        );
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "inline portion");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"][0]["text"], "real prompt");
    }

    #[test]
    fn cached_content_with_inline_preserves_history_alternation() {
        // History turns must still appear between the synthetic preamble and
        // the final user prompt, in order, so the model sees:
        //   user(inline) -> model(ack) -> user(history0) -> model(history1) -> user(prompt)
        let mut cfg = GenerateConfig::new("fix it");
        cfg.system_instruction = Some("inline portion".into());
        cfg.cached_content = Some("cachedContents/abc123".into());
        cfg.history.push(Turn { role: Role::User, text: "make a chair".into() });
        cfg.history.push(Turn { role: Role::Model, text: "scene { box }".into() });
        let body = build_request(&cfg);
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 5);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"][0]["text"], "make a chair");
        assert_eq!(contents[3]["role"], "model");
        assert_eq!(contents[4]["parts"][0]["text"], "fix it");
    }

    #[test]
    fn cached_content_alone_omits_system_instruction() {
        // Belt-and-braces: when only `cached_content` is set (e.g. a caller
        // pinned a pre-split cache resource that already carries the full
        // instruction), don't synthesize an empty `systemInstruction` field.
        let mut cfg = GenerateConfig::new("x");
        cfg.cached_content = Some("cachedContents/abc123".into());
        let body = build_request(&cfg);
        assert_eq!(body["cachedContent"], "cachedContents/abc123");
        assert!(body.get("systemInstruction").is_none());
    }

    #[test]
    fn history_is_threaded_before_current_prompt() {
        let mut cfg = GenerateConfig::new("fix the diagnostics");
        cfg.history.push(Turn { role: Role::User, text: "make a chair".into() });
        cfg.history.push(Turn { role: Role::Model, text: "scene { box }".into() });
        let body = build_request(&cfg);
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["parts"][0]["text"], "fix the diagnostics");
    }

    #[test]
    fn new_defaults_temperature_to_low_variance() {
        let cfg = GenerateConfig::new("x");
        assert_eq!(cfg.temperature, Some(DEFAULT_TEMPERATURE));
        let body = build_request(&cfg);
        assert!(
            (body["generationConfig"]["temperature"].as_f64().unwrap()
                - DEFAULT_TEMPERATURE as f64)
                .abs()
                < 1e-6
        );
    }

    #[test]
    fn seed_and_temperature_land_in_generation_config() {
        let mut cfg = GenerateConfig::new("x");
        cfg.seed = Some(42);
        cfg.temperature = Some(0.3);
        let body = build_request(&cfg);
        assert_eq!(body["generationConfig"]["seed"], 42);
        assert!((body["generationConfig"]["temperature"].as_f64().unwrap() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn new_defaults_thinking_to_high() {
        let cfg = GenerateConfig::new("x");
        assert_eq!(cfg.thinking_level, Some(ThinkingLevel::High));
        let body = build_request(&cfg);
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            ThinkingLevel::High.budget()
        );
    }

    #[test]
    fn thinking_none_omits_thinking_config() {
        let mut cfg = GenerateConfig::new("x");
        cfg.thinking_level = None;
        let body = build_request(&cfg);
        assert!(body["generationConfig"].get("thinkingConfig").is_none());
    }

    #[test]
    fn thinking_level_budgets_are_monotonic_and_in_range() {
        let budgets = [
            ThinkingLevel::Low.budget(),
            ThinkingLevel::Medium.budget(),
            ThinkingLevel::High.budget(),
            ThinkingLevel::XHigh.budget(),
        ];
        for w in budgets.windows(2) {
            assert!(w[0] < w[1], "levels must strictly increase");
        }
        // Gemini 2.5 Flash max is 24576; Pro min is 128.
        assert!(*budgets.first().unwrap() >= 128);
        assert!(*budgets.last().unwrap() <= 24576);
    }

    #[test]
    fn thinking_level_parse_accepts_case_and_aliases() {
        assert_eq!(ThinkingLevel::parse("low"), Some(ThinkingLevel::Low));
        assert_eq!(ThinkingLevel::parse("MEDIUM"), Some(ThinkingLevel::Medium));
        assert_eq!(ThinkingLevel::parse("Med"), Some(ThinkingLevel::Medium));
        assert_eq!(ThinkingLevel::parse(" High "), Some(ThinkingLevel::High));
        assert_eq!(ThinkingLevel::parse("xhigh"), Some(ThinkingLevel::XHigh));
        assert_eq!(ThinkingLevel::parse("x-high"), Some(ThinkingLevel::XHigh));
        assert_eq!(ThinkingLevel::parse("x_high"), Some(ThinkingLevel::XHigh));
        assert_eq!(ThinkingLevel::parse("wat"), None);
    }

    #[test]
    fn parses_error_message_from_api_body() {
        let json = br#"{"error":{"message":"API key invalid","status":"INVALID_ARGUMENT"}}"#;
        assert_eq!(parse_error_message(json), "API key invalid");
    }

    #[test]
    fn parse_error_falls_back_to_raw_body() {
        assert_eq!(parse_error_message(b"oh no"), "oh no");
    }

    #[test]
    fn from_oauth_uses_bundle_endpoint_base() {
        let bundle = OAuthBundle {
            access_token: "a".into(),
            refresh_token: "r".into(),
            access_expires_at_unix: u64::MAX,
            obtained_at_unix: 0,
            email: None,
            project_id: Some("proj".into()),
            managed_project_id: None,
            endpoint_base: Some("https://daily-cloudcode-pa.sandbox.googleapis.com".into()),
            scope: None,
        };
        let client = GeminiClient::from_oauth(bundle);
        assert!(client.is_oauth());
        assert_eq!(
            client.base_url(),
            "https://daily-cloudcode-pa.sandbox.googleapis.com"
        );
        assert_eq!(client.oauth_project_id().as_deref(), Some("proj"));
    }

    #[test]
    fn create_cached_content_unavailable_in_oauth_mode() {
        let bundle = OAuthBundle {
            access_token: "a".into(),
            refresh_token: "r".into(),
            access_expires_at_unix: u64::MAX,
            obtained_at_unix: 0,
            email: None,
            project_id: Some("proj".into()),
            managed_project_id: None,
            endpoint_base: None,
            scope: None,
        };
        let client = GeminiClient::from_oauth(bundle);
        let err = client
            .create_cached_content("gemini-pro-latest", "be helpful", 60)
            .expect_err("OAuth path must reject cachedContents");
        assert!(matches!(err, GeminiError::CacheUnavailableOverOAuth));
    }
}
