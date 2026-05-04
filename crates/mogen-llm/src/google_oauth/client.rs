//! OAuth client configuration.
//!
//! `mogen` ships with the public OAuth `client_id` / `client_secret` Google
//! publishes for the open-source Gemini CLI. Same values baked into
//! <https://github.com/google-gemini/gemini-cli> and reused by community
//! projects like <https://github.com/jenslys/opencode-gemini-auth>. They
//! authenticate the CLI itself, not the user — every login still goes
//! through Google's consent screen for the actual account.
//!
//! Out of the box, `mogen auth login` works with no setup. We do not read
//! these credentials from disk and there is no override mechanism — the
//! token bundle (`google_auth.json`) is the only OAuth artefact we
//! persist.
//!
//! Everything below (scopes, redirect URI, endpoint hosts, telemetry
//! headers) is non-secret and stays as `pub const`.
//!
//! Path-resolution helpers live here too so the token store
//! (`google_auth.json`) ends up in the canonical `~/.mogen/` location with
//! `~/.cache/mogen/` and `%LOCALAPPDATA%\mogen\` honoured as legacy
//! fallbacks for older installs.

use std::path::PathBuf;

/// Whitespace-separated OAuth scope list. Matches Google's Gemini CLI:
/// three scopes, no telemetry/experiments.
pub const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
                          https://www.googleapis.com/auth/userinfo.email \
                          https://www.googleapis.com/auth/userinfo.profile";

/// Public OAuth `client_id` for Google's Gemini CLI desktop client.
/// Same value baked into <https://github.com/google-gemini/gemini-cli>.
pub const CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";

/// Public OAuth `client_secret` paired with [`CLIENT_ID`]. Google ships
/// it in plain text in the open-source Gemini CLI; not a real secret in
/// the security sense, just paired identifier material.
pub const CLIENT_SECRET: &str = "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";

/// Antigravity desktop client OAuth values. Image generation on Cloud
/// Code Assist is gated behind this client — the gemini-cli OAuth client
/// gets a 403 from `:streamGenerateContent` regardless of scopes. Values
/// pulled from the public reference impl
/// <https://github.com/NoeFabris/opencode-antigravity-auth>.
pub const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub const ANTIGRAVITY_CLIENT_SECRET: &str = "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";

/// Antigravity scope set. Adds `cclog` + `experimentsandconfigs` on top
/// of the gemini-cli set. The image surface checks for these.
pub const ANTIGRAVITY_SCOPES: &str =
    "https://www.googleapis.com/auth/cloud-platform \
     https://www.googleapis.com/auth/userinfo.email \
     https://www.googleapis.com/auth/userinfo.profile \
     https://www.googleapis.com/auth/cclog \
     https://www.googleapis.com/auth/experimentsandconfigs";

/// Pinned config for one OAuth provider (gemini-cli or antigravity).
/// Centralises the per-provider knobs (id/secret/scopes/store filename) so
/// flow/refresh/store helpers stay generic.
#[derive(Clone, Copy, Debug)]
pub struct ProviderConfig {
    pub client_id: &'static str,
    pub client_secret: &'static str,
    pub scopes: &'static str,
    /// Filename used inside the resolved storage directory
    /// (`~/.mogen/<filename>` etc.) — keeps the two providers in
    /// separate files so logging into one doesn't clobber the other.
    pub token_filename: &'static str,
    /// Env var that overrides the absolute path of `token_filename`. Lets
    /// tests pin a temp file without touching the real `~/.mogen/`.
    pub token_store_env_override: &'static str,
}

/// Gemini-cli OAuth provider — the default for `mogen auth login` and
/// the only client that can talk to the text-generation surface.
pub const GEMINI_CLI_CONFIG: ProviderConfig = ProviderConfig {
    client_id: CLIENT_ID,
    client_secret: CLIENT_SECRET,
    scopes: SCOPES,
    token_filename: "google_auth.json",
    token_store_env_override: "MOGEN_TOKEN_STORE",
};

/// Antigravity OAuth provider — used by `mogen auth login --antigravity`
/// and required for image generation via OAuth (texturing, etc.). Token
/// lives next to the gemini-cli bundle in `~/.mogen/antigravity_auth.json`.
pub const ANTIGRAVITY_CONFIG: ProviderConfig = ProviderConfig {
    client_id: ANTIGRAVITY_CLIENT_ID,
    client_secret: ANTIGRAVITY_CLIENT_SECRET,
    scopes: ANTIGRAVITY_SCOPES,
    token_filename: "antigravity_auth.json",
    token_store_env_override: "MOGEN_ANTIGRAVITY_TOKEN_STORE",
};

