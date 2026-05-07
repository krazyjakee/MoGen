//! Visual auto-refinement for `mogen generate --auto-refine N` /
//! `mogen modify --auto-refine N`.
//!
//! Once we have a valid `.mog` from the regular generate-and-repair pass,
//! the CLI lowers it, renders a thumbnail through `mogen-render`'s headless
//! GL path, and feeds the PNG back to a vision-capable LLM as a self-critique
//! turn. The model is asked to look at what its DSL actually produced,
//! identify mismatches against the original natural-language prompt, and
//! emit a corrected DSL file. The corrected file is sent back through the
//! standard parse/validate/repair loop so any new validator errors are
//! patched up before the next iteration.
//!
//! This is the `ll3m`-style "give the model eyes" loop applied to mogen.
//! Only Gemini honours [`ImageInput`] today (see
//! [`crate::types::GenerateConfig::user_images`]); the CLI gates the flag
//! on [`crate::Provider::supports_images`] before reaching this module.

use crate::prompt::reviewer_system_instruction;
use crate::provider::{LlmClient, ProviderError};
use crate::repair::{generate_with_repair, GenerateOutcome, RepairConfig};
use crate::types::{GenerateConfig, ImageInput};

use mogen_dsl::ModuleRegistry;

/// Run one Reviewer agent pass.
///
/// `base` carries the model/temperature/seed/thinking/budget settings the
/// caller would otherwise hand to [`generate_with_repair`]. We rebuild the
/// system instruction on top of [`reviewer_system_instruction`] (a Reviewer
/// preamble + the standard Coder grammar reference) and clear any pinned
/// `cachedContents` resource — the cache is keyed for the Coder system
/// instruction, so reusing it here would inline the wrong preamble.
///
/// `image` is the rendered thumbnail of `current_dsl`. The bytes are
/// forwarded to Gemini via [`ImageInput`]; non-vision providers ignore the
/// field, but the CLI rejects this code path on those providers up front so
/// the call doesn't silently degrade to a text-only critique.
///
/// The returned [`GenerateOutcome`] is the post-repair revision: the
/// reviewer's raw output is fed straight into [`generate_with_repair`] so
/// any validator errors introduced by the rewrite are patched in the same
/// batch.
pub fn visual_refine(
    client: &LlmClient,
    base: &GenerateConfig,
    repair: &RepairConfig,
    registry: &ModuleRegistry,
    original_prompt: &str,
    current_dsl: &str,
    image: ImageInput,
) -> Result<GenerateOutcome, ProviderError> {
    let mut cfg = GenerateConfig::new(build_reviewer_message(original_prompt, current_dsl));
    cfg.model = base.model.clone();
    cfg.temperature = base.temperature;
    cfg.budget_tokens = base.budget_tokens;
    cfg.seed = base.seed;
    cfg.thinking_level = base.thinking_level;
    cfg.user_images = vec![image];

    // Rebuild the system instruction so the Reviewer preamble is in front
    // of the standard grammar reference. `cached_content` stays unset (the
    // default for a freshly-built `GenerateConfig`) — the cache is keyed
    // on the Coder system prompt, so reusing it would inline the wrong
    // preamble.
    let idx = crate::prompt::StdlibIndex::from_registry(registry);
    cfg.system_instruction = Some(reviewer_system_instruction(&idx));

    generate_with_repair(client, cfg, repair)
}

/// Pack the original prompt + previous DSL into the Reviewer's user turn.
/// The image is attached separately via [`GenerateConfig::user_images`];
/// this text reminds the model what it was asked for and what code produced
/// the picture it's looking at.
pub fn build_reviewer_message(original_prompt: &str, current_dsl: &str) -> String {
    let mut s = String::with_capacity(original_prompt.len() + current_dsl.len() + 512);
    s.push_str("Original prompt: ");
    s.push_str(original_prompt.trim());
    s.push_str("\n\nThe attached PNG is a 3/4 orbit-camera render of the DSL \
                below — your previous attempt at satisfying the prompt. Look \
                at the image, find concrete mismatches against the prompt \
                (wrong silhouette, missing parts, floating limbs, mis-scaled \
                pieces, wrong colour family), and emit a corrected DSL file.\n\n\
                Reuse names, materials, attaches, and animation tracks from \
                the previous attempt verbatim wherever they were already \
                correct. Re-emit the entire file — no diff, no commentary, \
                no markdown fences.\n\n\
                Previous DSL:\n\n");
    s.push_str(current_dsl.trim_end());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewer_message_includes_prompt_and_dsl() {
        let msg = build_reviewer_message(
            "a wooden stool",
            "scene { box \"seat\" (size=[1, 0.05, 1]) }",
        );
        assert!(msg.contains("a wooden stool"));
        assert!(msg.contains("box \"seat\""));
        // The reviewer is told to use the attached image — the image arrives
        // out-of-band, so the user-turn text has to point at it.
        assert!(msg.contains("attached PNG"));
        // Output contract reminder so the reviewer doesn't ship a diff.
        assert!(msg.contains("Re-emit the entire file"));
    }

    #[test]
    fn reviewer_message_orders_prompt_before_dsl() {
        // The Reviewer reads top-down. Original prompt has to land before
        // the previous DSL so the critique stays grounded in what the user
        // asked for, not in the file's existing structure.
        let msg = build_reviewer_message("subject", "scene {}");
        let prompt_idx = msg.find("Original prompt:").unwrap();
        let dsl_idx = msg.find("Previous DSL:").unwrap();
        assert!(prompt_idx < dsl_idx);
    }
}
