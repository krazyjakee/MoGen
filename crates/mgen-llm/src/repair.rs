//! Parse → validate → repair loop.
//!
//! After each Gemini call we:
//!   1. Strip any markdown fences the model added despite instructions.
//!   2. Parse the DSL (`mgen_dsl::parse`).
//!   3. Run AST validation (`mgen_validate::validate_ast`).
//!   4. If there are errors, feed them back as JSON diagnostics in a new turn.
//!
//! Parse-failures (grammar errors) are converted into a synthetic `E0001`
//! diagnostic so the model sees them in the same JSON shape as validator
//! output.
//!
//! The loop is bounded by [`RepairConfig::max_iters`] so a model that keeps
//! producing garbage can't drain the budget. The roadmap pins the default at 2.

use mgen_core::{has_errors, Diagnostic};

use crate::gemini::{GeminiClient, GeminiError, GenerateConfig, Usage};

pub struct RepairConfig {
    /// How many follow-up calls we allow after the first one. Default 2.
    pub max_iters: u32,
    /// Called with each repair feedback message (for CLI progress output).
    /// Keeps this crate free of println! noise.
    pub on_iteration: Option<Box<dyn Fn(u32, &[Diagnostic])>>,
}

impl Default for RepairConfig {
    fn default() -> Self {
        Self { max_iters: 2, on_iteration: None }
    }
}

impl std::fmt::Debug for RepairConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepairConfig")
            .field("max_iters", &self.max_iters)
            .field("on_iteration", &self.on_iteration.is_some())
            .finish()
    }
}

/// Final outcome after the repair loop terminates.
#[derive(Debug, Clone)]
pub struct GenerateOutcome {
    /// The best DSL produced. If `diagnostics` is non-empty this is the
    /// last attempt that still had errors — callers decide whether to keep it.
    pub dsl: String,
    /// Validator diagnostics for `dsl`. If empty, the DSL parsed cleanly and
    /// validated with no errors.
    pub diagnostics: Vec<Diagnostic>,
    /// Gemini usage summed across all calls in this session.
    pub usage: Usage,
    /// How many calls we actually made (initial + repair). Always `>= 1` on success.
    pub call_count: u32,
}

impl GenerateOutcome {
    pub fn is_ok(&self) -> bool {
        !has_errors(&self.diagnostics)
    }
}

/// Run `generate` then up to `repair.max_iters` repair passes.
pub fn generate_with_repair(
    client: &GeminiClient,
    cfg: GenerateConfig,
    repair: &RepairConfig,
) -> Result<GenerateOutcome, GeminiError> {
    let mut cfg = cfg;
    let mut total_usage = Usage::default();
    let mut calls = 0u32;

    // Each repair iteration is a fresh single-turn call: the previous DSL
    // attempt and its diagnostics are folded into the new user prompt. That
    // avoids accumulating (prompt, dsl, feedback, dsl, feedback...) in history
    // across retries, which ballooned token cost on multi-iteration repairs.
    let original_prompt = cfg.user_prompt.clone();

    for iter in 0..=repair.max_iters {
        let resp = client.generate(&cfg)?;
        calls += 1;
        total_usage.add(&resp.usage);

        let dsl = strip_markdown_fences(&resp.text);
        let diags = validate_text(&dsl);

        if !has_errors(&diags) {
            return Ok(GenerateOutcome {
                dsl,
                diagnostics: diags,
                usage: total_usage,
                call_count: calls,
            });
        }

        if iter == repair.max_iters {
            // Out of repair budget; return the last attempt with its diagnostics.
            return Ok(GenerateOutcome {
                dsl,
                diagnostics: diags,
                usage: total_usage,
                call_count: calls,
            });
        }

        if let Some(cb) = &repair.on_iteration {
            cb(iter + 1, &diags);
        }

        // Drop any prior history — the next turn carries everything it needs
        // inside the user prompt (original request + last DSL + diagnostics).
        cfg.history.clear();
        cfg.user_prompt = repair_message(&original_prompt, &dsl, &diags);
    }

    unreachable!("for loop always returns");
}

