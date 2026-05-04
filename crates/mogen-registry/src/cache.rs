//! On-disk cache layout for fetched registry files.
//!
//! ```text
//! $MOGEN_CACHE_DIR/registry/<user>/<slug>/<version>/
//!     <filename>      # the .mog and any sibling files
//!     mog_lock.json   # copied verbatim from server
//!     thumbnail.png   # lazily fetched, for Community UI
//!     .ok             # zero-byte sentinel: tree fully fetched (atomic)
//! ```
//!
//! Versions are immutable `i32`s server-side, so the cache is
//! poisoning-safe by construction — different versions never collide.
//! The `.ok` sentinel goes down last; any version directory missing it on
//! startup is treated as cold and re-fetched. Atomic writes keep a
//! killed mid-fetch process from leaving half-written files behind.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Root cache directory. Honours `MOGEN_CACHE_DIR`; falls back to the
/// platform cache dir + `mogen` (matching what `mogen-llm` uses for its
/// own cache so a single `MOGEN_CACHE_DIR` overrides everything).
pub fn cache_root() -> PathBuf {
    if let Ok(v) = std::env::var("MOGEN_CACHE_DIR") {
        return PathBuf::from(v);
    }
    if let Some(d) = dirs_cache_dir() {
        return d.join("mogen");
    }
    PathBuf::from(".mogen-cache")
}

fn dirs_cache_dir() -> Option<PathBuf> {
    // We don't pull in the `dirs` crate just for this — every supported
    // platform either honours `XDG_CACHE_HOME` or has a well-known fall
    // back relative to `HOME`.
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg));
        }
    }
    let home = std::env::var("HOME").ok()?;
    if home.is_empty() {
        return None;
    }
    if cfg!(target_os = "macos") {
        Some(PathBuf::from(home).join("Library").join("Caches"))
    } else {
        Some(PathBuf::from(home).join(".cache"))
    }
}

/// `$MOGEN_CACHE_DIR/registry/`.
pub fn registry_dir() -> PathBuf {
    registry_dir_in(&cache_root())
}

/// Same as [`registry_dir`] but rooted at a caller-supplied directory.
/// Used in tests + by callers that want to construct a `RegistryLoader`
/// without touching the process env.
pub fn registry_dir_in(cache_root: &Path) -> PathBuf {
    cache_root.join("registry")
}

/// `$MOGEN_CACHE_DIR/registry/<user>/<slug>/<version>/`.
pub fn version_dir(user: &str, slug: &str, version: i32) -> PathBuf {
    version_dir_in(&cache_root(), user, slug, version)
}

/// Same as [`version_dir`] but rooted at a caller-supplied directory.
pub fn version_dir_in(cache_root: &Path, user: &str, slug: &str, version: i32) -> PathBuf {
    registry_dir_in(cache_root)
        .join(user)
        .join(slug)
        .join(version.to_string())
}

/// True if a version directory has been fully populated (zero-byte `.ok`
/// sentinel present). Returning `false` for a partially-written directory
/// triggers a re-fetch.
pub fn is_complete(dir: &Path) -> bool {
    dir.join(".ok").is_file()
}

/// Write a file inside `dir` atomically: write to a sibling temp file
/// then rename into place. Creates `dir` if missing. Used for every
/// fetched `.mog`, `mog_lock.json`, and the final `.ok` sentinel.
pub fn write_atomic(dir: &Path, filename: &str, contents: &[u8]) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("creating cache dir {}", dir.display()))?;
    let final_path = dir.join(filename);
    let tmp_path = dir.join(format!(".{filename}.tmp"));
    {
        let mut f = fs::File::create(&tmp_path)
            .with_context(|| format!("opening {}", tmp_path.display()))?;
        f.write_all(contents)
            .with_context(|| format!("writing {}", tmp_path.display()))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("renaming {} -> {}", tmp_path.display(), final_path.display()))?;
    Ok(())
}

/// Drop the `.ok` sentinel. Call only after every other file in the
/// version directory has been written.
pub fn mark_complete(dir: &Path) -> Result<()> {
    write_atomic(dir, ".ok", b"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_dir_in_layout() {
        let scratch = tempfile::tempdir().unwrap();
        let d = version_dir_in(scratch.path(), "alice", "chairs", 3);
        assert!(d.starts_with(scratch.path()));
        assert!(d.ends_with("registry/alice/chairs/3"));
    }

    #[test]
    fn write_atomic_then_mark_complete() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().join("v1");
        write_atomic(&dir, "main.mog", b"hello").unwrap();
        assert!(!is_complete(&dir));
        mark_complete(&dir).unwrap();
        assert!(is_complete(&dir));
        let body = std::fs::read_to_string(dir.join("main.mog")).unwrap();
        assert_eq!(body, "hello");
    }
}
