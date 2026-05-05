//! [`mogen_registry::RegistryClient`] impl backed by [`MoghubClient`].
//!
//! The registry resolver pre-fetches every transitive dep into the
//! on-disk cache via this trait. The mapping is direct: a `RegistryRef`
//! becomes a `GET /api/m/:user/:slug` for the latest version, or — when
//! a specific version is requested — a `GET /api/m/:user/:slug/versions`
//! lookup followed by a `GET .../files/:filename` for each file.
//!
//! For latest fetches we use `model_detail` (the response carries the
//! latest version inline). For pinned non-latest fetches we use
//! `version_detail`, which P2 added on the moghub side and which returns
//! the same `ModelVersion` shape with file bodies, so the resolver
//! handles both with one round-trip per ref.

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

        // Pinned-but-not-latest: hit the by-version-number endpoint that
        // P2 shipped. Its response shape is `ModelVersionDetail`, which
        // wraps the same `ModelVersion` we get back inline from
        // `model_detail`, so the same `into_fetched` adapter works for
        // both branches. A 410 here means the model was tombstoned
        // between the latest check and now, which we surface verbatim.
        let pinned = spec.version.unwrap_or(-1);
        let v = self
            .version_detail(&spec.user, &spec.slug, pinned)
            .map_err(|e| anyhow!("fetching @{}/{}@{}: {e}", spec.user, spec.slug, pinned))?;
        if v.tombstoned {
            return Err(anyhow!(
                "@{}/{} was deleted by its owner; remove the `use` or pick a fork",
                spec.user,
                spec.slug
            ));
        }
        Ok(into_fetched(&v.model_id, &v.version))
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
