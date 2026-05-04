//! On-disk persistence for [`OAuthBundle`].
//!
//! Path resolution lives in [`super::client::resolve_user_path`] so the
//! token store and `oauth_client.json` end up in the same directory:
//! `~/.mogen/google_auth.json` is the primary default, with
//! `~/.cache/mogen/` and `%LOCALAPPDATA%\mogen\` honoured as legacy
//! fallbacks for older installs that already wrote tokens there.
//!
//! Atomic write via `tempfile::NamedTempFile::persist` (rename within the
//! same directory). On Unix we also chmod 0600 so other users on shared
//! hosts can't read the refresh token.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::token::OAuthBundle;
use super::OAuthError;

pub const TOKEN_STORE_FILENAME: &str = "google_auth.json";
const SCHEMA_VERSION: u32 = 1;

/// Wire envelope around [`OAuthBundle`]. The version field lets us evolve
/// the schema later; unknown fields are tolerated by serde so a newer
/// `mogen` writing extra keys won't break an older one reading them — it
/// just ignores them.
#[derive(Debug, Serialize, Deserialize)]
struct StoredBundle {
    version: u32,
    auth_kind: String,
    #[serde(flatten)]
    bundle: OAuthBundle,
}

/// Resolve the on-disk token path. Returns `None` only when no candidate
/// directory is configured (no `MOGEN_CACHE_DIR`, no `HOME`/`USERPROFILE`,
/// no `LOCALAPPDATA`) — the CLI surfaces it as a clear error.
///
/// Read paths walk `~/.mogen/` → `~/.cache/mogen/` → `%LOCALAPPDATA%\mogen\`
/// so existing installs keep finding their old `google_auth.json` while
/// new logins land in `~/.mogen/`.
pub fn token_store_path() -> Option<PathBuf> {
    super::client::resolve_user_path(TOKEN_STORE_FILENAME, "MOGEN_TOKEN_STORE")
}

/// Load a previously-saved bundle. Returns `Ok(None)` when the file does
/// not exist; `Err` for IO or JSON decode failures (callers usually treat
/// the latter as "force re-login").
pub fn load_bundle(path: &std::path::Path) -> Result<Option<OAuthBundle>, OAuthError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let stored: StoredBundle = serde_json::from_slice(&bytes)?;
    Ok(Some(stored.bundle))
}

/// Atomically write `bundle` to `path`. Creates the parent directory tree
/// if missing. On Unix the file ends up with mode 0600.
pub fn save_bundle(path: &std::path::Path, bundle: &OAuthBundle) -> Result<(), OAuthError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let stored = StoredBundle {
        version: SCHEMA_VERSION,
        auth_kind: "oauth".into(),
        bundle: bundle.clone(),
    };
    let json = serde_json::to_vec_pretty(&stored)?;

    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(&json)?;
    tmp.flush()?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(tmp.path(), perms)?;
    }

    tmp.persist(path)
        .map_err(|e| OAuthError::Io(format!("token store rename: {e}")))?;
    Ok(())
}

/// Delete the token store. Idempotent — missing file returns `Ok(())`.
pub fn delete_bundle(path: &std::path::Path) -> Result<(), OAuthError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bundle() -> OAuthBundle {
        OAuthBundle {
            access_token: "ya29.aaa".into(),
            refresh_token: "1//rt".into(),
            access_expires_at_unix: 1_700_000_000,
            obtained_at_unix: 1_699_996_400,
            email: Some("[email protected]".into()),
            project_id: Some("proj-1".into()),
            managed_project_id: None,
            endpoint_base: Some("https://cloudcode-pa.googleapis.com".into()),
            scope: Some("cloud-platform userinfo.email".into()),
        }
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TOKEN_STORE_FILENAME);
        let original = sample_bundle();
        save_bundle(&path, &original).unwrap();
        let loaded = load_bundle(&path).unwrap().expect("file present");
        assert_eq!(loaded.access_token, original.access_token);
        assert_eq!(loaded.refresh_token, original.refresh_token);
        assert_eq!(loaded.email, original.email);
        assert_eq!(loaded.project_id, original.project_id);
        assert_eq!(loaded.endpoint_base, original.endpoint_base);
    }

    #[test]
    fn missing_file_is_none_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(load_bundle(&path).unwrap().is_none());
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TOKEN_STORE_FILENAME);
        let blob = serde_json::json!({
            "version": 1,
            "auth_kind": "oauth",
            "access_token": "a",
            "refresh_token": "r",
            "access_expires_at_unix": 1u64,
            "obtained_at_unix": 0,
            "email": null,
            "project_id": null,
            "managed_project_id": null,
            "endpoint_base": null,
            "scope": null,
            "future_field": "ignored"
        });
        std::fs::write(&path, serde_json::to_vec(&blob).unwrap()).unwrap();
        let loaded = load_bundle(&path).unwrap().expect("present");
        assert_eq!(loaded.access_token, "a");
    }

    #[test]
    fn delete_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        delete_bundle(&path).unwrap();
        delete_bundle(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_perms_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TOKEN_STORE_FILENAME);
        save_bundle(&path, &sample_bundle()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
