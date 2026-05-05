//! Shared session-token storage for the MoGHub CLI subcommands.
//!
//! The token is stored in the OS keyring under the same
//! service+username pair Studio uses, so `mogen login` and Studio's
//! sign-in are interchangeable — sign in once, both surfaces see the
//! same session. On systems without a working keyring backend
//! (notably headless Linux without Secret Service) we fall back to a
//! plain file at `$XDG_CONFIG_HOME/mogen/moghub_session` with mode
//! 600 so the secret never leaks via process listings (which would
//! happen if we relied on env vars).
//!
//! Read order: `MOGHUB_SESSION` env var > keyring > fallback file.
//! The env override is mainly for CI / scripted use.
//!
//! Service / username constants are intentionally hard-coded to match
//! `mogen-studio/src/settings.rs` — keeping them in lockstep across
//! crates is a hand-maintained invariant; if either side changes, the
//! other must follow or sign-ins won't share state.

use std::path::PathBuf;

use anyhow::{anyhow, Result};

const KEYRING_SERVICE: &str = "mogen-studio";
const KEYRING_USERNAME: &str = "moghub_session";
const FALLBACK_FILENAME: &str = "moghub_session";

/// Look up the persisted MoGHub session token. Returns `None` when the
/// user is signed-out (no env, no keyring entry, no fallback file).
pub(crate) fn load_session() -> Option<String> {
    if let Ok(t) = std::env::var("MOGHUB_SESSION") {
        let t = t.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USERNAME) {
        if let Ok(t) = entry.get_password() {
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    let path = fallback_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let s = String::from_utf8(bytes).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Persist a freshly-issued token. Writes to the keyring first; on
/// failure falls back to the on-disk file. Returns the storage tier
/// that succeeded, for the caller to surface to the user.
pub(crate) fn store_session(token: &str) -> Result<StorageTier> {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USERNAME) {
        if entry.set_password(token).is_ok() {
            // If the fallback file holds a stale value from a previous
            // sign-in on a system without keyring, scrub it now —
            // otherwise a future load would prefer keyring (correct)
            // but the on-disk plaintext copy would linger.
            if let Some(p) = fallback_path() {
                let _ = std::fs::remove_file(&p);
            }
            return Ok(StorageTier::Keyring);
        }
    }
    let Some(path) = fallback_path() else {
        return Err(anyhow!(
            "no keyring backend available and no config dir for the fallback file"
        ));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_secret_file(&path, token)?;
    Ok(StorageTier::File(path))
}

/// Wipe the persisted token from every storage tier.
pub(crate) fn clear_session() -> Result<()> {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USERNAME) {
        // `delete_credential` returns NoEntry when there's nothing to
        // delete — treat that as success since the post-condition
        // (no token in keyring) is satisfied.
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(anyhow!("keyring delete failed: {e}")),
        }
    }
    if let Some(p) = fallback_path() {
        let _ = std::fs::remove_file(&p);
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum StorageTier {
    Keyring,
    File(PathBuf),
}

fn fallback_path() -> Option<PathBuf> {
    let dir = dirs::config_dir()?;
    Some(dir.join("mogen").join(FALLBACK_FILENAME))
}

#[cfg(unix)]
fn write_secret_file(path: &std::path::Path, token: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(token.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_file(path: &std::path::Path, token: &str) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(token.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}
