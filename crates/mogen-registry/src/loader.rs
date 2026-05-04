//! [`mogen_dsl::module::Loader`] impl that resolves both filesystem
//! imports and `@user/slug[@v]` registry refs through a
//! [`RegistryClient`] + on-disk cache.
//!
//! Used by `mogen build` and MoGen Studio. The server-side validator
//! plugs its own sqlx-backed `RegistryClient` into a separate loader
//! that doesn't need the cache layer (everything's already in the DB).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mogen_dsl::module::{FsLoader, LoadedFile, Loader as DslLoader, RegistrySpec};

use crate::cache::{cache_root, is_complete, mark_complete, version_dir_in, write_atomic};
use crate::client::{FetchedVersion, RegistryClient};
use crate::lockfile::{LockFile, LockResolved, LockResolvedFile};
use crate::refs::RegistryRef;

fn registry_ref_from_spec(spec: &RegistrySpec) -> RegistryRef {
    RegistryRef {
        user: spec.user.clone(),
        slug: spec.slug.clone(),
        version: spec.version,
        raw: spec.raw.clone(),
    }
}

/// Strictness mode for the resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Honour locked pins; fetch + add new refs as latest. Default.
    Honour,
    /// Refs missing from the lock are an error. CI / `--frozen` mode.
    Strict,
    /// Refuse any network call; fail if cache + lock don't already cover
    /// every ref. Used by `mogen build --offline`.
    Offline,
}

/// A composing loader. Local filesystem imports go through an inner
/// [`FsLoader`]. Registry refs go through the supplied
/// [`RegistryClient`] + cache.
///
/// The lockfile, when present, is the source of truth: refs that have a
/// pin in the lock resolve to the locked version even when the source
/// said `@user/slug` without `@v`. New refs encountered in source get
/// fetched as latest, added to `lock_dirty`, and written back when the
/// caller saves the lock.
pub struct RegistryLoader<'a, C: RegistryClient> {
    client: &'a C,
    fs: FsLoader,
    /// Where fetched versions live on disk. Resolves to
    /// [`crate::cache_root`] for production callers; tests pass a
    /// scratch tempdir to avoid racing on the global env var.
    cache_root: PathBuf,
    /// In-memory cache of fetched versions, keyed by (user, slug,
    /// version). Lets a single build resolve transitive deps without
    /// hitting the cache layer N times.
    fetched: HashMap<(String, String, i32), FetchedVersion>,
    /// Initial lock state. Pins resolve from here whenever they cover
    /// the ref. Mutations land in [`Self::lock_after`] so the caller can
    /// detect "lock changed during build" and decide whether to write
    /// back.
    lock_before: LockFile,
    /// Lock state after the build. Includes any newly-fetched pins.
    pub lock_after: LockFile,
    /// True if any pin was added or refreshed during this build.
    pub lock_dirty: bool,
    mode: LockMode,
}

impl<'a, C: RegistryClient> RegistryLoader<'a, C> {
    /// Construct a loader rooted at the platform cache (or
    /// `MOGEN_CACHE_DIR`).
    pub fn new(client: &'a C, lock: LockFile, mode: LockMode) -> Self {
        Self::with_cache_root(client, lock, mode, cache_root())
    }

    /// Construct a loader rooted at a caller-supplied cache directory.
    /// Used in tests and by callers that want a private cache (e.g. a
    /// per-project cache override).
    pub fn with_cache_root(
        client: &'a C,
        lock: LockFile,
        mode: LockMode,
        cache_root: PathBuf,
    ) -> Self {
        Self {
            client,
            fs: FsLoader::new(),
            cache_root,
            fetched: HashMap::new(),
            lock_before: lock.clone(),
            lock_after: lock,
            lock_dirty: false,
            mode,
        }
    }