/// Loopback redirect URI port. Fixed by Google's OAuth client registration —
/// you cannot pick another port.
pub const REDIRECT_PORT: u16 = 51121;

/// Path component of the loopback redirect URI.
pub const REDIRECT_PATH: &str = "/oauth-callback";

/// Full redirect URI as it appears in the authorize request.
pub const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";

/// Authorization endpoint (consent screen).
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Token endpoint (code exchange + refresh).
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Production Cloud Code Assist endpoint base.
pub const ENDPOINT_PROD: &str = "https://cloudcode-pa.googleapis.com";

/// Daily Cloud Code Assist endpoint base (sandbox failover).
pub const ENDPOINT_DAILY: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";

/// Autopush Cloud Code Assist endpoint base (sandbox failover).
pub const ENDPOINT_AUTOPUSH: &str = "https://autopush-cloudcode-pa.sandbox.googleapis.com";

/// Endpoint walk order for `loadCodeAssist` discovery.
pub const ENDPOINT_FALLOVER: [&str; 3] = [ENDPOINT_PROD, ENDPOINT_DAILY, ENDPOINT_AUTOPUSH];

/// Image-generation endpoint. Cloud Code Assist surface, **no `.sandbox.`
/// subdomain** — `daily-cloudcode-pa.googleapis.com` (not
/// `daily-cloudcode-pa.sandbox.googleapis.com`). The sandbox host 404s on
/// every current image-model ID; this one accepts `gemini-3.1-flash-image`
/// for Antigravity-issued bundles. Reference impl that exposed the
/// distinction: <https://github.com/McKrei/opencode-antigravity-nano-banana>.
pub const IMAGE_ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";

/// Image-endpoint failover list, walked on transport errors / 404. Daily
/// (no-sandbox) is the working primary; the two sandbox hosts and prod stay
/// here as best-effort fallbacks for upstream rotations.
pub const IMAGE_ENDPOINT_FALLOVER: [&str; 4] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
    "https://autopush-cloudcode-pa.sandbox.googleapis.com",
];

/// Current Cloud-Code-Assist-accepted Google image model IDs, ordered by
/// quality / availability preference. `gemini-3.1-flash-image` is the only
/// one observed to consistently route through `daily-cloudcode-pa…
/// :streamGenerateContent` for Antigravity bundles; the others are tried
/// as fallbacks in case Google rotates which models the surface honours.
/// Matches `IMAGE_MODEL_CANDIDATES` in McKrei's
/// `opencode-antigravity-nano-banana` (the canonical reference).
///
/// Names like `gemini-3-pro-image-preview` / `gemini-3.1-flash-image-preview`
/// look right but are not exposed on `daily-cloudcode-pa` for current
/// Antigravity-issued bundles — they 404 with "Requested entity was not
/// found." Pin the verified names below.
pub const ANTIGRAVITY_IMAGE_MODELS: [&str; 4] = [
    "gemini-3.1-flash-image",
    "gemini-3-pro-image",
    "gemini-3-flash-image",
    "gemini-2.5-flash-preview-image",
];

/// `User-Agent` string the Antigravity client uses on Cloud Code Assist
/// requests. We append `mogen-llm` so server-side telemetry can distinguish
/// our calls from the official client.
pub const USER_AGENT: &str = "gemini-cli/0.0.0 mogen-llm";

/// `X-Goog-Api-Client` header value for Cloud Code Assist requests.
pub const X_GOOG_API_CLIENT: &str = "gemini-cli/0.0.0";

/// `Client-Metadata` header value for Cloud Code Assist requests. The format
/// is `key=value,key=value`. We mirror Antigravity's `ideType=ANTIGRAVITY`
/// signal — the server expects this on `v1internal` to route the call.
pub const CLIENT_METADATA: &str =
    "ideType=ANTIGRAVITY,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI";

/// `User-Agent` for the image-generation surface. The image surface on
/// `daily-cloudcode-pa.sandbox.googleapis.com` is stricter than the text
/// surface — it 404s with the gemini-cli UA. Match what the
/// Antigravity desktop client sends instead.
///
/// **Note:** the server gates on a minimum Antigravity version and replies
/// with a refusal text-part ("This version of Antigravity is no longer
/// supported. Please upgrade…") for stale UAs. Bump this constant when the
/// upstream client ships a new release.
pub const IMAGE_USER_AGENT: &str = "antigravity/1.23.2 darwin/arm64";

