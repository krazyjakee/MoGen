//! DTO mirror of `moghub/src/dtos.rs`. Hand-written rather than
//! generated from the moghub `ts-rs` output because:
//! - Studio is in Rust, so the TS-side bindings would be the wrong
//!   target.
//! - Pulling in `chrono`/`uuid` to match the server's typed fields
//!   doubles the dep surface for zero gain — Studio displays these as
//!   strings.
//!
//! Keep type + field names identical to moghub so a grep for "ModelDetail"
//! finds both sides at once. Add new fields here whenever moghub adds
//! them; serde's default tolerance lets us run against a slightly older
//! server without breaking.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub file: String,
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub span: Option<Span>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidateRequest {
    pub source: String,
    #[serde(default)]
    pub filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidateResponse {
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSummary {
    /// UUID as a string. The server sends UUIDs serialised as RFC 4122
    /// strings, so a `String` round-trips cleanly without dragging the
    /// `uuid` crate into Studio.
    pub id: String,
    pub handle: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhoAmI {
    pub user: Option<UserSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DedupTarget {
    pub user: String,
    pub slug: String,
    pub version: i32,
    pub model_id: String,
    pub version_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFile {
    pub filename: String,
    pub is_entry: bool,
    pub bytes: i32,
    pub source: String,
    #[serde(default)]
    pub dedup_target: Option<DedupTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParentRef {
    pub user_handle: String,
    pub slug: String,
    pub version: i32,
    pub version_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelVersion {
    pub id: String,
    pub version: i32,
    pub publish_message: String,
    pub thumbnail_url: Option<String>,
    /// RFC3339 timestamp.
    pub created_at: String,
    pub files: Vec<ModelFile>,
}

/// Response shape for `GET /api/m/:user/:slug/versions/:version` — the
/// non-latest counterpart of [`ModelDetail`]. Carries the same file
/// bodies inline so a single GET round-trips a pinned version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelVersionDetail {
    pub model_id: String,
    pub user: UserSummary,
    pub slug: String,
    pub is_module: bool,
    pub tombstoned: bool,
    pub version: ModelVersion,
    /// Raw `mog.lock` JSON. Studio doesn't parse this directly; the
    /// registry resolver does.
    pub mog_lock: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDetail {
    pub id: String,
    pub user: UserSummary,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub license: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub like_count: i32,
    pub fork_count: i32,
    pub created_at: String,
    pub version: ModelVersion,
    pub parent: Option<ParentRef>,
    pub liked_by_me: bool,
    pub is_module: bool,
    pub dependent_count: i32,
    pub tombstoned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishFileInput {
    pub filename: String,
    pub source: String,
    pub is_entry: bool,
}

/// Binary asset bundled with a publish — texture PNG/JPG/WebP referenced
/// from a `.mog` material. Stored next to the `.mog` files in the asset
/// volume; `filename` must be a basename (no path separators) per the
/// moghub `decode_textures` validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishTextureInput {
    pub filename: String,
    /// Standard base64 of the raw image bytes (no `data:` prefix).
    pub bytes_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRequest {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub visibility: String,
    #[serde(default)]
    pub publish_message: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub files: Vec<PublishFileInput>,
    /// Texture/image assets shipped with the publish. Decoded server-side
    /// and written into the version directory; not validated through the
    /// mogen pipeline.
    #[serde(default)]
    pub textures: Vec<PublishTextureInput>,
    #[serde(default)]
    pub thumbnail_png_base64: Option<String>,
    #[serde(default)]
    pub parent_version_id: Option<String>,
    #[serde(default)]
    pub publish_as_module: bool,
    /// Set when republishing into an existing model the caller owns.
    /// Reuses the model_id + slug and appends a new version. Mutually
    /// exclusive with `parent_version_id` server-side. UUID as a string,
    /// matching the rest of the DTOs.
    #[serde(default)]
    pub target_model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishResponse {
    pub model_id: String,
    pub version_id: String,
    pub url_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSummary {
    pub id: String,
    pub user: UserSummary,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub license: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub like_count: i32,
    pub fork_count: i32,
    pub created_at: String,
    pub thumbnail_url: Option<String>,
    pub entry_filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverResponse {
    pub featured: Option<ModelSummary>,
    pub items: Vec<ModelSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub scenes: i64,
    pub modules: i64,
    pub creators: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub user: UserSummary,
    pub bio: String,
    pub joined_at: String,
    pub model_count: i64,
    pub like_count_total: i64,
    pub fork_count_total: i64,
    pub models: Vec<ModelSummary>,
    pub collections: Vec<CollectionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub user: UserSummary,
    pub body: String,
    pub body_html: String,
    pub created_at: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentList {
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCommentRequest {
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkList {
    pub forks: Vec<ModelSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub id: String,
    pub version: i32,
    pub publish_message: String,
    pub created_at: String,
    pub thumbnail_url: Option<String>,
    pub file_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionList {
    pub versions: Vec<VersionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LikeResponse {
    pub liked: bool,
    pub like_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionSummary {
    pub id: String,
    pub user: UserSummary,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub visibility: String,
    pub model_count: i64,
    pub cover_thumbnail_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionList {
    pub collections: Vec<CollectionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionDetail {
    pub id: String,
    pub user: UserSummary,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub visibility: String,
    pub created_at: String,
    pub updated_at: String,
    pub models: Vec<ModelSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCollectionRequest {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub visibility: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddToCollectionRequest {
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub model: ModelSummary,
    pub resolved_version: Option<i32>,
    pub resolved_version_id: Option<String>,
    pub version_constraint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyList {
    pub dependencies: Vec<DependencyEdge>,
    pub dependents: Vec<ModelSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub kind: String,
    pub created_at: String,
    pub read: bool,
    pub source_model: Option<ModelSummary>,
    pub source_version: Option<i32>,
    pub target_model: Option<ModelSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationList {
    pub items: Vec<Notification>,
    pub unread: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleSuggestion {
    pub user: String,
    pub slug: String,
    pub latest_version: i32,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAvailable {
    pub model_id: String,
    pub user: String,
    pub slug: String,
    pub version_constraint: String,
    pub pinned_version: i32,
    pub latest_version: i32,
    pub latest_version_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatesAvailable {
    pub updates: Vec<UpdateAvailable>,
}
