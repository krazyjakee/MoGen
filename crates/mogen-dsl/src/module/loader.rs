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

use anyhow::{anyhow, Context, Result};

/// Result of loading a single imported file.
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

/// Resolves `import "..."` directives. The walker in [`super::imports`] calls
/// `load` once per import; the loader implementation decides whether the spec
/// names a filesystem path, a tab in an in-memory map, or a registry pin
/// fetched from the network.
pub trait Loader {
    /// Resolve `spec` (the literal string inside `import "..."`) against
    /// `base_dir` and return the canonical id and source of the referenced
    /// file. `base_dir` is the parent directory of the importing file when
    /// recursing; at the top level it's whatever the caller passed to
    /// [`crate::lower::lower_with_source`]. The walker dedupes and
    /// cycle-checks on the returned canonical id, so loaders must produce a
    /// stable id for the same logical file across calls.
    fn load(&mut self, spec: &str, base_dir: Option<&Path>) -> Result<LoadedFile>;
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
