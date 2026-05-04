use mogen_llm::{GenerateConfig, Provider, ThinkingLevel};

use super::llm::{build_provider_client, Credential};
use crate::app::types::EnhanceTarget;

/// Run a single prompt-enhancement call against the fast model. Returns the
/// rewritten prompt on success or a human-readable error string. Kept
/// separate from `run_llm` because the contract is plain prose, not DSL —
/// system instruction is intentionally minimal (no grammar, no stdlib) so
/// the token bill stays tiny.
///
/// `target` selects a rewrite template matched to the field being enhanced:
/// the Generate field wants a fresh asset description, Modify/Animate want
/// precise edit requests against an existing scene, and Texture Style wants
/// a compact PBR material descriptor. Using one template for all four
/// produced rewrites that were technically vivid but the wrong shape for
/// three of the four callers.
pub(in crate::app) fn run_prompt_enhance(
    target: EnhanceTarget,
    raw_prompt: String,
    provider: Provider,
    credential: Credential,
    model: String,
    claude_code_path: String,
) -> Result<String, String> {
    let client = build_provider_client(provider, credential, &claude_code_path);
    let raw = raw_prompt.trim();
    // Templates focus the rewrite on enriching the user's high-level
    // description — what the object looks like or how a part should change —
    // and deliberately stay silent about primitives, CSG, mirror/array, or
    // animation templates. The downstream generate/modify/animate prompts
    // already carry the DSL contract; bleeding it into the enhanced prompt
    // pre-bakes implementation choices and steers the actual generation step.
    let user = match target {
        EnhanceTarget::Generate => format!(
             "Enrich the following asset description with vivid, concrete visual \
              detail about the object itself — overall silhouette and proportion, \
              character or mood, surface materials and colour cues. Describe ONLY \
              the object. Do NOT add a setting, scene, environment, background, \
              location, lighting, weather, surroundings, or any other object \
              (companions, props, base, pedestal, ground, etc.). Do not prescribe \
              how it should be modelled, list parts, or suggest construction \
              steps. Keep it compact (1–3 sentences). The original subject phrase \
              \"{raw}\" MUST appear verbatim in your rewrite (typically as the \
              opening noun phrase) — you are adding detail to it, not replacing \
              it with a synonym or a different object. Do NOT use code fences, \
              do NOT prefix with \"Enhanced prompt:\" or similar. Reply with \
              only the rewritten prompt.\n\nPrompt: {raw}",
        ),
        EnhanceTarget::Modify => format!(
            "Rewrite the following instruction as a clear, specific edit request \
             against an existing 3D scene. Be precise about which named part \
             changes, in which direction, and by how much when it matters. Keep \
             it imperative (\"make…\", \"replace…\", \"scale…\"), 1–3 sentences. \
             Assume the scene already exists; do not redesign the whole object \
             and do not prescribe modelling steps or implementation details. Do \
             NOT use code fences, do NOT prefix with labels. Reply with only the \
             rewritten instruction.\n\nInstruction: {raw}",
        ),
        EnhanceTarget::Animate => format!(
            "Rewrite the following animation request for an existing 3D scene. \
             Be specific about which named part moves, what kind of motion \
             (rotation, swing, bob, …), the axis or direction, the amplitude or \
             angle, the speed or duration, and whether it loops. 1–3 sentences, \
             imperative tone. Do not redesign the object and do not prescribe \
             implementation details. Do NOT use code fences, do NOT prefix with \
             labels. Reply with only the rewritten request.\n\nRequest: {raw}",
        ),
        EnhanceTarget::TextureStyle => format!(
            "Rewrite the following texture / material hint into a compact PBR \
             style descriptor — colour palette, finish, wear, era or setting \
             cues. This text is appended to every material's albedo-image \
             generation prompt, so it must read as stylistic guidance, not as a \
             scene description. ≤ 20 words, comma-separated adjectives and short \
             noun phrases, no full sentences, no leading label. Do NOT use code \
             fences. Reply with only the rewritten hint.\n\nHint: {raw}",
        ),
    };

    let mut cfg = GenerateConfig::new(user);
    cfg.model = model;
    // Prompt rewriting is a low-reasoning task — Low budget is plenty and
    // keeps latency/cost in line with the "fast" label.
    cfg.thinking_level = Some(ThinkingLevel::Low);
    // A touch of variance produces more natural phrasing than the DSL default.
    cfg.temperature = Some(0.7);

    match client.generate(&cfg) {
        Ok(resp) => {
            let cleaned = resp.text.trim();
            // Strip markdown fences if the model ignored the "no code" hint.
            let cleaned = cleaned
                .strip_prefix("```")
                .map(|s| s.trim_start_matches(char::is_alphanumeric).trim_start())
                .unwrap_or(cleaned)
                .trim_end_matches("```")
                .trim();
            if cleaned.is_empty() {
                Err(format!("empty response from {}", provider.label()))
            } else {
                Ok(cleaned.to_string())
            }
        }
        Err(e) => Err(format!("{e}")),
    }
}
