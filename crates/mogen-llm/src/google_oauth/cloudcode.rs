//! Cloud Code Assist (`v1internal`) URL/body/header construction.
//!
//! Differences from the public `generativelanguage.googleapis.com/v1beta`
//! surface:
//! - URL is `{endpoint_base}/v1internal:generateContent` (no `/models/...`).
//! - Body shape mirrors what the Antigravity desktop client sends:
//!   `{ project, model, request: { contents, ..., sessionId },
//!      requestType: "agent", userAgent: "antigravity",
//!      requestId: "agent-<uuid>" }`. Critically, `model` is on the OUTER
//!   envelope (NOT inside `request`) and uses bare names like
//!   `"gemini-3-pro-preview"` — never `"models/..."`.
//! - Auth header is `Authorization: Bearer <access_token>` — never set
//!   `x-api-key` or `x-goog-user-project` on this surface.
//! - The Antigravity surface only sets `User-Agent`. The Gemini CLI surface
//!   adds `X-Goog-Api-Client` + `Client-Metadata`; we mirror Antigravity
//!   here because the OAuth client is Antigravity-flavoured.

use super::client;

/// Build the `:generateContent` URL for an OAuth-authenticated request.
pub fn generate_content_url(endpoint_base: &str) -> String {
    format!("{}/v1internal:generateContent", endpoint_base.trim_end_matches('/'))
}

/// Build the `:streamGenerateContent?alt=sse` URL. Image generation on the
/// Cloud Code Assist surface only responds on the streaming endpoint —
/// `:generateContent` returns 404 for image models. SSE chunks each carry
/// a fragment of the response; the caller must scan every `data: {…}`
/// line for `inlineData` parts.
pub fn stream_generate_content_url(endpoint_base: &str) -> String {
    format!(
        "{}/v1internal:streamGenerateContent?alt=sse",
        endpoint_base.trim_end_matches('/')
    )
}

/// Strip a `models/` prefix from a model name. Cloud Code Assist takes
/// bare names like `"gemini-3-pro-preview"` — sending `"models/gemini-..."`
/// returns 404 (entity not found).
fn bare_model(model: &str) -> &str {
    model.strip_prefix("models/").unwrap_or(model)
}

/// Build the `:fetchAvailableModels` URL. Used at the start of a texture
/// run to learn which model IDs are actually live for the bundle's
/// project. Mirrors `fetchAvailableModels` in McKrei's
/// `opencode-antigravity-nano-banana` reference impl.
pub fn fetch_available_models_url(endpoint_base: &str) -> String {
    format!(
        "{}/v1internal:fetchAvailableModels",
        endpoint_base.trim_end_matches('/')
    )
}

/// Body for the `:fetchAvailableModels` POST. Antigravity sends just
/// `{ project }`; if the bundle hasn't been routed to a project yet the
/// surface accepts an empty object. We always have a project at this
/// point (resolved during login).
pub fn fetch_available_models_body(project: &str) -> serde_json::Value {
    serde_json::json!({ "project": project })
}