    /// Resolve a single registry ref. Looks at (in order): in-memory
    /// fetched cache → existing lock pin → on-disk version cache → live
    /// network fetch. Honours `LockMode::Strict` and `Offline`.
    fn resolve(&mut self, spec: &RegistrySpec) -> Result<FetchedVersion> {
        // Lock pin wins when it exists. Even if `spec.version` is
        // `None`, a previous build's lock entry pins us to the same
        // integer version so re-builds are reproducible.
        if let Some(pin) = self.lock_before.pin_for(&spec.raw) {
            let key = (spec.user.clone(), spec.slug.clone(), pin.version);
            if let Some(fv) = self.fetched.get(&key) {
                return Ok(fv.clone());
            }
            if let Some(fv) = self.try_load_from_disk(&spec.user, &spec.slug, pin.version)? {
                self.fetched.insert(key, fv.clone());
                return Ok(fv);
            }
            if matches!(self.mode, LockMode::Offline) {
                anyhow::bail!(
                    "use \"{}\" — cache miss for locked version {} ({}/{}); offline mode \
                     refuses to fetch. Re-run without --offline once.",
                    spec.raw,
                    pin.version,
                    spec.user,
                    spec.slug,
                );
            }
            // Cache cold but lock present: fetch the pinned version.
            let pinned_ref = RegistryRef {
                user: spec.user.clone(),
                slug: spec.slug.clone(),
                version: Some(pin.version),
                raw: spec.raw.clone(),
            };
            let fv = self.client.fetch(&pinned_ref)?;
            cache_fetched_version_in(&self.cache_root, &spec.user, &spec.slug, &fv)?;
            self.fetched.insert(key, fv.clone());
            return Ok(fv);
        }

        if matches!(self.mode, LockMode::Strict) {
            anyhow::bail!(
                "use \"{}\" is not pinned in mog.lock; run `mogen update` to refresh, \
                 or remove --frozen.",
                spec.raw
            );
        }
        if matches!(self.mode, LockMode::Offline) {
            anyhow::bail!(
                "use \"{}\" has no lock pin and offline mode refuses to fetch. \
                 Run a fresh `mogen build` once to populate mog.lock.",
                spec.raw
            );
        }

        // First time we've seen this ref: fetch latest, write to cache,
        // record a pin in the lock.
        let registry_ref = registry_ref_from_spec(spec);
        let fv = self.client.fetch(&registry_ref)?;
        cache_fetched_version_in(&self.cache_root, &spec.user, &spec.slug, &fv)?;
        let key = (spec.user.clone(), spec.slug.clone(), fv.version);
        self.fetched.insert(key, fv.clone());

        let pin = LockResolved {
            raw: spec.raw.clone(),
            model_id: fv.model_id.clone(),
            version_id: fv.version_id.clone(),
            version: fv.version,
            files: fv
                .files
                .iter()
                .map(|f| LockResolvedFile {
                    filename: f.filename.clone(),
                    sha256: f.sha256.clone().unwrap_or_default(),
                })
                .collect(),
        };
        self.lock_after.upsert_pin(pin);
        self.lock_dirty = true;
        Ok(fv)
    }

    fn try_load_from_disk(
        &self,
        user: &str,
        slug: &str,
        version: i32,
    ) -> Result<Option<FetchedVersion>> {
        let dir = version_dir_in(&self.cache_root, user, slug, version);
        if !is_complete(&dir) {
            return Ok(None);
        }
        // Read every .mog in the directory. We don't have a manifest on
        // disk yet, so the cache layout is "every file in the dir except
        // dotfiles is a model file." Filenames + bodies preserve order
        // by sort.
        let mut files = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .with_context(|| format!("reading cache dir {}", dir.display()))?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for ent in entries {
            let name = ent.file_name();
            let s = name.to_string_lossy();
            if s.starts_with('.') || s == "mog_lock.json" || s == "thumbnail.png" {
                continue;
            }
            let body = std::fs::read_to_string(ent.path())
                .with_context(|| format!("reading cached {}", ent.path().display()))?;
            files.push(crate::client::FetchedFile {
                filename: s.to_string(),
                source: body,
                sha256: None,
            });
        }
        let mog_lock = std::fs::read_to_string(dir.join("mog_lock.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        // Pull (model_id, version_id) from the existing lock pin if we
        // happen to have one — purely informational on cache loads.
        let (model_id, version_id) = self
            .lock_before
            .resolved
            .iter()
            .find(|p| p.version == version)
            .map(|p| (p.model_id.clone(), p.version_id.clone()))
            .unwrap_or_default();
        Ok(Some(FetchedVersion {
            model_id,
            version_id,
            version,
            files,
            mog_lock,
        }))
    }

}

impl<'a, C: RegistryClient> DslLoader for RegistryLoader<'a, C> {
    fn load(&mut self, spec: &str, base_dir: Option<&Path>) -> Result<LoadedFile> {
        // If `base_dir` is inside a registry version cache, route the
        // sibling-file load through the cache rather than the FS so
        // multi-file registry versions work without leaking absolute
        // paths into source. Detection is by a "registry/<u>/<s>/<v>/"
        // segment in the canonical path; we can't generally detect that
        // synthetic prefix on every platform, so for P1 we delegate
        // straight to the inner FsLoader. Multi-file registry versions
        // are a P2 concern.
        self.fs.load(spec, base_dir)
    }

    fn load_registry(&mut self, spec: &RegistrySpec) -> Result<LoadedFile> {
        let fv = self.resolve(spec)?;
        // The walker uses LoadedFile::canonical for cycle detection and
        // the `origin` stamp; synthesize a stable PathBuf under the
        // version cache directory so two calls for the same ref dedupe.
        let dir = version_dir_in(&self.cache_root, &spec.user, &spec.slug, fv.version);
        let entry = fv
            .files
            .first()
            .map(|f| f.filename.clone())
            .unwrap_or_else(|| "main.mog".to_string());
        let canonical = dir.join(&entry);
        let source = fv
            .files
            .first()
            .map(|f| f.source.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "registry ref \"{}\" resolved to a version with no files",
                    spec.raw
                )
            })?;
        Ok(LoadedFile { canonical, source })
    }
}

