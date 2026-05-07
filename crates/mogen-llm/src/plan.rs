//! Architect-agent planning pass for `mogen generate --plan` and
//! `mogen modify --plan`.
//!
//! The planner is a single-shot LLM call that converts the user's natural
//! language prompt into a structured Markdown plan (subject, parts,
//! hierarchy, materials, optional animation, notes). The plan is then
//! folded into the user prompt for the regular DSL "coder" pass.
//!
//! Splitting generation this way is the steering trick that addresses the
//! "drowning in primitives" failure mode: the heavy spatial reasoning
//! happens in plain language where the model is strongest, and the second
//! pass is left to translate an already-decomposed plan into syntax. The
//! [`crate::prompt::PLANNER_PREAMBLE`] system instruction explicitly forbids
//! the planner from emitting DSL — keeping the two passes textually
//! separable so a reviewer can read what the model "intended" before the
//! Coder pass.

use crate::prompt::planner_system_instruction;
use crate::provider::{LlmClient, ProviderError};
use crate::types::{GenerateConfig, Usage};

/// Outcome of a planning call. The plan is the verbatim model output with
/// surrounding whitespace trimmed; `usage` reports the planning call's own
/// tokens so the CLI can roll it into the total budget.
#[derive(Debug, Clone)]
pub struct PlanOutcome {
    pub plan: String,
    pub usage: Usage,
}

/// Run the Architect agent.
///
/// `base` is the [`GenerateConfig`] the caller would otherwise pass to
/// [`crate::repair::generate_with_repair`] — we copy `model`, `temperature`,
/// `seed`, `thinking_level`, and `budget_tokens` over so the planner runs
/// on the same surface as the Coder pass. The planner uses its own
/// purpose-built system instruction ([`planner_system_instruction`]) and
/// never reuses a `cachedContents` resource keyed for the DSL grammar
/// (different prompt = different cache key).
pub fn generate_plan(
    client: &LlmClient,
    base: &GenerateConfig,
    prompt: &str,
) -> Result<PlanOutcome, ProviderError> {
    let mut cfg = GenerateConfig::new(prompt);
    cfg.model = base.model.clone();
    cfg.temperature = base.temperature;
    cfg.budget_tokens = base.budget_tokens;
    cfg.seed = base.seed;
    cfg.thinking_level = base.thinking_level;
    cfg.system_instruction = Some(planner_system_instruction());
    // Cache resources are keyed by the DSL system instruction; the planner
    // ships a different system prompt, so reusing a coder-side cache would
    // duplicate context inside the call. Send everything inline.
    cfg.cached_content = None;

    let resp = client.generate(&cfg)?;
    let plan = sanitize_plan_text(resp.text.trim());
    Ok(PlanOutcome {
        plan,
        usage: resp.usage,
    })
}

/// Defensive scrub for planner output. The planner system instruction
/// forbids DSL emission, but enforcement is zero — a planner that ignores
/// the rule poisons the Coder turn with phantom DSL the second model is
/// then primed to copy. If any DSL keyword appears we (a) warn on stderr
/// and (b) strip fenced code blocks, which is where models almost always
/// stash leaked DSL.
fn sanitize_plan_text(plan: &str) -> String {
    if let Some(needle) = DSL_LEAK_NEEDLES.iter().find(|n| plan.contains(*n)) {
        eprintln!(
            "mogen plan: planner emitted DSL keyword `{needle}`; stripping fenced blocks"
        );
        return strip_fenced_blocks(plan);
    }
    plan.to_string()
}

/// Substrings that only occur in mogen DSL emission — never in well-formed
/// Markdown plans. Match is conservative on purpose: we'd rather miss a
/// rare leak than false-positive on a legit description. The keyword set
/// matches the prohibitions enumerated in the planner system instruction.
const DSL_LEAK_NEEDLES: &[&str] = &[
    "scene {",
    "attach (",
    "material(",
    "material (",
    "clip(",
    "clip (",
    "track(",
    "track (",
    "joint(",
    "joint (",
];

/// Strip Markdown fenced code blocks (``` … ```), tildes included, leaving
/// the surrounding prose intact. Models that leak DSL almost always do it
/// inside fences; the surrounding plan is usually fine.
fn strip_fenced_blocks(plan: &str) -> String {
    let mut out = String::with_capacity(plan.len());
    let mut in_block = false;
    for line in plan.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_block = !in_block;
            continue;
        }
        if !in_block {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

/// Build the user prompt for the Coder pass once a plan is in hand. Uses a
/// stable section structure so the model can find each piece quickly:
/// `Original prompt:` then `Plan:` followed by the architect's Markdown.
/// The plan is treated as load-bearing context, not a suggestion — the
/// instruction explicitly tells the model to follow it.
pub fn compose_coder_prompt(original_prompt: &str, plan: &str) -> String {
    let mut s = String::with_capacity(original_prompt.len() + plan.len() + 256);
    s.push_str("Original prompt: ");
    s.push_str(original_prompt.trim());
    s.push_str("\n\nFollow the Architect's plan below when picking parts, \
                dimensions, attachment hierarchy, and materials. Translate it \
                into the smallest valid `mogen` DSL file you can — names and \
                hierarchy from the plan should round-trip into the file.\n\n\
                Plan:\n\n");
    s.push_str(plan.trim());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coder_prompt_carries_original_and_plan() {
        let s = compose_coder_prompt("a wooden stool", "## Subject\nfour-legged stool");
        assert!(s.contains("a wooden stool"));
        assert!(s.contains("four-legged stool"));
        // The Coder pass needs to know the plan is load-bearing, not just
        // background context.
        assert!(s.contains("Follow the Architect's plan"));
        // Plan is fenced under a "Plan:" heading so the model can find it.
        assert!(s.contains("\nPlan:\n"));
    }

    #[test]
    fn coder_prompt_trims_whitespace() {
        // Trailing newlines from the planner shouldn't bleed into the prompt
        // — the Coder will see double-blank-lines that look like section
        // breaks otherwise.
        let s = compose_coder_prompt("  prompt  ", "  plan  \n\n");
        assert!(s.contains("Original prompt: prompt"));
        assert!(s.trim_end().ends_with("plan"));
    }

    #[test]
    fn dsl_leak_is_warned_and_stripped() {
        // Planner that ignored the "no DSL" rule and stuffed a fenced
        // `scene { … }` block into its plan. The fence and its contents
        // must be stripped; the surrounding prose must survive.
        let raw = "## Subject\n\
                   four-legged stool\n\
                   \n\
                   ```mogen\n\
                   scene {\n  box \"x\" (size=[1,1,1])\n}\n\
                   ```\n\
                   \n\
                   ## Notes\nbeech wood";
        let cleaned = sanitize_plan_text(raw);
        assert!(cleaned.contains("## Subject"));
        assert!(cleaned.contains("## Notes"));
        assert!(cleaned.contains("beech wood"));
        assert!(!cleaned.contains("scene {"));
        assert!(!cleaned.contains("```"));
    }

    #[test]
    fn clean_plan_passes_through_unchanged() {
        // No DSL keywords -> sanitize_plan_text is a no-op so legitimate
        // mentions of e.g. "the scene" in prose don't get mangled.
        let raw = "## Subject\n\
                   the scene shows a stool with four legs";
        assert_eq!(sanitize_plan_text(raw), raw);
    }
}
