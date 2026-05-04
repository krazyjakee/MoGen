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

/// Strip a `models/` prefix from a model name. Cloud Code Assist takes
/// bare names like `"gemini-3-pro-preview"` — sending `"models/gemini-..."`
/// returns 404 (entity not found).
fn bare_model(model: &str) -> &str {
    model.strip_prefix("models/").unwrap_or(model)
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

/// Apply the Antigravity-flavoured headers + bearer auth to a request
/// builder. Returns the modified builder for chaining.
pub fn apply_headers(
    req: reqwest::blocking::RequestBuilder,
    access_token: &str,
) -> reqwest::blocking::RequestBuilder {
    req.bearer_auth(access_token)
        .header("User-Agent", client::USER_AGENT)
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
