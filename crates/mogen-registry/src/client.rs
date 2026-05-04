//! Sync resolver trait used by the desktop CLI, MoGen Studio, and
//! MoGHub's in-process publish flow.
//!
//! Two impls live outside this crate:
//! - `mogen-moghub-client::MoghubClient` — `reqwest::blocking` against
//!   the public API. Used by the desktop CLI and Studio.
//! - moghub server's `mogen.rs` — sqlx-backed in-process resolver. Used
//!   at upload time.
//!
//! The trait is sync because the desktop callers are sync (Studio's egui
//! main loop and `mogen build`), and the wasm playground pre-fetches
//! every reachable file before invoking the resolver. Going async would
//! force every consumer onto tokio for no measured benefit.

use anyhow::Result;

use crate::refs::RegistryRef;

/// One file inside a fetched [`FetchedVersion`]. `sha256` is the same
/// hex digest the server stamps in `model_files.content_hash`; populated
/// when the server response includes it, `None` otherwise.
#[derive(Debug, Clone)]
pub struct FetchedFile {
    pub filename: String,
    pub source: String,
    pub sha256: Option<String>,
}

/// What the resolver returns for a single registry ref. Every field is
/// what we need to (a) populate the on-disk cache and (b) write a pin
/// into `mog.lock`.
#[derive(Debug, Clone)]
pub struct FetchedVersion {
    pub model_id: String,
    pub version_id: String,
    pub version: i32,
    pub files: Vec<FetchedFile>,
    /// The server's view of this version's `mog.lock` (transitive deps
    /// hoisted, etc.). Stored verbatim in the cache so the resolver can
    /// chase transitive refs without re-walking ASTs.
    pub mog_lock: Option<serde_json::Value>,
}

pub trait RegistryClient {
    /// Resolve a single `@user/slug[@v]` ref to its concrete files +
    /// mog_lock. Implementations decide whether to hit the network, the
    /// cache, or a database — only the cache layer in this crate cares
    /// about the source of truth.
    fn fetch(&self, spec: &RegistryRef) -> Result<FetchedVersion>;
}
