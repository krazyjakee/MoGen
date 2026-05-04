//! Pluggable file source for `import "..."` resolution.
//!
//! `module::imports` walks the import graph; this trait owns the I/O. The
//! desktop CLI hands it an [`FsLoader`] and reads from disk; the wasm editor
//! hands it a loader backed by an in-memory file map plus a JS callback that
//! fetches registry pins. Splitting along this seam is what keeps `mogen
//! check`, MoGHub's upload-time validator, and the in-browser preview running
//! the same resolver — see `docs/PLAN.md` "Phase 4a" in the MoGHub repo.
//!
//! The trait stays sync because the desktop and server callers are sync and
//! the wasm caller pre-fetches every reachable file before invoking the
//! resolver — the trade-off is one extra BFS pass on the wasm side in
//! exchange for keeping `lower_with_source` synchronous.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

/// Result of loading a single imported file.
#[derive(Debug, Clone)]
pub struct LoadedFile {
    /// Canonical id used for cycle detection, dedup, and as the `origin`
    /// stamped onto every node hoisted out of this file. For [`FsLoader`]
    /// this is the canonicalised absolute path; the wasm loader synthesises
    /// an opaque `PathBuf` keyed by the open tab name or the registry pin so
    /// downstream code (Studio's per-import scoping, material lookup) keeps
    /// working unchanged.
    pub canonical: PathBuf,
    /// Source text for the file.
    pub source: String,
}

/// What a [`Loader`] needs to resolve a `use "@user/slug[@v]"` registry
/// ref. Mirrors `mogen_registry::RegistryRef` but is duplicated here to
/// keep `mogen-dsl` free of a dependency on the registry crate — the
/// registry crate's loader implementation builds these and hands them
/// in. Comparison + hashing on `raw` is what cycle detection uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrySpec {
    pub user: String,
    pub slug: String,
    pub version: Option<i32>,
    /// Verbatim token from the `.mog` source — `@alice/chairs` or
    /// `@alice/chairs@3`. Used as the synthesised module name and as
    /// the cycle-detection key.
    pub raw: String,
}

/// Resolves `import "..."` directives and `use "@user/slug[@v]"` registry
/// refs. The walker in [`super::imports`] calls `load` once per import and
/// `load_registry` once per registry ref; the loader implementation decides
/// whether the spec names a filesystem path, a tab in an in-memory map, or
/// a registry pin fetched from the network.
pub trait Loader {
    /// Resolve `spec` (the literal string inside `import "..."`) against
    /// `base_dir` and return the canonical id and source of the referenced
    /// file. `base_dir` is the parent directory of the importing file when
    /// recursing; at the top level it's whatever the caller passed to
    /// [`crate::lower::lower_with_source`]. The walker dedupes and
    /// cycle-checks on the returned canonical id, so loaders must produce a
    /// stable id for the same logical file across calls.
    fn load(&mut self, spec: &str, base_dir: Option<&Path>) -> Result<LoadedFile>;

    /// Resolve a `use "@user/slug[@v]"` registry reference. Default impl
    /// errors out — the desktop [`FsLoader`] can't reach a registry.
    /// MoGen Studio and the `mogen` CLI plug a registry-aware loader
    /// (see the `mogen-registry` crate) that pre-fetches transitive deps
    /// into a local cache, then satisfies these calls synchronously.
    ///
    /// The walker's stack tracks `LoadedFile::canonical`, so registry
    /// loaders should return a synthetic but stable PathBuf — typically
    /// `registry/<user>/<slug>/<version>` — that won't collide with any
    /// filesystem path.
    fn load_registry(&mut self, _spec: &RegistrySpec) -> Result<LoadedFile> {
        bail!(
            "use \"{}\" is a cross-author registry reference, but no registry-aware loader \
             is installed; build via `mogen build` or open this file in MoGen Studio",
            _spec.raw
        )
    }
}

/// Filesystem-backed [`Loader`] used by the desktop CLI and `mogen-studio`.
/// Resolves relative specs against `base_dir`, canonicalises the resulting
/// path, and reads the file from disk.
#[derive(Default)]
pub struct FsLoader;

impl FsLoader {
    pub fn new() -> Self {
        Self
    }
}

impl Loader for FsLoader {
    fn load(&mut self, spec: &str, base_dir: Option<&Path>) -> Result<LoadedFile> {
        let resolved = resolve_path(spec, base_dir)?;
        let canonical = fs::canonicalize(&resolved).with_context(|| {
            format!("import \"{}\" — could not open {}", spec, resolved.display())
        })?;
        let source = fs::read_to_string(&canonical)
            .with_context(|| format!("reading imported file {}", canonical.display()))?;
        Ok(LoadedFile { canonical, source })
    }
}

/// Try to parse a `use` token as a cross-author registry ref. Returns
/// `None` for local/named uses (those stay in `mog_lock.uses`). Kept in
/// lockstep with `mogen_registry::parse_registry_ref` — any change to
/// the handle/slug grammar must land in both places, and the
/// `cross_crate_parser_parity` test in `mogen-registry` guards that.
pub fn parse_registry_spec(token: &str) -> Option<RegistrySpec> {
    let body = token.strip_prefix('@')?;
    let (head, version_s) = match body.rsplit_once('@') {
        Some((h, v)) => (h, Some(v)),
        None => (body, None),
    };
    let (user, slug) = head.split_once('/')?;
    if user.is_empty() || slug.is_empty() {
        return None;
    }
    if !is_handle_like(user) || !is_slug_like(slug) {
        return None;
    }
    let version = match version_s {
        Some(v) => Some(v.parse::<i32>().ok()?),
        None => None,
    };
    Some(RegistrySpec {
        user: user.to_string(),
        slug: slug.to_string(),
        version,
        raw: token.to_string(),
    })
}

fn is_handle_like(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn is_slug_like(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn resolve_path(raw: &str, base_dir: Option<&Path>) -> Result<PathBuf> {
    let p = Path::new(raw);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    let base = base_dir.ok_or_else(|| {
        anyhow!(
            "import \"{}\" is relative but no source directory is set; \
             pass an absolute path or call `lower_with_source` with the \
             importing file's directory",
            raw
        )
    })?;
    Ok(base.join(p))
}
