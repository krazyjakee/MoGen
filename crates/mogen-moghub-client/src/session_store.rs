//! On-disk persistence for the MoGHub session token.
//!
//! Lives next to the OAuth bundles (`~/.mogen/google_auth.json`,
//! `~/.mogen/antigravity_auth.json`) so every credential `mogen` keeps
//! is in one directory: `~/.mogen/moghub_auth.json`. JSON envelope
//! mirrors the OAuth one — `version` + `auth_kind` + the value, plus an
//! optional `base_url` so re-running `mogen auth moghub login` against a
//! self-hosted instance round-trips the URL.
//!
//! Atomic write via sibling-tmp + rename. On Unix the file is chmod
//! 0600 so other users on a shared host can't read it.
//!
//! Path resolution honours `MOGEN_MOGHUB_SESSION_STORE` (full file
//! path) and `MOGEN_CACHE_DIR` (parent dir); legacy `~/.cache/mogen/`
//! and `%LOCALAPPDATA%\mogen\` are read-only fallbacks for users that
//! pre-date the move to `~/.mogen/`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Filename inside the resolved mogen-owned directory.
const FILENAME: &str = "moghub_auth.json";
/// Env var that, when set, overrides the entire session-file path.
const ENV_OVERRIDE: &str = "MOGEN_MOGHUB_SESSION_STORE";
const SCHEMA_VERSION: u32 = 1;

/// Wire envelope around the session UUID. The `auth_kind` discriminator
/// matches the OAuth bundle's `"oauth"` so a user inspecting `~/.mogen/`
/// can tell at a glance which credential each file holds.
#[derive(Debug, Serialize, Deserialize)]
struct StoredSession {
    version: u32,
    auth_kind: String,
    session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
}

/// Whether the resolver is being asked for a path to *read* an
/// existing file (try legacy locations) or to *write* a new file
/// (always use the canonical `~/.mogen/` location).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathMode {
    Read,
    Write,
}

/// Canonical session-file path. Returns `None` only when the platform
/// exposes neither `HOME`/`USERPROFILE` nor `LOCALAPPDATA` (effectively
/// never on a real install).
pub fn session_path(mode: PathMode) -> Option<PathBuf> {
    resolve_user_path(FILENAME, ENV_OVERRIDE, mode)
}

/// Every existing on-disk session file across canonical and legacy
/// locations. Used by `mogen auth moghub logout` to remove all copies
/// so a half-cleaned legacy file can't silently re-authenticate.
pub fn all_existing_session_paths() -> Vec<PathBuf> {
    all_existing_user_paths(FILENAME, ENV_OVERRIDE)
}

/// Read the persisted session token. Returns `None` when the file is
/// missing, empty, or malformed — callers treat all three as
/// "logged out".
pub fn read_session() -> Option<String> {
    let path = session_path(PathMode::Read)?;
    let bytes = fs::read(&path).ok()?;
    let stored: StoredSession = serde_json::from_slice(&bytes).ok()?;
    let trimmed = stored.session.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Read the persisted base URL alongside the session. `None` when the
/// file is missing or pre-dates the field. Lets `mogen auth moghub
/// status` show which server the stored token belongs to.
pub fn read_base_url() -> Option<String> {
    let path = session_path(PathMode::Read)?;
    let bytes = fs::read(&path).ok()?;
    let stored: StoredSession = serde_json::from_slice(&bytes).ok()?;
    stored.base_url.filter(|s| !s.trim().is_empty())
}

/// Atomically write `token` (and optional `base_url`) to the canonical
/// session file. Creates the parent directory tree if missing. On Unix
/// the file ends up with mode 0600.
pub fn save_session(token: &str, base_url: Option<&str>) -> io::Result<()> {
    let path = session_path(PathMode::Write).ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "no canonical config dir for moghub session")
    })?;
    write_to(&path, token, base_url)
}

/// Write the session JSON to `path` atomically. Exposed for tests; in
/// production prefer [`save_session`].
pub fn write_to(path: &Path, token: &str, base_url: Option<&str>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let stored = StoredSession {
        version: SCHEMA_VERSION,
        auth_kind: "session".into(),
        session: token.to_string(),
        base_url: base_url.map(str::to_string),
    };
    let bytes = serde_json::to_vec_pretty(&stored)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &bytes)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&tmp, perms)?;
    }

    fs::rename(&tmp, path)
}

