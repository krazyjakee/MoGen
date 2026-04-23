//! Natural-language → DSL generation via the Gemini API.
//!
//! The crate exposes three concerns that compose into `mgen generate`:
//!
//! - [`gemini`] — HTTP client for Google's `generateContent` endpoint.
//! - [`prompt`] — assembles the system instruction (grammar + stdlib index + examples).
//! - [`repair`] — drives parse → validate → feed JSON diagnostics back on error.
//!
//! The top-level [`generate`] function ties them together and returns DSL text
//! plus usage metadata. Seed handling (for reproducibility) is the caller's
//! responsibility — wrap the output in [`embed_seed_header`] before writing to disk.

pub mod cache;
pub mod gemini;
pub mod image;
pub mod image_cache;
pub mod pbr_maps;
pub mod prompt;
pub mod repair;
pub mod textures;

pub use cache::{default_cache_path, resolve_or_create as resolve_or_create_cache, DEFAULT_TTL_SECONDS};
pub use gemini::{CachedContent, GeminiClient, GeminiError, GenerateConfig, ThinkingLevel, Usage};
pub use image::{GeneratedImage, DEFAULT_IMAGE_MODEL};
pub use image_cache::{default_image_cache_dir, ImageCache};
pub use prompt::{system_instruction, StdlibIndex};
pub use repair::{generate_with_repair, GenerateOutcome, RepairConfig};

/// Prepend a seed comment to DSL text so rebuilds are reproducible.
///
/// The seed is written as a DSL line comment; the parser ignores it. When the
/// user re-runs `mgen generate --seed N` with the same prompt, the same seed
/// lands in the output header.
pub fn embed_seed_header(dsl: &str, seed: u64, prompt: &str) -> String {
    let mut out = String::with_capacity(dsl.len() + 128);
    out.push_str(&format!("// mgen-generate seed={seed}\n"));
    // Collapse the prompt onto a single line; preserve it for reproducibility.
    let flat = prompt.replace('\n', " ").replace('\r', " ");
    out.push_str(&format!("// prompt: {}\n", flat.trim()));
    if !dsl.starts_with('\n') {
        out.push('\n');
    }
    out.push_str(dsl);
    out
}

/// Extract the seed from a DSL header previously written by [`embed_seed_header`].
pub fn parse_seed_header(dsl: &str) -> Option<u64> {
    for line in dsl.lines().take(8) {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("// mgen-generate seed=") {
            return rest.trim().parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_header_roundtrip() {
        let dsl = "scene { box \"b\" (size=[1,1,1]) }\n";
        let wrapped = embed_seed_header(dsl, 42, "a simple box");
        assert_eq!(parse_seed_header(&wrapped), Some(42));
        assert!(wrapped.contains("// prompt: a simple box"));
        assert!(wrapped.contains("scene {"));
    }

    #[test]
    fn seed_header_ignores_newlines_in_prompt() {
        let wrapped = embed_seed_header("x", 1, "line1\nline2");
        assert!(wrapped.contains("// prompt: line1 line2"));
    }

    #[test]
    fn parse_seed_header_missing() {
        assert_eq!(parse_seed_header("scene {}\n"), None);
    }
}
