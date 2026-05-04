//! [`mogen_registry::RegistryClient`] impl backed by [`MoghubClient`].
//!
//! The registry resolver pre-fetches every transitive dep into the
//! on-disk cache via this trait. The mapping is direct: a `RegistryRef`
//! becomes a `GET /api/m/:user/:slug` for the latest version, or — when
//! a specific version is requested — a `GET /api/m/:user/:slug/versions`
//! lookup followed by a `GET .../files/:filename` for each file.
//!
//! For latest fetches we can do everything with a single `model_detail`
//! call because the response includes `version: ModelVersion` (with
//! files inline). For pinned fetches we have to walk the version history
//! today; a per-version endpoint (planned in P2) would collapse this to
//! one call.

use anyhow::{anyhow, Result};

use mogen_registry::{
    client::{FetchedFile, FetchedVersion},
    refs::RegistryRef,
    RegistryClient,
};

use crate::MoghubClient;

impl RegistryClient for MoghubClient {
    fn fetch(&self, spec: &RegistryRef) -> Result<FetchedVersion> {
        let detail = self
            .model_detail(&spec.user, &spec.slug)
            .map_err(|e| anyhow!("fetching @{}/{}: {e}", spec.user, spec.slug))?;
        if detail.tombstoned {
            return Err(anyhow!(
                "@{}/{} was deleted by its owner; remove the `use` or pick a fork",
                spec.user,
                spec.slug
            ));
        }

        // Latest case: the model_detail response already carries the
        // latest version inline. If `spec.version` matches (or is None),
        // we're done.
        if spec.version.map_or(true, |v| v == detail.version.version) {
            return Ok(into_fetched(&detail.id, &detail.version));
        }

        // Pinned-but-not-latest: today the public API doesn't expose a
        // by-version-number GET that includes file bodies, so fall back
        // to fetching the file list via /versions and pulling each
        // file's source via /files (latest-only). That works for v=N
        // when N is latest, but for older Ns it's a known gap until the
        // P2 endpoint lands. Document the failure clearly so users can
        // upgrade with `mogen update`.
        Err(anyhow!(
            "@{}/{}@{} is not the latest version on the server (latest is {}). \
             Pinning to older versions requires a server endpoint that isn't \
             live yet — run `mogen update` to refresh the lockfile, or remove \
             the `@{}` suffix to track latest.",
            spec.user,
            spec.slug,
            spec.version.unwrap_or(-1),
            detail.version.version,
            spec.version.unwrap_or(-1),
        ))
    }
}

fn into_fetched(model_id: &str, v: &crate::dtos::ModelVersion) -> FetchedVersion {
    FetchedVersion {
        model_id: model_id.to_string(),
        version_id: v.id.clone(),
        version: v.version,
        files: v
            .files
            .iter()
            .map(|f| FetchedFile {
                filename: f.filename.clone(),
                source: f.source.clone(),
                sha256: None,
            })
            .collect(),
        mog_lock: None,
    }
}