/// Delete every existing session file across canonical and legacy
/// paths. Idempotent — missing files are not an error.
pub fn clear_session() -> io::Result<()> {
    let paths = all_existing_session_paths();
    if paths.is_empty() {
        return Ok(());
    }
    let mut first_err: Option<io::Error> = None;
    for path in paths {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
    }
    match first_err {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Walk all candidate `mogen`-owned directories, returning every
/// existing `filename` location. Mirrors the resolver in
/// `mogen-llm::google_oauth::client` — kept inline here to avoid a
/// reverse dep from `mogen-moghub-client` onto `mogen-llm`.
fn all_existing_user_paths(filename: &str, file_override_var: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var(file_override_var) {
        if !p.trim().is_empty() {
            let path = PathBuf::from(p);
            if path.exists() {
                out.push(path);
            }
        }
    }
    if let Ok(dir) = std::env::var("MOGEN_CACHE_DIR") {
        if !dir.trim().is_empty() {
            let path = PathBuf::from(dir).join(filename);
            if path.exists() && !out.contains(&path) {
                out.push(path);
            }
        }
    }
    let home_candidate = std::env::var("HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    if let Some(home) = home_candidate.as_deref() {
        for dir in [".mogen", ".cache/mogen"] {
            let mut p = PathBuf::from(home);
            for seg in dir.split('/') {
                p.push(seg);
            }
            p.push(filename);
            if p.exists() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    if let Ok(localapp) = std::env::var("LOCALAPPDATA") {
        if !localapp.trim().is_empty() {
            let p = PathBuf::from(localapp).join("mogen").join(filename);
            if p.exists() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Resolve a `mogen`-owned file path. Read mode prefers the canonical
/// `~/.mogen/{filename}` but falls back to legacy `~/.cache/mogen/`
/// and `%LOCALAPPDATA%\mogen\` if the canonical file doesn't exist
/// yet. Write mode skips existence checks and goes straight to the
/// canonical target.
fn resolve_user_path(filename: &str, file_override_var: &str, mode: PathMode) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(file_override_var) {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if let Ok(dir) = std::env::var("MOGEN_CACHE_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join(filename));
        }
    }
    let home_candidate = std::env::var("HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    if mode == PathMode::Read {
        if let Some(home) = home_candidate.as_deref() {
            let dotdir = PathBuf::from(home).join(".mogen").join(filename);
            if dotdir.exists() {
                return Some(dotdir);
            }
            let legacy = PathBuf::from(home).join(".cache").join("mogen").join(filename);
            if legacy.exists() {
                return Some(legacy);
            }
        }
        if let Ok(localapp) = std::env::var("LOCALAPPDATA") {
            if !localapp.trim().is_empty() {
                let legacy = PathBuf::from(localapp).join("mogen").join(filename);
                if legacy.exists() {
                    return Some(legacy);
                }
            }
        }
    }
    if let Some(home) = home_candidate {
        return Some(PathBuf::from(home).join(".mogen").join(filename));
    }
    if let Ok(localapp) = std::env::var("LOCALAPPDATA") {
        if !localapp.trim().is_empty() {
            return Some(PathBuf::from(localapp).join("mogen").join(filename));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tempdir();
        let path = dir.join(FILENAME);
        write_to(&path, "abc-123", Some("https://example.test")).unwrap();

        // Re-read directly from the file (bypass the env-driven resolver
        // so the test stays hermetic).
        let bytes = fs::read(&path).unwrap();
        let stored: StoredSession = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(stored.session, "abc-123");
        assert_eq!(stored.base_url.as_deref(), Some("https://example.test"));
        assert_eq!(stored.auth_kind, "session");
        assert_eq!(stored.version, SCHEMA_VERSION);
    }

    #[test]
    fn write_creates_parent_dir() {
        let dir = tempdir();
        let path = dir.join("nested").join("dirs").join(FILENAME);
        write_to(&path, "xyz", None).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn read_session_via_env_override() {
        let dir = tempdir();
        let path = dir.join(FILENAME);
        write_to(&path, "from-env", None).unwrap();

        let _guard = EnvGuard::set(ENV_OVERRIDE, path.to_str().unwrap());
        assert_eq!(read_session().as_deref(), Some("from-env"));
    }

    #[test]
    fn read_session_returns_none_when_missing() {
        let dir = tempdir();
        let _guard = EnvGuard::set(ENV_OVERRIDE, dir.join("nope.json").to_str().unwrap());
        assert!(read_session().is_none());
    }

    #[test]
    fn read_session_returns_none_when_blank() {
        let dir = tempdir();
        let path = dir.join(FILENAME);
        write_to(&path, "   ", None).unwrap();
        let _guard = EnvGuard::set(ENV_OVERRIDE, path.to_str().unwrap());
        assert!(read_session().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn unix_perms_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let path = dir.join(FILENAME);
        write_to(&path, "secret", None).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// Tiny scoped tempdir without pulling in the `tempfile` crate.
    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("mogen-moghub-session-{pid}-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// RAII env-var setter so a failing test doesn't leak state into
    /// the next one. Tests that touch the same var must not run in
    /// parallel — `cargo test` does parallelise by default, but the
    /// override paths are unique-per-tempdir so the worst case is a
    /// hot-swap mid-call, not data corruption.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
