//! Natural-language → DSL generation across multiple LLM providers.
//!
//! The crate exposes four concerns that compose into the `mogen generate`,
//! `modify`, `animate`, `repair`, `bench`, and `textures` commands:
//!
//! - [`provider`] — the [`LlmClient`] enum dispatches to the right backend
//!   (Gemini, OpenAI, Anthropic, Ollama). [`Provider`] describes the
//!   selector and per-provider defaults; [`ProviderError`] is the unified
//!   error returned by [`LlmClient::generate`].
//! - [`gemini`] / [`openai`] / [`anthropic`] / [`ollama`] — per-backend
//!   HTTP clients, each with its own request/response wire types.
//! - [`prompt`] — assembles the system instruction (grammar + stdlib index +
//!   examples). Provider-agnostic — the same string ships to all four.
//! - [`repair`] — drives parse → validate → feed JSON diagnostics back on
//!   error. Takes [`LlmClient`] so it works against any provider.
//!
//! The top-level [`generate_with_repair`] function ties the provider client
//! and the repair loop together. Seed handling (for reproducibility) is the
//! caller's responsibility — wrap the output in [`embed_seed_header`] before
//! writing to disk.

pub mod anthropic;
pub mod cache;
pub mod claude_code;
pub mod fireworks;
pub mod gemini;
pub mod google_oauth;
pub mod image;
pub mod image_client;
pub mod imports;
pub mod ollama;
pub mod openai;
pub mod pbr_maps;
pub mod plan;
pub mod prompt;
pub mod provider;
pub mod refine;
pub mod repair;
pub mod settings_store;
pub mod textures;
pub mod types;
pub mod zai;
pub mod zai_chat;

pub use cache::{default_cache_path, resolve_or_create as resolve_or_create_cache, DEFAULT_TTL_SECONDS};
pub use gemini::{CachedContent, GeminiAuth, GeminiClient, GeminiError};
pub use google_oauth::{
    all_existing_token_paths, all_existing_token_paths_for, delete_bundle, load_bundle,
    run_login_flow, save_bundle, token_store_path, token_store_path_for, token_store_write_path,
    token_store_write_path_for, LoginOptions, LoginOutcome, OAuthBundle, OAuthError,
    TOKEN_STORE_FILENAME,
};
pub use google_oauth::client::{resolve_user_path, PathMode};
pub use image::{GeneratedImage, DEFAULT_IMAGE_MODEL};
pub use image_client::{ImageClient, ImageError};
pub use imports::{
    format_import_aabb_preamble, format_imports_preserve_block, summarize_imports, ImportSummary,
};
pub use plan::{compose_coder_prompt, generate_plan, PlanOutcome};
pub use prompt::{
    cacheable_block, inline_block, planner_system_instruction, reviewer_system_instruction,
    system_instruction, StdlibIndex,
};
pub use provider::{GoogleCredential, LlmClient, Provider, ProviderError};
pub use refine::{build_reviewer_message, visual_refine};
pub use repair::{
    generate_with_repair, repair_message, validate_text, GenerateOutcome, RepairConfig,
};
pub use settings_store::{
    load_api_keys, read_api_key, settings_path as settings_store_path, zai_base_url, ApiKeys,
};
pub use fireworks::{FireworksClient, FireworksError};
pub use textures::parse_prompt_header;
pub use zai::{ZaiClient, ZaiError};
pub use zai_chat::{
    ZaiChatClient, ZaiChatError, CODING_PLAN_BASE_URL as ZAI_CODING_PLAN_BASE_URL,
    DEFAULT_BASE_URL as ZAI_DEFAULT_BASE_URL, DEFAULT_VISION_MODEL as ZAI_DEFAULT_VISION_MODEL,
};
pub use types::{
    GenerateConfig, GenerateResponse, ImageInput, Role, ThinkingLevel, Turn, Usage,
    DEFAULT_TEMPERATURE,
};

/// Default heavy text model — kept as the legacy alias so existing callers
/// (CLI clap defaults, Studio settings) keep compiling. New code should
/// pick the per-provider default via [`Provider::default_model`].
pub const DEFAULT_FAST_MODEL: &str = gemini::DEFAULT_FAST_MODEL;

/// Stamp the LLM generation metadata (seed, optional thinking budget, original
/// prompt) into the top-level `meta(...)` block so future runs can reproduce
/// the call without re-supplying flags.
///
/// Any legacy `// mogen-generate ...` / `// prompt:` comment header from older
/// MoGen versions is stripped first, so files migrate cleanly on the next
/// save. The meta block is created if absent; existing attrs (name, version,
/// tags, etc.) are preserved.
pub fn embed_seed_header(
    dsl: &str,
    seed: u64,
    prompt: &str,
    thinking: Option<ThinkingLevel>,
) -> String {
    let cleaned = mogen_dsl::strip_legacy_seed_comments(dsl);
    let mut out = mogen_dsl::upsert_meta_attr(&cleaned, "seed", &seed.to_string());
    if let Some(level) = thinking {
        out = mogen_dsl::upsert_meta_attr(&out, "thinking", level.key());
    }
    let flat = prompt.replace(['\n', '\r'], " ");
    let trimmed = flat.trim();
    if !trimmed.is_empty() {
        out = mogen_dsl::upsert_meta_attr(&out, "prompt", trimmed);
    }
    out
}

