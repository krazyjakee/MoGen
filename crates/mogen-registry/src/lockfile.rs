//! `mog.lock` reader/writer.
//!
//! Format extends MoGHub's `model_versions.mog_lock` JSON with a
//! desktop-only `"resolved"` table that maps each registry-ref token
//! (`@user/slug` or `@user/slug@v`) to a concrete `(model_id, version_id,
//! version, files[])` pin. Server-side serde ignores unknown keys, so a
//! lock file written by Studio survives a republish.
//!
//! Two modes:
//! - **Honour** (default): if the lock file is consistent with the
//!   source's current registry refs, every ref resolves to the locked
//!   version — even when the source said `@user/slug` with no `@v`. Refs
//!   that appear in source but not in the lock are fetched as latest and
//!   added on save.
//! - **Strict** (`mogen build --frozen`): refs missing from the lock
//!   abort the build. CI mode.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::refs::UseGraph;

/// Concrete pin for a single registry ref. `raw` is the lookup key used
/// in [`LockFile::resolved`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockResolved {
    /// Original token text, identical to [`crate::RegistryRef::raw`].
    pub raw: String,
    /// Resolved model id, as a UUID string. Stable across renames so the
    /// pin survives a published-model rename.
    pub model_id: String,
    /// Resolved version id (UUID).
    pub version_id: String,
    /// Resolved integer version. The number that ends up in
    /// `mog.lock` even when the source said `@user/slug` without `@v`.
    pub version: i32,
    /// Files that make up the resolved version, in their server order.
    /// Hashes let `--offline` builds detect cache corruption without
    /// re-fetching.
    pub files: Vec<LockResolvedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockResolvedFile {
    pub filename: String,
    pub sha256: String,
}

/// In-memory representation of a `mog.lock` file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LockFile {
    /// Local `import "x.mog"` paths. Mirrors moghub's `mog_lock.imports`.
    #[serde(default)]
    pub imports: Vec<String>,
    /// Local `use "name"` references. Mirrors moghub's `mog_lock.uses`.
    #[serde(default)]
    pub uses: Vec<String>,
    /// Cross-author refs from source (pre-resolution shape — same as
    /// what the server stamps).
    #[serde(default)]
    pub registry: Vec<serde_json::Value>,
    /// Desktop-only: resolved pins keyed by `raw` token. Server ignores
    /// this field.
    #[serde(default)]
    pub resolved: Vec<LockResolved>,
}

impl LockFile {
    /// Build a lockfile shell from a freshly-extracted use graph.
    /// `resolved` starts empty — the caller fills it as fetches complete.
    pub fn from_use_graph(g: &UseGraph) -> Self {
        Self {
            imports: g.imports.clone(),
            uses: g.uses.clone(),
            registry: g
                .registry
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "user": r.user,
                        "slug": r.slug,
                        "version": r.version,
                        "raw": r.raw,
                    })
                })
                .collect(),
            resolved: Vec::new(),
        }
    }

    /// Look up a previously-resolved pin by its `raw` ref token.
    pub fn pin_for(&self, raw: &str) -> Option<&LockResolved> {
        self.resolved.iter().find(|p| p.raw == raw)
    }

    /// Replace or insert a resolved pin. Keeps the table in stable order
    /// by `raw` so the on-disk lock has minimal churn between runs.
    pub fn upsert_pin(&mut self, pin: LockResolved) {
        if let Some(slot) = self.resolved.iter_mut().find(|p| p.raw == pin.raw) {
            *slot = pin;
        } else {
            self.resolved.push(pin);
            self.resolved.sort_by(|a, b| a.raw.cmp(&b.raw));
        }
    }

    /// True if every ref in `g.registry` has a matching pin in
    /// `self.resolved`. Used by strict / `--frozen` mode and by the
    /// honour-mode short circuit that skips network IO when the lock is
    /// already complete.
    pub fn covers(&self, g: &UseGraph) -> bool {
        g.registry
            .iter()
            .all(|r| self.resolved.iter().any(|p| p.raw == r.raw))
    }
}