/// Validate DSL text, producing a Diagnostic list. Parse errors are promoted to
/// a synthetic E0001 diagnostic; lowering failures (e.g. unresolved `attach`
/// references) are promoted to E0701 so the repair loop can emit them in the
/// same JSON shape as validator output.
pub fn validate_text(dsl: &str) -> Vec<Diagnostic> {
    let ast = match mgen_dsl::parse(dsl) {
        Ok(ast) => ast,
        Err(e) => return vec![Diagnostic::error("E0001", format!("parse error: {e}"))],
    };
    let mut diags = mgen_validate::validate_ast(&ast);
    if has_errors(&diags) {
        return diags;
    }
    match mgen_dsl::lower(&ast) {
        Ok(graph) => {
            diags.extend(mgen_validate::validate_graph(&graph));
        }
        Err(e) => {
            diags.push(Diagnostic::error("E0701", format!("lowering error: {e}")));
        }
    }
    diags
}

/// Build the repair message we send back to the model. It's self-contained —
/// the original prompt, the last DSL attempt, and the diagnostics are all
/// inlined so the call can be made with an empty `history`.
pub fn repair_message(original_prompt: &str, prev_dsl: &str, diags: &[Diagnostic]) -> String {
    let mut s = String::with_capacity(prev_dsl.len() + 512);
    s.push_str("Original prompt: ");
    s.push_str(&original_prompt.replace('\n', " "));
    s.push_str("\n\nYour previous attempt was:\n\n");
    s.push_str(prev_dsl.trim_end());
    s.push_str(
        "\n\nIt failed validation with these diagnostics (one JSON object per line):\n\n",
    );
    for d in diags {
        let obj = serde_json::json!({
            "severity": d.severity,
            "code": d.code,
            "message": d.message,
            "span": d.span,
        });
        s.push_str(&obj.to_string());
        s.push('\n');
    }
    s.push_str(
        "\nProduce a corrected DSL file that addresses every error while preserving the \
         original intent. Reply with ONLY the corrected DSL — no commentary, no markdown fences.",
    );
    s
}

/// Some models wrap their output in ``` fences despite the system instruction.
/// Peel them off so the parser sees clean DSL. Idempotent: a fence-free input
/// is returned trimmed.
pub fn strip_markdown_fences(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    // Cut the first line (which may say ```mgen, ```rust, or just ```).
    let after_first_line = match trimmed.find('\n') {
        Some(nl) => &trimmed[nl + 1..],
        None => return trimmed.trim_start_matches('`').to_string(),
    };
    // Cut the trailing ``` if present.
    let body = match after_first_line.rfind("```") {
        Some(pos) => &after_first_line[..pos],
        None => after_first_line,
    };
    body.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_plain_fences() {
        let t = "```\nscene { box \"b\" (size=[1,1,1]) }\n```";
        assert_eq!(strip_markdown_fences(t), "scene { box \"b\" (size=[1,1,1]) }");
    }

    #[test]
    fn strips_language_tagged_fences() {
        let t = "```mgen\nscene {}\n```\n";
        assert_eq!(strip_markdown_fences(t), "scene {}");
    }

    #[test]
    fn passes_through_unfenced_text() {
        let t = "scene {}\n";
        assert_eq!(strip_markdown_fences(t), "scene {}");
    }

    #[test]
    fn validate_text_reports_parse_errors_as_e0001() {
        let d = validate_text("scene { box (size=");
        assert!(has_errors(&d));
        assert_eq!(d[0].code, "E0001");
    }

    #[test]
    fn validate_text_reports_validator_errors() {
        let d = validate_text("scene { wombat \"x\" (size=[1,1,1]) }");
        assert!(has_errors(&d));
        assert!(d.iter().any(|x| x.code == "E0101"));
    }

    #[test]
    fn validate_text_passes_clean_dsl() {
        let d = validate_text("scene { box \"b\" (size=[1,1,1]) }");
        assert!(!has_errors(&d));
    }

    #[test]
    fn repair_message_contains_json_diagnostics_prompt_and_prev_dsl() {
        let diags = vec![Diagnostic::error("E0101", "unknown node kind \"wombat\"")];
        let msg = repair_message("a wombat", "scene { wombat \"w\" (size=[1,1,1]) }", &diags);
        assert!(msg.contains("\"code\":\"E0101\""));
        assert!(msg.contains("a wombat"));
        assert!(msg.contains("wombat \"w\""), "should quote the prior DSL body");
        assert!(msg.contains("no markdown fences"));
    }
}
