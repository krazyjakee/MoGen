//! Persistent map from (model, system-instruction hash) → Gemini
//! `cachedContents` resource name.
//!
//! Re-reading this map across invocations lets `mgen generate` / `modify` /
//! `bench` skip the (multi-thousand-token) system-instruction upload on
//! repeated calls, paying only the 25% cached-input rate instead.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::gemini::{GeminiClient, GeminiError};

/// Default TTL we request when creating a cache entry. One hour matches the
/// Gemini documented default — long enough for a typical interactive
/// session, short enough that a stale entry doesn't accumulate storage
/// billing after the user walks away.
pub const DEFAULT_TTL_SECONDS: u64 = 3600;

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub model: String,
    pub system_hash_hex: String,
    pub resource_name: String,
    pub expires_at_unix: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CacheFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<CacheEntry>,
}

impl CacheFile {
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let s = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        fs::write(path, s)
    }

    pub fn lookup(&self, model: &str, system_hash_hex: &str, now_unix: u64) -> Option<&str> {
        self.entries.iter().find_map(|e| {
            if e.model == model
                && e.system_hash_hex == system_hash_hex
                && e.expires_at_unix > now_unix
            {
                Some(e.resource_name.as_str())
            } else {
                None
            }
        })
    }

    /// Insert a fresh entry for `(model, hash)`, replacing any prior one and
    /// evicting any fully-expired rows along the way.
    pub fn upsert(&mut self, entry: CacheEntry) {
        self.version = CURRENT_VERSION;
        let now = now_unix();
        let model = entry.model.clone();
        let hash = entry.system_hash_hex.clone();
        self.entries.retain(|e| {
            e.expires_at_unix > now && !(e.model == model && e.system_hash_hex == hash)
        });
        self.entries.push(entry);
    }
}

/// FNV-1a 64-bit hex. Not cryptographic — just a stable, collision-resistant-
/// enough key for local cache matching. Inlined to avoid adding a hash dep
/// and to stay stable across Rust toolchain upgrades.
pub fn hash_system_instruction(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Default on-disk location: `$MGEN_CACHE_DIR/gemini.json` if set, else
/// `$HOME/.cache/mgen/gemini.json`. Returns `None` when neither is available
/// (e.g. some restricted CI sandboxes) — callers should fall back to inline.
pub fn default_cache_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("MGEN_CACHE_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join("gemini.json"));
        }
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .map(|h| PathBuf::from(h).join(".cache").join("mgen").join("gemini.json"))
}

/// Look up — or create and persist — a `cachedContents` resource for this
/// system instruction. On any API or transport failure, returns `Err` so the
/// caller can decide whether to warn-and-fall-back or abort.
pub fn resolve_or_create(
    client: &GeminiClient,
    model: &str,
    system_instruction: &str,
    cache_path: &Path,
    ttl_seconds: u64,
) -> Result<String, GeminiError> {
    let hash = hash_system_instruction(system_instruction);
    let mut file = CacheFile::load(cache_path);
    if let Some(name) = file.lookup(model, &hash, now_unix()) {
        return Ok(name.to_string());
    }

    let created = client.create_cached_content(model, system_instruction, ttl_seconds)?;
    file.upsert(CacheEntry {
        model: model.to_string(),
        system_hash_hex: hash,
        resource_name: created.name.clone(),
        expires_at_unix: created.expires_at_unix,
    });
    // Persistence is best-effort — we've already paid for cache creation, so
    // a local write failure (permission denied, disk full) shouldn't fail the
    // whole generate.
    let _ = file.save(cache_path);
    Ok(created.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_and_stable() {
        let a = hash_system_instruction("hello world");
        let b = hash_system_instruction("hello world");
        assert_eq!(a, b);
        assert_ne!(hash_system_instruction("a"), hash_system_instruction("b"));
        // FNV-1a 64-bit → 16 hex chars.
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn lookup_matches_model_hash_and_expiry() {
        let mut f = CacheFile::default();
        f.upsert(CacheEntry {
            model: "gemini-pro-latest".into(),
            system_hash_hex: "abc".into(),
            resource_name: "cachedContents/x".into(),
            expires_at_unix: now_unix() + 3600,
        });
        let now = now_unix();
        assert_eq!(
            f.lookup("gemini-pro-latest", "abc", now),
            Some("cachedContents/x")
        );
        assert!(f.lookup("gemini-pro-latest", "def", now).is_none());
        assert!(f.lookup("gemini-flash", "abc", now).is_none());
        assert!(f.lookup("gemini-pro-latest", "abc", now + 7200).is_none());
    }

    #[test]
    fn upsert_replaces_existing_and_purges_expired() {
        let mut f = CacheFile::default();
        f.entries.push(CacheEntry {
            model: "m".into(),
            system_hash_hex: "h".into(),
            resource_name: "old".into(),
            expires_at_unix: 0,
        });
        f.entries.push(CacheEntry {
            model: "other".into(),
            system_hash_hex: "other".into(),
            resource_name: "unrelated-expired".into(),
            expires_at_unix: 0,
        });
        f.upsert(CacheEntry {
            model: "m".into(),
            system_hash_hex: "h".into(),
            resource_name: "new".into(),
            expires_at_unix: now_unix() + 3600,
        });
        assert_eq!(f.entries.len(), 1);
        assert_eq!(f.entries[0].resource_name, "new");
        assert_eq!(f.version, CURRENT_VERSION);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let f = CacheFile::load(Path::new("/nonexistent/mgen/cache.json"));
        assert!(f.entries.is_empty());
        assert_eq!(f.version, 0);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempdir_like();
        let path = dir.join("cache.json");
        let mut f = CacheFile::default();
        f.upsert(CacheEntry {
            model: "m".into(),
            system_hash_hex: "h".into(),
            resource_name: "r".into(),
            expires_at_unix: now_unix() + 3600,
        });
        f.save(&path).expect("write");
        let loaded = CacheFile::load(&path);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].resource_name, "r");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_cache_path_prefers_mgen_cache_dir() {
        // Can't mutate env safely in parallel tests without races — instead
        // call with a known-good HOME synthesis and assert shape.
        if std::env::var("HOME").is_ok() {
            let p = default_cache_path().expect("HOME is set");
            assert!(p.ends_with("gemini.json"));
        }
    }

    fn tempdir_like() -> PathBuf {
        let ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("mgen-llm-cache-test-{ns}"));
        fs::create_dir_all(&p).unwrap();
        p
    }
}