/// Read a `mog.lock` from disk. Returns `Ok(None)` when the file is
/// missing — that's the normal cold-start case. IO errors and parse
/// errors propagate so the user sees them instead of silently rebuilding
/// against an empty lock.
pub fn read_lock(path: &Path) -> Result<Option<LockFile>> {
    let body = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("reading lockfile {}", path.display()));
        }
    };
    let lf: LockFile = serde_json::from_str(&body)
        .with_context(|| format!("parsing lockfile {}", path.display()))?;
    Ok(Some(lf))
}

/// Write a `mog.lock` atomically. Pretty-printed so version-controlled
/// locks diff cleanly.
pub fn write_lock(path: &Path, lock: &LockFile) -> Result<()> {
    let body = serde_json::to_string_pretty(lock)
        .with_context(|| format!("serialising lockfile {}", path.display()))?;
    let tmp = path.with_extension("lock.tmp");
    fs::write(&tmp, body.as_bytes())
        .with_context(|| format!("writing temp lockfile {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refs::extract_use_graph;

    #[test]
    fn from_use_graph_round_trip() {
        let src = r#"
            import "shared.mog"
            scene { use "@alice/chairs@2" () }
        "#;
        let ast = mogen_dsl::parse(src).unwrap();
        let g = extract_use_graph(&ast);
        let lf = LockFile::from_use_graph(&g);
        assert_eq!(lf.imports, vec!["shared.mog".to_string()]);
        assert_eq!(lf.registry.len(), 1);
        assert!(lf.resolved.is_empty());
        assert!(!lf.covers(&g));
    }

    #[test]
    fn upsert_pin_replaces_in_place() {
        let mut lf = LockFile::default();
        lf.upsert_pin(LockResolved {
            raw: "@alice/chairs".into(),
            model_id: "m".into(),
            version_id: "v1".into(),
            version: 1,
            files: vec![],
        });
        lf.upsert_pin(LockResolved {
            raw: "@alice/chairs".into(),
            model_id: "m".into(),
            version_id: "v2".into(),
            version: 2,
            files: vec![],
        });
        assert_eq!(lf.resolved.len(), 1);
        assert_eq!(lf.resolved[0].version, 2);
    }

    #[test]
    fn read_missing_lock_returns_none() {
        let scratch = tempfile::tempdir().unwrap();
        let p = scratch.path().join("nope.lock");
        assert!(read_lock(&p).unwrap().is_none());
    }

    #[test]
    fn write_then_read_round_trips() {
        let scratch = tempfile::tempdir().unwrap();
        let p = scratch.path().join("mog.lock");
        let mut lf = LockFile::default();
        lf.imports.push("a.mog".into());
        lf.upsert_pin(LockResolved {
            raw: "@alice/chairs".into(),
            model_id: "m".into(),
            version_id: "v".into(),
            version: 3,
            files: vec![LockResolvedFile {
                filename: "main.mog".into(),
                sha256: "deadbeef".into(),
            }],
        });
        write_lock(&p, &lf).unwrap();
        let back = read_lock(&p).unwrap().unwrap();
        assert_eq!(back.imports, vec!["a.mog".to_string()]);
        assert_eq!(back.resolved.len(), 1);
        assert_eq!(back.resolved[0].version, 3);
        assert_eq!(back.resolved[0].files[0].filename, "main.mog");
    }

    #[test]
    fn covers_returns_true_when_all_refs_pinned() {
        let src = r#"scene { use "@alice/chairs@2" () use "@bob/lamps" () }"#;
        let ast = mogen_dsl::parse(src).unwrap();
        let g = extract_use_graph(&ast);
        let mut lf = LockFile::from_use_graph(&g);
        assert!(!lf.covers(&g));
        lf.upsert_pin(LockResolved {
            raw: "@alice/chairs@2".into(),
            model_id: "a".into(),
            version_id: "av".into(),
            version: 2,
            files: vec![],
        });
        lf.upsert_pin(LockResolved {
            raw: "@bob/lamps".into(),
            model_id: "b".into(),
            version_id: "bv".into(),
            version: 5,
            files: vec![],
        });
        assert!(lf.covers(&g));
    }
}