/// Extract the seed from a DSL `meta(seed=...)` attribute. Falls back to the
/// legacy `// mogen-generate seed=…` comment header so files written by older
/// versions of MoGen keep round-tripping until they're re-saved.
pub fn parse_seed_header(dsl: &str) -> Option<u64> {
    if let Some(v) = mogen_dsl::read_meta_attr(dsl, "seed") {
        if let Ok(n) = v.parse() {
            return Some(n);
        }
    }
    for line in dsl.lines().take(8) {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("// mogen-generate seed=") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Extract the thinking level from a DSL `meta(thinking=...)` attribute.
/// Falls back to the legacy `// mogen-generate thinking=…` comment header so
/// older files keep working.
pub fn parse_thinking_header(dsl: &str) -> Option<ThinkingLevel> {
    if let Some(v) = mogen_dsl::read_meta_attr(dsl, "thinking") {
        if let Some(level) = ThinkingLevel::parse(&v) {
            return Some(level);
        }
    }
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
        assert!(wrapped.contains("prompt = \"a simple box\""));
        assert!(wrapped.contains("scene {"));
        // The new representation lives in the meta block, not a comment.
        assert!(!wrapped.contains("// mogen-generate"));
        assert!(!wrapped.contains("// prompt:"));
    }

    #[test]
    fn seed_header_ignores_newlines_in_prompt() {
        let wrapped = embed_seed_header("x", 1, "line1\nline2", None);
        assert!(wrapped.contains("prompt = \"line1 line2\""));
    }

    #[test]
    fn parse_seed_header_missing() {
        assert_eq!(parse_seed_header("scene {}\n"), None);
    }

    #[test]
    fn parse_seed_header_reads_legacy_comment() {
        let src = "// mogen-generate seed=99\nscene {}\n";
        assert_eq!(parse_seed_header(src), Some(99));
    }

    #[test]
    fn thinking_header_roundtrip() {
        let dsl = "scene { box \"b\" (size=[1,1,1]) }\n";
        let wrapped = embed_seed_header(dsl, 7, "p", Some(ThinkingLevel::Low));
        assert!(wrapped.contains("thinking = \"low\""));
        assert_eq!(parse_thinking_header(&wrapped), Some(ThinkingLevel::Low));
        // Seed + prompt still round-trip alongside the new line.
        assert_eq!(parse_seed_header(&wrapped), Some(7));
    }

    #[test]
    fn thinking_header_omitted_when_none() {
        let wrapped = embed_seed_header("scene {}\n", 1, "p", None);
        assert!(!wrapped.contains("thinking ="));
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

    #[test]
    fn parse_thinking_header_reads_legacy_comment() {
        let src = "// mogen-generate thinking=high\nscene {}\n";
        assert_eq!(parse_thinking_header(src), Some(ThinkingLevel::High));
    }

    #[test]
    fn embed_seed_header_preserves_llm_supplied_meta() {
        // The LLM is now instructed to lead its output with a `meta(name,
        // description, tags)` block. `embed_seed_header` must append
        // seed/thinking/prompt to that same block without trampling the
        // author-facing attrs — otherwise MoGHub publish loses the title
        // and tag set.
        let llm_output = "\
meta (name = \"wooden_stool\", description = \"a four-legged wooden stool\", tags = [\"furniture\", \"stool\"])

material \"wood\" (color=[0.55, 0.35, 0.18])

scene { box \"b\" (size=[1,1,1], mat=\"wood\") }
";
        let wrapped = embed_seed_header(llm_output, 17, "a wooden stool", Some(ThinkingLevel::Low));
        assert!(wrapped.contains("name = \"wooden_stool\""));
        assert!(wrapped.contains("description = \"a four-legged wooden stool\""));
        assert!(wrapped.contains("tags = [\"furniture\", \"stool\"]"));
        // And the auto-stamped attrs land in the same block.
        assert_eq!(parse_seed_header(&wrapped), Some(17));
        assert_eq!(parse_thinking_header(&wrapped), Some(ThinkingLevel::Low));
        assert!(wrapped.contains("prompt = \"a wooden stool\""));
        // No second `meta (...)` block was inserted.
        assert_eq!(wrapped.matches("meta (").count(), 1);
    }

    #[test]
    fn embed_strips_legacy_comments() {
        let src = "// mogen-generate seed=1\n// mogen-generate thinking=low\n// prompt: old\nscene {}\n";
        let wrapped = embed_seed_header(src, 2, "new", Some(ThinkingLevel::High));
        assert!(!wrapped.contains("// mogen-generate"));
        assert!(!wrapped.contains("// prompt:"));
        assert_eq!(parse_seed_header(&wrapped), Some(2));
        assert_eq!(parse_thinking_header(&wrapped), Some(ThinkingLevel::High));
    }
}