/// Parse a `:fetchAvailableModels` response and return the IDs of
/// image-capable models for the bundle's project. Filters by name —
/// any key containing `image` (case-insensitive) is kept. The surface
/// returns either `{ models: { "gemini-3.1-flash-image": {...}, ... } }`
/// (object map keyed by name) or a flat array of `{ name }` objects;
/// we accept both because we've seen both shapes during rollout.
pub fn parse_available_image_models(body: &[u8]) -> Vec<String> {
    let parsed: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    if let Some(obj) = parsed.get("models").and_then(|m| m.as_object()) {
        for key in obj.keys() {
            if key.to_ascii_lowercase().contains("image") {
                out.push(key.clone());
            }
        }
    } else if let Some(arr) = parsed.get("models").and_then(|m| m.as_array()) {
        for item in arr {
            if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                if name.to_ascii_lowercase().contains("image") {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

/// Format a v4 UUID without pulling in a `uuid` crate dep. Output looks
/// like `"550e8400-e29b-41d4-a716-446655440000"` with the version (4) and
/// variant (10) bits set per RFC 4122. Good enough for `requestId` /
/// `sessionId` — these are opaque correlation tokens, not crypto material.
fn random_uuid_v4() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant 10
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Wrap an inner public-API request body into the Antigravity Cloud Code
/// Assist envelope. `model` ends up at the OUTER level (bare, no `models/`
/// prefix); the inner request gets a generated `sessionId`. The envelope
/// also carries `requestType: "agent"` + `userAgent: "antigravity"` +
/// `requestId: "agent-<uuid>"` — Antigravity rejects calls without these.
pub fn wrap_body(project: &str, model: &str, inner: serde_json::Value) -> serde_json::Value {
    wrap_body_with_ids(project, model, inner, &random_uuid_v4(), &random_uuid_v4())
}

/// Test seam for [`wrap_body`] — lets a test pin the `requestId` and
/// `sessionId` UUIDs so JSON-shape assertions stay deterministic.
pub fn wrap_body_with_ids(
    project: &str,
    model: &str,
    inner: serde_json::Value,
    request_id: &str,
    session_id: &str,
) -> serde_json::Value {
    let mut req = inner;
    if let Some(obj) = req.as_object_mut() {
        obj.remove("model");
        obj.insert(
            "sessionId".to_string(),
            serde_json::Value::String(session_id.to_string()),
        );
    }
    serde_json::json!({
        "project": project,
        "model": bare_model(model),
        "request": req,
        "requestType": "agent",
        "userAgent": "antigravity",
        "requestId": format!("agent-{request_id}"),
    })
}

/// Image-generation envelope. Matches the
/// [`pi-nano-antigravity-image`](https://github.com/gwelinder/pi-nano-antigravity-image)
/// reference plugin shape — the image surface 404s for any other body.
/// The inner request loses `responseModalities` (image surface uses
/// `imageConfig` instead), gains a stock systemInstruction + safety
/// settings, and `candidateCount: 1`. The outer envelope is the same
/// `{project, model, request, requestType, requestId, userAgent}`
/// shape as the text path but `requestId` uses `nano-banana-<ts>-<rand>`
/// (NOT `agent-<uuid>`) and there is no `session_id`.
pub fn wrap_image_body(
    project: &str,
    model: &str,
    inner: serde_json::Value,
) -> serde_json::Value {
    wrap_image_body_with_ids(project, model, inner, &random_request_id())
}

/// Test seam — pinned `requestId` for deterministic JSON assertions.
pub fn wrap_image_body_with_ids(
    project: &str,
    model: &str,
    inner: serde_json::Value,
    request_id: &str,
) -> serde_json::Value {
    let req = build_image_request_payload(inner);
    serde_json::json!({
        "project": project,
        "model": bare_model(model),
        "request": req,
        "requestType": "agent",
        "requestId": request_id,
        "userAgent": "antigravity",
    })
}

/// Generate the `nano-banana-<unix_ms>-<rand>` requestId used by the
/// image plugin. The exact format isn't validated server-side, but
/// matching the reference impl reduces the surface for "wrong shape"
/// 404 paths.
fn random_request_id() -> String {
    use rand::Rng;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // 8-char base36 random suffix to match `Math.random().toString(36).slice(2,10)`.
    let mut rng = rand::thread_rng();
    let suffix: String = (0..8)
        .map(|_| {
            let n: u8 = rng.gen_range(0..36);
            if n < 10 {
                (b'0' + n) as char
            } else {
                (b'a' + (n - 10)) as char
            }
        })
        .collect();
    format!("nano-banana-{now_ms}-{suffix}")
}

/// Construct the inner `request` payload for image gen. Drops
/// `responseModalities` (replaced by `imageConfig`), adds a stock
/// systemInstruction, adds permissive `safetySettings`, and keeps
/// whatever `contents` and `generationConfig.seed` the caller passed.
fn build_image_request_payload(mut inner: serde_json::Value) -> serde_json::Value {
    use serde_json::json;
    let obj = inner
        .as_object_mut()
        .expect("image inner request must be a JSON object");
    obj.remove("model");

    // Wipe `responseModalities` — image surface rejects it. Keep `seed`
    // if the caller passed one (lives under `generationConfig.seed`).
    let seed = obj
        .get("generationConfig")
        .and_then(|g| g.get("seed"))
        .cloned();

    let mut gen_cfg = json!({
        "imageConfig": { "aspectRatio": "1:1", "imageSize": "2K" },
        "candidateCount": 1,
    });
    if let Some(s) = seed {
        gen_cfg["seed"] = s;
    }
    obj.insert("generationConfig".to_string(), gen_cfg);

    obj.insert(
        "systemInstruction".to_string(),
        json!({
            "parts": [{
                "text": "You are an AI image generator. Produce high-quality images and follow user instructions precisely.",
            }],
        }),
    );

    obj.insert(
        "safetySettings".to_string(),
        json!([
            { "category": "HARM_CATEGORY_HARASSMENT",        "threshold": "BLOCK_ONLY_HIGH" },
            { "category": "HARM_CATEGORY_HATE_SPEECH",       "threshold": "BLOCK_ONLY_HIGH" },
            { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "BLOCK_ONLY_HIGH" },
            { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "BLOCK_ONLY_HIGH" },
        ]),
    );

    inner
}

/// Apply the Antigravity-flavoured headers + bearer auth to a request
/// builder. Returns the modified builder for chaining.
pub fn apply_headers(
    req: reqwest::blocking::RequestBuilder,
    access_token: &str,
) -> reqwest::blocking::RequestBuilder {
    req.bearer_auth(access_token)
        .header("User-Agent", client::USER_AGENT)
}

/// Apply image-surface headers + bearer auth. The image surface on
/// `daily-cloudcode-pa.googleapis.com` (no `.sandbox.` subdomain) is
/// stricter than the text surface — it 404s with the gemini-cli UA and
/// rejects requests whose `Client-Metadata.ideType` isn't `"ANTIGRAVITY"`.
/// Mirrors the routing requirements observed in
/// `opencode-antigravity-nano-banana`: Antigravity UA,
/// `google-cloud-sdk` `X-Goog-Api-Client`, JSON-stringified
/// `Client-Metadata` with `ideType: "ANTIGRAVITY"`, and `Accept:
/// text/event-stream` for SSE streaming.
pub fn apply_image_headers(
    req: reqwest::blocking::RequestBuilder,
    access_token: &str,
) -> reqwest::blocking::RequestBuilder {
    req.bearer_auth(access_token)
        .header("User-Agent", client::IMAGE_USER_AGENT)
        .header("X-Goog-Api-Client", client::IMAGE_X_GOOG_API_CLIENT)
        .header("Client-Metadata", client::IMAGE_CLIENT_METADATA)
        .header("Accept", "text/event-stream")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_uses_v1internal_action_form() {
        let url = generate_content_url("https://cloudcode-pa.googleapis.com");
        assert_eq!(url, "https://cloudcode-pa.googleapis.com/v1internal:generateContent");
    }

    #[test]
    fn url_strips_trailing_slash() {
        let url = generate_content_url("https://cloudcode-pa.googleapis.com/");
        assert_eq!(url, "https://cloudcode-pa.googleapis.com/v1internal:generateContent");
    }

    #[test]
    fn wrap_body_puts_model_at_outer_level_bare() {
        // Cloud Code Assist returns 404 if model goes inside `request` or
        // carries the `models/` prefix. Outer-level + bare name is the
        // only shape Antigravity accepts.
        let inner = serde_json::json!({
            "contents": [ { "role": "user", "parts": [{ "text": "hi" }] } ]
        });
        let wrapped = wrap_body_with_ids(
            "proj-1",
            "models/gemini-3-pro-preview",
            inner,
            "REQ",
            "SES",
        );
        assert_eq!(wrapped["project"], "proj-1");
        assert_eq!(wrapped["model"], "gemini-3-pro-preview");
        assert!(
            wrapped["request"].get("model").is_none(),
            "model must NOT live inside request",
        );
        assert_eq!(wrapped["request"]["contents"][0]["role"], "user");
    }

    #[test]
    fn wrap_body_adds_antigravity_envelope_fields() {
        // Antigravity rejects calls without requestType/userAgent/requestId.
        // sessionId goes inside the request body for cross-call signature
        // caching.
        let inner = serde_json::json!({ "contents": [] });
        let wrapped = wrap_body_with_ids("p", "m", inner, "REQ-UUID", "SES-UUID");
        assert_eq!(wrapped["requestType"], "agent");
        assert_eq!(wrapped["userAgent"], "antigravity");
        assert_eq!(wrapped["requestId"], "agent-REQ-UUID");
        assert_eq!(wrapped["request"]["sessionId"], "SES-UUID");
    }

    #[test]
    fn wrap_body_strips_inner_model_when_present() {
        // Some callers (or repair-loop reuses) may include `model` inside
        // the inner body. wrap_body must move it to the outer envelope and
        // never duplicate it.
        let inner = serde_json::json!({
            "model": "models/gemini-stale",
            "contents": []
        });
        let wrapped = wrap_body_with_ids("p", "gemini-3-pro-preview", inner, "R", "S");
        assert_eq!(wrapped["model"], "gemini-3-pro-preview");
        assert!(wrapped["request"].get("model").is_none());
    }

    #[test]
    fn wrap_body_preserves_original_inner_keys() {
        let inner = serde_json::json!({
            "contents": [],
            "systemInstruction": { "parts": [{ "text": "be helpful" }] },
            "generationConfig": { "temperature": 0.2 }
        });
        let wrapped = wrap_body_with_ids("p", "m", inner, "R", "S");
        assert_eq!(wrapped["request"]["systemInstruction"]["parts"][0]["text"], "be helpful");
        assert_eq!(wrapped["request"]["generationConfig"]["temperature"].as_f64().unwrap(), 0.2);
    }

    #[test]
    fn wrap_image_body_strips_response_modalities_and_adds_image_config() {
        // The image surface 404s if `responseModalities` is present;
        // `imageConfig` is required instead. Covers the shape pinned by
        // the `pi-nano-antigravity-image` reference plugin.
        let inner = serde_json::json!({
            "contents": [{ "role": "user", "parts": [{ "text": "a banana" }] }],
            "generationConfig": { "responseModalities": ["IMAGE"], "seed": 42 }
        });
        let wrapped = wrap_image_body_with_ids("p", "gemini-3-pro-image", inner, "REQ-ID");
        let req = &wrapped["request"];
        let gc = &req["generationConfig"];
        assert!(gc.get("responseModalities").is_none(), "responseModalities must be stripped");
        assert_eq!(gc["imageConfig"]["aspectRatio"], "1:1");
        assert_eq!(gc["imageConfig"]["imageSize"], "2K");
        assert_eq!(gc["candidateCount"], 1);
        assert_eq!(gc["seed"], 42, "caller-provided seed must survive");
        assert!(req["systemInstruction"]["parts"][0]["text"].is_string());
        assert!(req["safetySettings"].is_array());
    }

    #[test]
    fn wrap_image_body_uses_nano_banana_request_id_and_no_session_id() {
        // Image envelope differs from the text envelope: requestId is a
        // `nano-banana-…` token (NOT `agent-<uuid>`), and there is no
        // `sessionId` (text-surface caching key, irrelevant on image).
        let inner = serde_json::json!({ "contents": [] });
        let wrapped = wrap_image_body_with_ids("p", "m", inner, "nano-banana-1700000000000-abcd1234");
        assert_eq!(wrapped["requestId"], "nano-banana-1700000000000-abcd1234");
        assert_eq!(wrapped["requestType"], "agent");
        assert_eq!(wrapped["userAgent"], "antigravity");
        assert!(wrapped["request"].get("sessionId").is_none());
    }

    #[test]
    fn random_request_id_matches_nano_banana_format() {
        let id = random_request_id();
        let prefix = "nano-banana-";
        assert!(id.starts_with(prefix));
        let tail = &id[prefix.len()..];
        let mut parts = tail.split('-');
        let ts = parts.next().expect("timestamp segment");
        let suffix = parts.next().expect("suffix segment");
        assert!(parts.next().is_none(), "exactly two segments after prefix");
        assert!(ts.chars().all(|c| c.is_ascii_digit()), "timestamp digits only");
        assert_eq!(suffix.len(), 8, "8-char base36 suffix");
        assert!(suffix.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn fetch_available_models_url_uses_v1internal_action_form() {
        let url = fetch_available_models_url("https://daily-cloudcode-pa.googleapis.com");
        assert_eq!(
            url,
            "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels"
        );
    }

    #[test]
    fn fetch_available_models_url_strips_trailing_slash() {
        let url = fetch_available_models_url("https://daily-cloudcode-pa.googleapis.com/");
        assert_eq!(
            url,
            "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels"
        );
    }

    #[test]
    fn fetch_available_models_body_carries_project() {
        let body = fetch_available_models_body("proj-1");
        assert_eq!(body["project"], "proj-1");
    }

    #[test]
    fn parse_available_image_models_picks_image_named_from_object_map() {
        // Catalog can come back as an object keyed by model name, with
        // arbitrary metadata under each key. Filter must pick keys whose
        // name contains "image" (case-insensitive) and ignore the rest.
        let body = br#"{
            "models": {
                "gemini-3.1-flash-image": { "displayName": "Gemini 3.1 Flash Image" },
                "gemini-2.5-pro": { "displayName": "Gemini 2.5 Pro" },
                "gemini-3-PRO-image": { "displayName": "case-mixed" }
            }
        }"#;
        let mut got = parse_available_image_models(body);
        got.sort();
        assert_eq!(got, vec!["gemini-3-PRO-image", "gemini-3.1-flash-image"]);
    }

    #[test]
    fn parse_available_image_models_picks_image_named_from_array() {
        // Some surfaces return a flat `[{name}]` array shape instead.
        let body = br#"{
            "models": [
                { "name": "gemini-3.1-flash-image" },
                { "name": "gemini-pro-latest" },
                { "name": "gemini-3-pro-image" }
            ]
        }"#;
        let mut got = parse_available_image_models(body);
        got.sort();
        assert_eq!(got, vec!["gemini-3-pro-image", "gemini-3.1-flash-image"]);
    }

    #[test]
    fn parse_available_image_models_returns_empty_on_invalid_json() {
        // Non-JSON / corrupt body → empty list (caller falls back to
        // static `ANTIGRAVITY_IMAGE_MODELS`).
        assert!(parse_available_image_models(b"not json").is_empty());
        assert!(parse_available_image_models(b"{}").is_empty());
        assert!(parse_available_image_models(b"{\"models\": null}").is_empty());
    }

    #[test]
    fn random_uuid_v4_format_is_rfc4122() {
        let id = random_uuid_v4();
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
        // Version 4 marker
        assert_eq!(&parts[2][0..1], "4");
        // Variant 10xx — first nibble must be 8/9/a/b
        let variant_nibble = parts[3].chars().next().unwrap();
        assert!(matches!(variant_nibble, '8' | '9' | 'a' | 'b'));
    }
}