/// `X-Goog-Api-Client` for the image surface — also strict.
pub const IMAGE_X_GOOG_API_CLIENT: &str = "google-cloud-sdk vscode_cloudshelleditor/0.1";

/// `Client-Metadata` for the image surface. JSON-stringified (NOT the
/// `key=value` form the text surface uses) — the image plugin sends
/// `{ideType, platform, pluginType}` as a serialised object. `ideType`
/// **must** be `"ANTIGRAVITY"` — `"IDE_UNSPECIFIED"` lands you on the
/// sandbox host that 404s every model ID.
pub const IMAGE_CLIENT_METADATA: &str =
    r#"{"ideType":"ANTIGRAVITY","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#;

/// Minimum slack between `now` and `expires_at_unix` before we eagerly
/// refresh. Matches the Antigravity client's 60 s buffer.
pub const TOKEN_EXPIRY_BUFFER_SECS: u64 = 60;

/// Whether [`resolve_user_path`] is being asked for a path to *read* an
/// existing file (try legacy locations) or to *write* a new file (always
/// use the canonical `~/.mogen/` location, regardless of what's on disk).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathMode {
    Read,
    Write,
}

/// Walk all candidate `mogen`-owned directories, returning every existing
/// `filename` location. Used by `mogen auth logout` to clean up legacy
/// installs that wrote tokens to `~/.cache/mogen/` or `%LOCALAPPDATA%`
/// before the move to `~/.mogen/`.
pub fn all_existing_user_paths(filename: &str, file_override_var: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var(file_override_var) {
        if !p.trim().is_empty() {
            let path = PathBuf::from(p);
            if path.exists() {
                out.push(path);
            }
        }
    }
    if let Ok(dir) = std::env::var("MOGEN_CACHE_DIR") {
        if !dir.trim().is_empty() {
            let path = PathBuf::from(dir).join(filename);
            if path.exists() && !out.contains(&path) {
                out.push(path);
            }
        }
    }
    let home_candidate = std::env::var("HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    if let Some(home) = home_candidate.as_deref() {
        for dir in [".mogen", ".cache/mogen"] {
            let mut p = PathBuf::from(home);
            for seg in dir.split('/') {
                p.push(seg);
            }
            p.push(filename);
            if p.exists() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    if let Ok(localapp) = std::env::var("LOCALAPPDATA") {
        if !localapp.trim().is_empty() {
            let p = PathBuf::from(localapp).join("mogen").join(filename);
            if p.exists() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Resolve a `mogen`-owned file path. In [`PathMode::Read`] mode, prefers
/// the canonical `~/.mogen/{filename}` but falls back to legacy
/// `~/.cache/mogen/` and `%LOCALAPPDATA%\mogen\` if the canonical file
/// doesn't exist yet (lets existing installs keep working after the move
/// to `~/.mogen/`). In [`PathMode::Write`] mode, skips the existence
/// checks and goes straight to the canonical target so new logins always
/// land in `~/.mogen/`.
///
/// `file_override_var` (e.g. `MOGEN_TOKEN_STORE`) supplies an absolute
/// path override; `MOGEN_CACHE_DIR` overrides the directory. Both are
/// honoured in either mode.
pub fn resolve_user_path(
    filename: &str,
    file_override_var: &str,
    mode: PathMode,
) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(file_override_var) {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if let Ok(dir) = std::env::var("MOGEN_CACHE_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join(filename));
        }
    }
    let home_candidate = std::env::var("HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    if mode == PathMode::Read {
        if let Some(home) = home_candidate.as_deref() {
            let dotdir = PathBuf::from(home).join(".mogen").join(filename);
            if dotdir.exists() {
                return Some(dotdir);
            }
            let legacy = PathBuf::from(home).join(".cache").join("mogen").join(filename);
            if legacy.exists() {
                return Some(legacy);
            }
        }
        if let Ok(localapp) = std::env::var("LOCALAPPDATA") {
            if !localapp.trim().is_empty() {
                let legacy = PathBuf::from(localapp).join("mogen").join(filename);
                if legacy.exists() {
                    return Some(legacy);
                }
            }
        }
    }
    // Canonical write target (also the read-mode fallback when nothing is
    // on disk yet): `~/.mogen/{filename}`.
    if let Some(home) = home_candidate {
        return Some(PathBuf::from(home).join(".mogen").join(filename));
    }
    if let Ok(localapp) = std::env::var("LOCALAPPDATA") {
        if !localapp.trim().is_empty() {
            return Some(PathBuf::from(localapp).join("mogen").join(filename));
        }
    }
    None
}
