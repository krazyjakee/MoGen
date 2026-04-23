//! Natural-language → DSL generation via the Gemini API.
//!
//! The crate exposes three concerns that compose into `mogen generate`:
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
pub use gemini::{
    CachedContent, GeminiClient, GeminiError, GenerateConfig, ThinkingLevel, Usage,
    DEFAULT_FAST_MODEL,
};
pub use image::{GeneratedImage, DEFAULT_IMAGE_MODEL};
pub use image_cache::{default_image_cache_dir, ImageCache};
pub use prompt::{system_instruction, StdlibIndex};
pub use repair::{
    generate_with_repair, repair_message, validate_text, GenerateOutcome, RepairConfig,
};
pub use textures::parse_prompt_header;

/// Prepend a seed comment to DSL text so rebuilds are reproducible.
///
/// The seed is written as a DSL line comment; the parser ignores it. When the
/// user re-runs `mogen generate --seed N` with the same prompt, the same seed
/// lands in the output header.
///
/// When `thinking` is `Some`, a sibling `// mogen-generate thinking=<level>`
/// line is written alongside the seed so the next LLM call on this file
/// (CLI or Studio) can pick up the per-file budget without a flag.
pub fn embed_seed_header(
    dsl: &str,
    seed: u64,
    prompt: &str,
    thinking: Option<ThinkingLevel>,
) -> String {
    let mut out = String::with_capacity(dsl.len() + 160);
    out.push_str(&format!("// mogen-generate seed={seed}\n"));
    if let Some(level) = thinking {
        out.push_str(&format!("// mogen-generate thinking={}\n", level.key()));
    }
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
        if let Some(rest) = trimmed.strip_prefix("// mogen-generate seed=") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Extract the thinking level from a DSL header previously written by
/// [`embed_seed_header`]. Returns `None` when the line is absent, the value is
/// malformed, or the file predates the per-file header.
pub fn parse_thinking_header(dsl: &str) -> Option<ThinkingLevel> {
    for line in dsl.lines().take(8) {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("// mogen-generate thinking=") {
            return ThinkingLevel::parse(rest.trim());
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
        let wrapped = embed_seed_header(dsl, 42, "a simple box", None);
        assert_eq!(parse_seed_header(&wrapped), Some(42));
        assert!(wrapped.contains("// prompt: a simple box"));
        assert!(wrapped.contains("scene {"));
    }

    #[test]
    fn seed_header_ignores_newlines_in_prompt() {
        let wrapped = embed_seed_header("x", 1, "line1\nline2", None);
        assert!(wrapped.contains("// prompt: line1 line2"));
    }

    #[test]
    fn parse_seed_header_missing() {
        assert_eq!(parse_seed_header("scene {}\n"), None);
    }

    #[test]
    fn thinking_header_roundtrip() {
        let dsl = "scene { box \"b\" (size=[1,1,1]) }\n";
        let wrapped = embed_seed_header(dsl, 7, "p", Some(ThinkingLevel::Low));
        assert!(wrapped.contains("// mogen-generate thinking=low"));
        assert_eq!(parse_thinking_header(&wrapped), Some(ThinkingLevel::Low));
        // Seed + prompt still round-trip alongside the new line.
        assert_eq!(parse_seed_header(&wrapped), Some(7));
    }

    #[test]
    fn thinking_header_omitted_when_none() {
        let wrapped = embed_seed_header("scene {}\n", 1, "p", None);
        assert!(!wrapped.contains("thinking="));
        assert_eq!(parse_thinking_header(&wrapped), None);
    }

    #[test]
    fn parse_thinking_header_absent_or_malformed() {
        assert_eq!(parse_thinking_header("scene {}\n"), None);
        assert_eq!(
            parse_thinking_header("// mogen-generate thinking=weird\nscene {}\n"),
            None,
        );
    }
}