/// Persists every file in `fv` into the on-disk cache under
/// (`user`, `slug`, `version`) rooted at the platform cache (resolved
/// via [`crate::cache_root`]). For tests + private caches use
/// [`cache_fetched_version_in`].
pub fn cache_fetched_version(
    user: &str,
    slug: &str,
    fv: &FetchedVersion,
) -> Result<PathBuf> {
    cache_fetched_version_in(&cache_root(), user, slug, fv)
}

/// Same as [`cache_fetched_version`] but rooted at a caller-supplied
/// cache directory. Marks complete only after every write succeeds.
/// Idempotent — re-writing an already complete version directory is a
/// no-op.
pub fn cache_fetched_version_in(
    cache_root: &Path,
    user: &str,
    slug: &str,
    fv: &FetchedVersion,
) -> Result<PathBuf> {
    let dir = version_dir_in(cache_root, user, slug, fv.version);
    if is_complete(&dir) {
        return Ok(dir);
    }
    for f in &fv.files {
        write_atomic(&dir, &f.filename, f.source.as_bytes())?;
    }
    if let Some(lock) = &fv.mog_lock {
        let body = serde_json::to_vec_pretty(lock)
            .context("serialising fetched mog_lock for cache")?;
        write_atomic(&dir, "mog_lock.json", &body)?;
    }
    mark_complete(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{FetchedFile, FetchedVersion};
    use crate::refs::RegistryRef;

    /// In-memory `RegistryClient` for tests. Returns whatever fixture
    /// was registered; no network, no disk.
    struct FakeClient {
        responses: HashMap<String, FetchedVersion>,
    }
    impl RegistryClient for FakeClient {
        fn fetch(&self, spec: &RegistryRef) -> Result<FetchedVersion> {
            self.responses
                .get(&spec.raw)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no fixture for {}", spec.raw))
        }
    }

    fn fixture(version: i32, src: &str) -> FetchedVersion {
        FetchedVersion {
            model_id: format!("model-{version}"),
            version_id: format!("vid-{version}"),
            version,
            files: vec![FetchedFile {
                filename: "main.mog".to_string(),
                source: src.to_string(),
                sha256: None,
            }],
            mog_lock: None,
        }
    }

    #[test]
    fn cache_fetched_version_writes_files_and_marker() {
        let scratch = tempfile::tempdir().unwrap();
        let fv = fixture(3, r#"scene { box "b" (size=[1,1,1]) }"#);
        let dir =
            cache_fetched_version_in(scratch.path(), "alice", "chairs", &fv).unwrap();
        assert!(is_complete(&dir));
        let body = std::fs::read_to_string(dir.join("main.mog")).unwrap();
        assert!(body.contains("box"));
        // Idempotent.
        let dir2 =
            cache_fetched_version_in(scratch.path(), "alice", "chairs", &fv).unwrap();
        assert_eq!(dir, dir2);
    }

    #[test]
    fn loader_resolves_simple_registry_ref() {
        let scratch = tempfile::tempdir().unwrap();
        let mut responses = HashMap::new();
        responses.insert(
            "@alice/chairs".to_string(),
            fixture(2, r#"scene { box "leg" (size=[1, 0.1, 1]) }"#),
        );
        let client = FakeClient { responses };
        let mut loader = RegistryLoader::with_cache_root(
            &client,
            LockFile::default(),
            LockMode::Honour,
            scratch.path().to_path_buf(),
        );
        let spec = RegistrySpec {
            user: "alice".into(),
            slug: "chairs".into(),
            version: None,
            raw: "@alice/chairs".into(),
        };
        let loaded = loader.load_registry(&spec).unwrap();
        assert!(loaded.source.contains("box"));
        assert!(loader.lock_dirty, "first fetch should mark lock dirty");
        let pin = loader.lock_after.pin_for("@alice/chairs").unwrap();
        assert_eq!(pin.version, 2);
    }

    #[test]
    fn strict_mode_fails_without_pin() {
        let scratch = tempfile::tempdir().unwrap();
        let client = FakeClient {
            responses: HashMap::new(),
        };
        let mut loader = RegistryLoader::with_cache_root(
            &client,
            LockFile::default(),
            LockMode::Strict,
            scratch.path().to_path_buf(),
        );
        let spec = RegistrySpec {
            user: "alice".into(),
            slug: "chairs".into(),
            version: None,
            raw: "@alice/chairs".into(),
        };
        let err = loader.load_registry(&spec).unwrap_err().to_string();
        assert!(err.contains("not pinned"), "got: {err}");
    }

    #[test]
    fn lock_pin_wins_over_source_unversioned_ref() {
        let scratch = tempfile::tempdir().unwrap();
        // Source says `@alice/chairs` (no version); lock pins it to v2.
        let mut responses = HashMap::new();
        responses.insert(
            "@alice/chairs".to_string(),
            fixture(2, r#"scene { box "leg" (size=[1, 0.1, 1]) }"#),
        );
        let client = FakeClient { responses };
        let mut lock = LockFile::default();
        lock.upsert_pin(LockResolved {
            raw: "@alice/chairs".into(),
            model_id: "m".into(),
            version_id: "vid-2".into(),
            version: 2,
            files: vec![LockResolvedFile {
                filename: "main.mog".into(),
                sha256: String::new(),
            }],
        });
        // Pre-populate the cache so `try_load_from_disk` succeeds.
        cache_fetched_version_in(
            scratch.path(),
            "alice",
            "chairs",
            responses_lookup(&client, "@alice/chairs"),
        )
        .unwrap();
        let mut loader = RegistryLoader::with_cache_root(
            &client,
            lock,
            LockMode::Offline,
            scratch.path().to_path_buf(),
        );
        let spec = RegistrySpec {
            user: "alice".into(),
            slug: "chairs".into(),
            version: None,
            raw: "@alice/chairs".into(),
        };
        let loaded = loader.load_registry(&spec).unwrap();
        assert!(loaded.source.contains("box"));
        // Offline mode + lock pin + populated cache must NOT mark dirty.
        assert!(!loader.lock_dirty);
    }

    fn responses_lookup<'a>(c: &'a FakeClient, raw: &str) -> &'a FetchedVersion {
        c.responses.get(raw).expect("fixture")
    }

    #[test]
    fn end_to_end_lower_resolves_registry_use() {
        // Author writes `use "@alice/chairs"` against a fake registry.
        // Lowering through `RegistryLoader` should fetch the source,
        // synthesise a module under the registry token, and produce a
        // scene graph containing the imported geometry.
        let scratch = tempfile::tempdir().unwrap();
        let mut responses = HashMap::new();
        responses.insert(
            "@alice/chairs".to_string(),
            fixture(1, r#"scene { box "seat" (size=[1, 0.1, 1]) }"#),
        );
        let client = FakeClient { responses };
        let mut loader = RegistryLoader::with_cache_root(
            &client,
            LockFile::default(),
            LockMode::Honour,
            scratch.path().to_path_buf(),
        );

        let main_src = r#"scene { use "@alice/chairs" () }"#;
        let ast = mogen_dsl::parse(main_src).unwrap();
        let scene = mogen_dsl::lower::lower_with_loader(&ast, None, &mut loader).unwrap();
        assert!(
            scene.nodes.iter().any(|n| n.name == "seat"),
            "expected the @alice/chairs `seat` to land in the composed scene; got nodes: {:?}",
            scene.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        assert!(loader.lock_dirty);
        assert_eq!(loader.lock_after.pin_for("@alice/chairs").unwrap().version, 1);
    }
}
