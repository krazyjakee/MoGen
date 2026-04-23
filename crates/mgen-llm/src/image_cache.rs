//! Content-addressed PNG cache for LLM-generated textures.
//!
//! Keyed by FNV-1a over `(model, prompt)`. File existence = cache hit; there's
//! no separate index — deleting the directory clears the cache. Intentionally
//! minimal: the expensive side is the API round trip, not the lookup.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::cache::hash_system_instruction;

/// Default on-disk location for generated images. Follows the same env-var
/// convention as [`crate::cache::default_cache_path`].
pub fn default_image_cache_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("MGEN_CACHE_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join("images"));
        }
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .map(|h| PathBuf::from(h).join(".cache").join("mgen").join("images"))
}

pub struct ImageCache {
    dir: PathBuf,
}

impl ImageCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Stable cache key: `FNV(model || "\n" || prompt)`. Exposed so callers can
    /// log it alongside cache hits without re-hashing.
    pub fn key(model: &str, prompt: &str) -> String {
        hash_system_instruction(&format!("{model}\n{prompt}"))
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.png"))
    }

    /// Return the on-disk path if we have a cached PNG for this key.
    pub fn lookup(&self, key: &str) -> Option<PathBuf> {
        let p = self.path_for(key);
        if p.exists() {
            Some(p)
        } else {
            None
        }
    }

    /// Write `bytes` under `key`. Creates the cache directory if missing.
    pub fn store(&self, key: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        fs::create_dir_all(&self.dir)?;
        let p = self.path_for(key);
        fs::write(&p, bytes)?;
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tempdir_like() -> PathBuf {
        let ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("mgen-image-cache-test-{ns}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn key_depends_on_both_model_and_prompt() {
        let a = ImageCache::key("m1", "p1");
        let b = ImageCache::key("m2", "p1");
        let c = ImageCache::key("m1", "p2");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(a, ImageCache::key("m1", "p1"));
    }

    #[test]
    fn store_then_lookup_roundtrip() {
        let dir = tempdir_like();
        let cache = ImageCache::new(&dir);
        let key = ImageCache::key("model", "a prompt");
        assert!(cache.lookup(&key).is_none());

        let path = cache.store(&key, b"\x89PNG fake").unwrap();
        assert!(path.exists());
        let found = cache.lookup(&key).expect("hit");
        assert_eq!(fs::read(&found).unwrap(), b"\x89PNG fake");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lookup_miss_when_file_absent() {
        let dir = tempdir_like();
        let cache = ImageCache::new(&dir);
        assert!(cache.lookup("nonexistent").is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
