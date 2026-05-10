//! Parse → validate → repair loop.
//!
//! After each Gemini call we:
//!   1. Strip any markdown fences the model added despite instructions.
//!   2. Parse the DSL (`mogen_dsl::parse`).
//!   3. Run AST validation (`mogen_validate::validate_ast`).
//!   4. If there are errors, feed them back in a new turn with:
//!        * a per-code `fix:` hint that maps E-codes to concrete remedies;
//!        * a line/col excerpt of the offending source with a caret underline;
//!        * a rolling list of codes the model already fixed in prior attempts
//!          so it doesn't oscillate (fix A, break B; fix B, break A).
//!
//! Parse-failures (grammar errors) are converted into a synthetic `E0001`
//! diagnostic so the model sees them in the same shape as validator output.
//!
//! The loop is bounded by [`RepairConfig::max_iters`] so a model that keeps
//! producing garbage can't drain the budget. The roadmap pins the default at 2.

use std::collections::BTreeSet;

use mogen_core::{has_errors, Diagnostic, Severity, Span};

use crate::provider::{LlmClient, ProviderError};
use crate::types::{GenerateConfig, Usage};

pub struct RepairConfig {
    /// How many follow-up calls we allow after the first one. Default 2.
    pub max_iters: u32,
    /// Called with each repair feedback message (for CLI progress output).
    /// Keeps this crate free of println! noise.
    pub on_iteration: Option<Box<dyn Fn(u32, &[Diagnostic])>>,
    /// When all errors on a turn are "local" (typo / wrong attr type / unknown
    /// material), ask the model for SEARCH/REPLACE blocks instead of a full
    /// rewrite. Cuts output tokens dramatically on small fix-ups; falls back
    /// to a full rewrite automatically if the blocks don't apply cleanly.
    /// Default `true`. Set `false` to force the legacy rewrite-every-turn path.
    pub allow_edit_mode: bool,
}

impl Default for RepairConfig {
    fn default() -> Self {
        Self { max_iters: 2, on_iteration: None, allow_edit_mode: true }
    }
}

impl std::fmt::Debug for RepairConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepairConfig")
            .field("max_iters", &self.max_iters)
            .field("on_iteration", &self.on_iteration.is_some())
            .field("allow_edit_mode", &self.allow_edit_mode)
            .finish()
    }
}

/// What we asked the model to produce on a given turn. Determines how we
/// interpret the response: `Rewrite` parses it as a full DSL file, `Edits`
/// parses SEARCH/REPLACE blocks and applies them to the previous attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairMode {
    /// Model returns a complete corrected DSL file. Used for the first call
    /// and for any iteration whose diagnostics include structural errors
    /// (E1001/E1007/E1101 etc.) where surgical edits aren't enough.
    Rewrite,
    /// Model returns one or more `<<<<<<< SEARCH … >>>>>>> REPLACE` blocks
    /// applied to the previous attempt. Used when every error on the prior
    /// turn was local (E0101/E0103/E0104/E020x/E040x/E041x/E0421/E060x).
    Edits,
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
    /// LLM usage summed across all calls in this session.
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
    client: &LlmClient,
    cfg: GenerateConfig,
    repair: &RepairConfig,
) -> Result<GenerateOutcome, ProviderError> {
    let mut cfg = cfg;
    let mut total_usage = Usage::default();
    let mut calls = 0u32;

    // Each repair iteration is a fresh single-turn call: the previous DSL
    // attempt and its diagnostics are folded into the new user prompt. That
    // avoids accumulating (prompt, dsl, feedback, dsl, feedback...) in history
    // across retries, which ballooned token cost on multi-iteration repairs.
    let original_prompt = cfg.user_prompt.clone();

    // Cross-iteration error memory: every error code we've ever seen. Codes
    // that appeared in a prior iteration but are absent now are "fixed" and
    // listed back to the model so it won't reintroduce them.
    let mut ever_seen: BTreeSet<String> = BTreeSet::new();

    // Edit-mode plumbing: when the previous iteration's diagnostics were all
    // local, we asked for SEARCH/REPLACE blocks and need `prev_dsl` to apply
    // them against. `asked_for_edits` mirrors what we requested, not what we
    // got — the model may ignore the format and return a full file anyway,
    // which we detect by parse failure and treat as a rewrite.
    let mut asked_for_edits = false;
    let mut prev_dsl: Option<String> = None;

    for iter in 0..=repair.max_iters {
        let resp = client.generate(&cfg)?;
        calls += 1;
        total_usage.add(&resp.usage);

        // Materialise the candidate DSL. Edit-mode: try to parse + apply
        // SEARCH/REPLACE blocks against the previous attempt; on any failure
        // (no blocks, malformed, missing/ambiguous match) fall through to
        // treating the response as a full rewrite. The validator will catch
        // it if neither interpretation is valid DSL.
        let dsl = if asked_for_edits {
            let prev = prev_dsl.as_deref().unwrap_or("");
            match parse_edit_blocks(&resp.text)
                .and_then(|blocks| apply_edit_blocks(prev, &blocks))
            {
                Ok(merged) => merged,
                Err(_) => strip_markdown_fences(&resp.text),
            }
        } else {
            strip_markdown_fences(&resp.text)
        };

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

        let current_codes: BTreeSet<String> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.code.clone())
            .collect();
        let fixed: Vec<String> =
            ever_seen.difference(&current_codes).cloned().collect();

        // Decide the mode for the NEXT turn based on the diagnostics we just
        // got. Only enter edit-mode when every error is local — structural
        // errors (parse failures, lowering, skin topology, disconnection)
        // need a rewrite to restructure the source.
        let next_mode = if repair.allow_edit_mode && is_local_only(&diags) {
            RepairMode::Edits
        } else {
            RepairMode::Rewrite
        };
        asked_for_edits = next_mode == RepairMode::Edits;
        prev_dsl = Some(dsl.clone());

        // Drop any prior history — the next turn carries everything it needs
        // inside the user prompt (original request + last DSL + diagnostics).
        cfg.history.clear();
        cfg.user_prompt = repair_message(&original_prompt, &dsl, &diags, &fixed, next_mode);

        ever_seen.extend(current_codes);
    }

    unreachable!("for loop always returns");
}

/// Validate DSL text, producing a Diagnostic list. Parse errors are promoted to
/// a synthetic E0001 diagnostic; lowering failures (e.g. unresolved `attach`
/// references) are promoted to E0701 so the repair loop can emit them in the
/// same JSON shape as validator output.
pub fn validate_text(dsl: &str) -> Vec<Diagnostic> {
    let ast = match mogen_dsl::parse(dsl) {
        Ok(ast) => ast,
        Err(e) => return vec![Diagnostic::error("E0001", format!("parse error: {e}"))],
    };
    let mut diags = mogen_validate::validate_ast(&ast);
    if has_errors(&diags) {
        return diags;
    }
    match mogen_dsl::lower(&ast) {
        Ok(graph) => {
            diags.extend(mogen_validate::validate_graph(&graph));
        }
        Err(e) => {
            diags.push(Diagnostic::error("E0701", format!("lowering error: {e}")));
        }
    }
    diags
}

/// True when every *error*-severity diagnostic in `diags` is the kind that
/// can be fixed by editing one span in place — typos, wrong attr types,
/// unknown material/module references, bad enum values. False if any error
/// is structural (parse failure, missing/extra child kind, lowering failure,
/// skin topology, disconnected clusters), where a SEARCH/REPLACE chain would
/// be fragile and a full rewrite is more reliable.
///
/// Warnings are ignored — we only gate on errors because warnings don't
/// trigger another repair iteration anyway.
pub fn is_local_only(diags: &[Diagnostic]) -> bool {
    let mut saw_any = false;
    for d in diags.iter().filter(|d| d.severity == Severity::Error) {
        saw_any = true;
        let local = matches!(
            d.code.as_str(),
            // unknown kind / wrong attr type / unknown material
            "E0101" | "E0103" | "E0104"
            // material attr typos (NOT E0201 — it's a missing-name structural fix)
            | "E0203" | "E0206"
            // module attr typos (NOT E0302 — duplicate is a delete; NOT E0304 — unknown ref might need a new module)
            | "E0303" | "E0305"
            // joint attrs (E0401 missing-name is borderline but still a local insert/rename)
            | "E0402" | "E0403" | "E0404"
            // clip / track / proc-anim attrs
            | "E0413" | "E0414" | "E0421"
            // attach attrs
            | "E0601" | "E0602"
        );
        if !local {
            return false;
        }
    }
    saw_any
}

/// Build the repair message we send back to the model. It's self-contained —
/// the original prompt, the last DSL attempt, the diagnostics (with `fix:`
/// hints and source excerpts), and the list of codes the model already fixed
/// in prior attempts are all inlined so the call can be made with an empty
/// `history`.
///
/// `fixed_codes` is the set of error codes seen in prior iterations that are
/// absent from the current `diags` — pass `&[]` for the first pass or for
/// one-shot callers that don't track repair history.
///
/// `mode` switches the closing instructions between a full-file rewrite and
/// SEARCH/REPLACE blocks. The body of the message (prompt, prev DSL,
/// diagnostics, fixed codes) is identical between modes.
pub fn repair_message(
    original_prompt: &str,
    prev_dsl: &str,
    diags: &[Diagnostic],
    fixed_codes: &[String],
    mode: RepairMode,
) -> String {
    let mut s = String::with_capacity(prev_dsl.len() + 1024);
    s.push_str("Original prompt: ");
    s.push_str(&original_prompt.replace('\n', " "));
    s.push_str("\n\nYour previous attempt was:\n\n");
    s.push_str(prev_dsl.trim_end());
    s.push_str("\n\nIt failed validation with these errors:\n\n");
    for d in diags {
        let tag = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        };
        s.push_str(&format!("[{}] {} ({})\n", d.code, d.message, tag));
        if let Some(span) = d.span {
            if let Some(excerpt) = render_span_excerpt(prev_dsl, span) {
                s.push_str(&excerpt);
            }
        }
        if let Some(hint) = fix_hint(&d.code) {
            s.push_str("  fix: ");
            s.push_str(hint);
            s.push('\n');
        }
        s.push('\n');
    }

    if !fixed_codes.is_empty() {
        s.push_str(
            "You already fixed these in earlier attempts — do not reintroduce them: ",
        );
        s.push_str(&fixed_codes.join(", "));
        s.push_str(".\n\n");
    }

    match mode {
        RepairMode::Rewrite => s.push_str(
            "Produce a corrected DSL file that addresses every error while preserving the \
             original intent. Reply with ONLY the corrected DSL — no commentary, no markdown fences.",
        ),
        RepairMode::Edits => s.push_str(
            "Reply with one or more SEARCH/REPLACE blocks that fix every error. Use this exact format:\n\
             \n\
             <<<<<<< SEARCH\n\
             <text copied byte-for-byte from your previous attempt, including indentation>\n\
             =======\n\
             <replacement text>\n\
             >>>>>>> REPLACE\n\
             \n\
             Rules:\n\
             - Each SEARCH must appear EXACTLY ONCE in the previous attempt. If a fragment is ambiguous, expand it with surrounding context until it is unique.\n\
             - SEARCH must be non-empty.\n\
             - Emit one block per fix; multiple blocks are applied in order.\n\
             - Do not include any commentary, explanations, or markdown fences — only the blocks.",
        ),
    }
    s
}

/// One SEARCH/REPLACE block parsed from a model response. `search` and
/// `replace` are the raw strings between the markers, with the leading and
/// trailing newlines around the markers stripped (so a block whose body is
/// `"foo\n"` round-trips to a literal `"foo\n"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditBlock {
    pub search: String,
    pub replace: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("no SEARCH/REPLACE blocks found in response")]
    NoBlocks,
    #[error("malformed SEARCH/REPLACE block (missing `=======` divider or `>>>>>>> REPLACE` terminator)")]
    Malformed,
    #[error("SEARCH text not found in previous DSL: {0:?}")]
    SearchNotFound(String),
    #[error("SEARCH text matches {count} places, must be unique: {snippet:?}")]
    SearchAmbiguous { snippet: String, count: usize },
    #[error("SEARCH block is empty")]
    EmptySearch,
}

/// Parse a model response containing one or more `<<<<<<< SEARCH … >>>>>>> REPLACE`
/// blocks. Tolerates leading/trailing prose between blocks (in practice the
/// system prompt forbids it but Gemini occasionally adds a leading sentence).
/// Tolerates markdown fences wrapping the whole response.
///
/// Returns `EditError::NoBlocks` if no `<<<<<<< SEARCH` marker is present at
/// all — this is the signal the caller uses to fall back to interpreting the
/// response as a full rewrite.
pub fn parse_edit_blocks(text: &str) -> Result<Vec<EditBlock>, EditError> {
    let stripped = strip_markdown_fences(text);
    const HEAD: &str = "<<<<<<< SEARCH";
    const SEP: &str = "=======";
    const TAIL: &str = ">>>>>>> REPLACE";

    if !stripped.contains(HEAD) {
        return Err(EditError::NoBlocks);
    }

    let mut out = Vec::new();
    let mut rest = stripped.as_str();
    while let Some(head_idx) = rest.find(HEAD) {
        let after_head = &rest[head_idx + HEAD.len()..];
        // Skip the rest of the marker line.
        let after_head = match after_head.find('\n') {
            Some(p) => &after_head[p + 1..],
            None => return Err(EditError::Malformed),
        };
        // Find the divider — must be on its own line.
        let sep_idx = find_marker_line(after_head, SEP).ok_or(EditError::Malformed)?;
        let search = after_head[..sep_idx].to_string();
        let after_sep = &after_head[sep_idx + SEP.len()..];
        let after_sep = match after_sep.find('\n') {
            Some(p) => &after_sep[p + 1..],
            None => return Err(EditError::Malformed),
        };
        // Find the terminator.
        let tail_idx = find_marker_line(after_sep, TAIL).ok_or(EditError::Malformed)?;
        let replace = after_sep[..tail_idx].to_string();
        let after_tail = &after_sep[tail_idx + TAIL.len()..];
        // Strip the trailing newline on the search/replace bodies so the
        // markers themselves don't contribute. The text right before SEP is
        // the body; we don't want the "\n" that immediately precedes SEP to
        // count as part of the user's content. But we DO want to preserve
        // trailing newlines that are part of the body — so only strip the
        // single newline that's adjacent to the marker.
        let search = strip_trailing_newline(&search);
        let replace = strip_trailing_newline(&replace);
        out.push(EditBlock { search, replace });
        rest = after_tail;
    }

    if out.is_empty() {
        return Err(EditError::NoBlocks);
    }
    Ok(out)
}

/// Find the byte index of `marker` such that it sits on its own line — i.e.
/// at start-of-input or immediately after a `\n`, and followed by `\n` or
/// end-of-input. Returns the offset of the marker itself, not the line.
fn find_marker_line(haystack: &str, marker: &str) -> Option<usize> {
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(marker) {
        let abs = start + rel;
        let at_line_start = abs == 0 || haystack.as_bytes().get(abs - 1) == Some(&b'\n');
        let after = abs + marker.len();
        let at_line_end =
            after == haystack.len() || haystack.as_bytes().get(after) == Some(&b'\n');
        if at_line_start && at_line_end {
            return Some(abs);
        }
        start = abs + marker.len();
    }
    None
}

fn strip_trailing_newline(s: &str) -> String {
    if let Some(rest) = s.strip_suffix('\n') {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// Apply a sequence of edit blocks to `prev`, in order. Each SEARCH must
/// match the *current* state exactly once — so a later block can match text
/// that an earlier block produced, but if a block's SEARCH appears 0 or 2+
/// times we abort the whole apply (don't partially mutate the source).
pub fn apply_edit_blocks(prev: &str, blocks: &[EditBlock]) -> Result<String, EditError> {
    if blocks.is_empty() {
        return Err(EditError::NoBlocks);
    }
    let mut cur = prev.to_string();
    for b in blocks {
        if b.search.is_empty() {
            return Err(EditError::EmptySearch);
        }
        let count = cur.matches(&b.search).count();
        match count {
            0 => return Err(EditError::SearchNotFound(b.search.clone())),
            1 => cur = cur.replacen(&b.search, &b.replace, 1),
            n => {
                return Err(EditError::SearchAmbiguous {
                    snippet: b.search.clone(),
                    count: n,
                })
            }
        }
    }
    Ok(cur)
}

/// Render a two-line excerpt for a span: the source line containing the span
/// start, prefixed with its 1-based line number, followed by a caret
/// underline. Returns `None` if the span is out of bounds.
///
/// Multi-line spans are truncated to the first line — a caret that only marks
/// the start of the offending region is enough for the model to locate it.
fn render_span_excerpt(source: &str, span: Span) -> Option<String> {
    if span.start > source.len() {
        return None;
    }
    let before = &source[..span.start];
    let line_num = 1 + before.bytes().filter(|&b| b == b'\n').count();
    let line_start = before.rfind('\n').map(|p| p + 1).unwrap_or(0);
    let line_end = source[line_start..]
        .find('\n')
        .map(|p| line_start + p)
        .unwrap_or(source.len());
    let line = &source[line_start..line_end];
    let col = span.start - line_start + 1;

    let caret_end = span.end.min(line_end);
    let caret_len = caret_end.saturating_sub(span.start).max(1);

    let prefix = format!("  {} | ", line_num);
    let pad: String = std::iter::repeat(' ').take(prefix.len() + (col - 1)).collect();
    let carets: String = std::iter::repeat('^').take(caret_len).collect();

    let mut out = String::with_capacity(prefix.len() + line.len() + pad.len() + caret_len + 2);
    out.push_str(&prefix);
    out.push_str(line);
    out.push('\n');
    out.push_str(&pad);
    out.push_str(&carets);
    out.push('\n');
    Some(out)
}

/// Maps each diagnostic code to a short, actionable remedy. Returning
/// `Option` lets warning-only codes (Wxxxx) and future codes fall through
/// gracefully — no hint is better than a stale one.
pub fn fix_hint(code: &str) -> Option<&'static str> {
    Some(match code {
        "E0001" => "Grammar error — fix the syntactic issue first. Every node is `kind [\"name\"] (attrs) { children }`; strings must be quoted; lists use `[a, b, c]`.",
        "E0101" => "Unknown node kind — pick a kind from the kinds table (primitives like `box`/`cylinder`/`sphere`, containers like `group`/`solid`/`stack`, plus `attach`, `joint`, `clip`, `track`, `skeleton`, `bone`, or procedural animations `spin`/`open_close`/`wave`/`flap`/`idle`).",
        "E0103" => "Attribute value has the wrong type — check the kinds table. `size` is a vec3 `[x,y,z]`, numbers are plain, strings are quoted, material references use `mat=\"<name>\"`.",
        "E0104" => "Unknown material — declare `material \"<name>\" (color=[r,g,b], ...)` somewhere in the file before referencing it via `mat=\"<name>\"`.",
        "E0201" => "`material` needs a name: `material \"<name>\" (color=[r,g,b])`.",
        "E0203" => "`alpha_mode` must be one of `\"opaque\"`, `\"mask\"`, or `\"blend\"`.",
        "E0206" => "`uv_mode` must be `\"tile\"` (repeating, world-space UVs) or `\"fit\"` (per-face image UVs).",
        "E0301" => "Module needs a name: `module \"<name>\" (param=default, ...) { ... }`.",
        "E0302" => "Duplicate `module` — rename or remove the second declaration.",
        "E0303" => "Module parameter defaults must be numeric or a simple expression — strings and vec3 literals can't be defaults.",
        "E0304" => "Unknown module — check that a matching `module \"<name>\"` is declared (spelling, quoting).",
        "E0305" => "`use` requires the module name: `use \"<name>\" (arg=value, ...)`.",
        "E0401" => "Joint needs a name: `joint \"<name>\" (type=hinge|slider|ball|rotor, pivot=\"<node>\")`.",
        "E0402" => "Joint `type=` must be one of `hinge`, `slider`, `ball`, or `rotor`.",
        "E0403" => "Joint needs `pivot=\"<node>\"` — the node it rotates around.",
        "E0404" => "Joint `limits=` must be a 2-element list `[lo, hi]`.",
        "E0411" => "Clip needs a name and `seconds=`: `clip \"<name>\" (seconds=1.0) { track ... }`.",
        "E0412" => "Only `track` nodes belong inside a `clip` — move anything else out.",
        "E0413" => "`track` needs a name that matches a joint, bone, or node already declared in the scene.",
        "E0414" => "`track` needs either `from=A, to=B` (linear 2-keyframe) or `keys=[[t, v], [t, v], ...]` (multi-keyframe).",
        "E0421" => "Procedural animations (`spin`, `open_close`, `wave`, `flap`, `idle`) require `target=\"<node name>\"`.",
        "E0501" => "Skeleton needs a name: `skeleton \"<rig>\" { bone \"root\" (...) { ... } }`.",
        "E0502" => "A skeleton must contain at least one `bone`.",
        "E0503" => "Only `bone` nodes belong inside a `skeleton` — no primitives or groups.",
        "E0504" => "Bone needs a name — tracks and `skin=` binding reference bones by name.",
        "E0505" => "Inside a `bone`, only nest other `bone` nodes (children of that joint) or `connector` blocks (attachment slots that follow the joint when it animates).",
        "E0601" => "`attach` requires `parent=\"<node name>\"`.",
        "E0602" => "`attach` requires `child=\"<node name>\"`.",
        "E0701" => "Lowering/config error — usually an unresolved reference (`attach`/`track`/`skin` pointing at a name that doesn't exist) or an invalid enum value. Read the message for the specific name.",
        "E1001" => "Skin skeleton root must be an ancestor of every joint — restructure the `skeleton` so every bone lives under one root bone.",
        "E1002" | "E1005" => "Internal skin inconsistency — usually caused by a malformed rig. Verify every bone referenced by a `track` exists in the declared `skeleton`.",
        "E1003" => "`skin=\"<rig>\"` references a skeleton that doesn't exist — spell-check it against the `skeleton \"<rig>\"` declaration.",
        "E1004" => "Node has `skin=` but no mesh — remove the `skin=` attribute, or put a primitive (box/cylinder/sphere/etc.) on that node.",
        "E1006" => "Joint index out of range — a bone referenced by a `track` isn't in the declared skeleton. Add it or rename the track.",
        "E1007" => "Vertex weights don't sum to 1.0 — the mesh has vertices outside every bone's envelope. Widen `envelope=` on the nearest bone so each vertex sits inside at least one bone's radius (0.15–0.25 m for a humanoid limb).",
        "E1101" => "Disconnected part clusters — either `attach` the orphan parts to the main body (directly or through an intermediate), or put `tags=\"floating\"` on the orphan subtree (or an ancestor) if the gap is intentional (chandelier, rotor, orbiting body).",
        _ => return None,
    })
}

/// Some models wrap their output in ``` fences despite the system instruction.
/// Peel them off so the parser sees clean DSL. Idempotent: a fence-free input
/// is returned trimmed.
pub fn strip_markdown_fences(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    // Cut the first line (which may say ```mogen, ```rust, or just ```).
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
        let t = "```mogen\nscene {}\n```\n";
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
    fn repair_message_includes_code_message_prompt_and_prev_dsl() {
        let diags = vec![Diagnostic::error("E0101", "unknown node kind \"wombat\"")];
        let msg = repair_message(
            "a wombat",
            "scene { wombat \"w\" (size=[1,1,1]) }",
            &diags,
            &[],
            RepairMode::Rewrite,
        );
        assert!(msg.contains("[E0101]"));
        assert!(msg.contains("unknown node kind \"wombat\""));
        assert!(msg.contains("Original prompt: a wombat"));
        assert!(msg.contains("wombat \"w\""), "should quote the prior DSL body");
        assert!(msg.contains("no markdown fences"));
    }

    #[test]
    fn repair_message_injects_fix_hint_for_known_codes() {
        // Every code in the hint map should appear in the output with a `fix:` line.
        let diags = vec![Diagnostic::error("E1101", "scene has 2 disconnected clusters")];
        let msg = repair_message("two things", "scene {}", &diags, &[], RepairMode::Rewrite);
        assert!(msg.contains("fix:"), "expected `fix:` line, got:\n{msg}");
        assert!(
            msg.contains("`tags=\"floating\"`"),
            "E1101 hint should mention the floating escape hatch"
        );
    }

    #[test]
    fn repair_message_omits_fix_hint_for_unknown_codes() {
        // Warnings (Wxxxx) and unmapped codes should not get a bogus hint.
        let d = Diagnostic::warning("W9999", "something off");
        let msg = repair_message("x", "scene {}", &[d], &[], RepairMode::Rewrite);
        assert!(!msg.contains("fix:"), "unknown code should not produce a hint");
    }

    #[test]
    fn repair_message_renders_span_excerpt_with_caret() {
        let src = "scene {\n  wombat \"w\" (size=[1,1,1])\n}\n";
        // Span over "wombat" on line 2.
        let start = src.find("wombat").unwrap();
        let span = Span::new(start, start + "wombat".len());
        let diags = vec![Diagnostic::error("E0101", "unknown node kind \"wombat\"")
            .with_span(span)];
        let msg = repair_message("w", src, &diags, &[], RepairMode::Rewrite);
        assert!(msg.contains("  2 | "), "expected line-2 excerpt:\n{msg}");
        assert!(msg.contains("wombat \"w\""), "excerpt should contain the source line");
        assert!(msg.contains("^^^^^^"), "expected a 6-char caret under `wombat`:\n{msg}");
    }

    #[test]
    fn repair_message_lists_previously_fixed_codes() {
        let diags = vec![Diagnostic::error("E1101", "disconnected")];
        let fixed = vec!["E0101".to_string(), "E0103".to_string()];
        let msg = repair_message("thing", "scene {}", &diags, &fixed, RepairMode::Rewrite);
        assert!(msg.contains("already fixed"));
        assert!(msg.contains("E0101, E0103"));
    }

    #[test]
    fn repair_message_skips_fixed_block_when_empty() {
        let diags = vec![Diagnostic::error("E0101", "unknown")];
        let msg = repair_message("x", "scene {}", &diags, &[], RepairMode::Rewrite);
        assert!(!msg.contains("already fixed"));
    }

    #[test]
    fn repair_message_edit_mode_describes_search_replace_format() {
        let diags = vec![Diagnostic::error("E0101", "unknown node kind")];
        let msg = repair_message("x", "scene {}", &diags, &[], RepairMode::Edits);
        // Body still includes diagnostics + previous DSL.
        assert!(msg.contains("[E0101]"));
        assert!(msg.contains("scene {}"));
        // Closing instructions describe the block format.
        assert!(msg.contains("<<<<<<< SEARCH"));
        assert!(msg.contains("======="));
        assert!(msg.contains(">>>>>>> REPLACE"));
        // And do NOT ask for a corrected file.
        assert!(!msg.contains("Produce a corrected DSL file"));
    }

    #[test]
    fn is_local_only_returns_true_for_typo_class_codes() {
        let diags = vec![
            Diagnostic::error("E0101", "unknown kind"),
            Diagnostic::error("E0103", "wrong attr type"),
            Diagnostic::error("E0413", "track name"),
        ];
        assert!(is_local_only(&diags));
    }

    #[test]
    fn is_local_only_returns_false_when_any_error_is_structural() {
        let diags = vec![
            Diagnostic::error("E0101", "unknown kind"),
            Diagnostic::error("E1101", "disconnected"),
        ];
        assert!(!is_local_only(&diags));
    }

    #[test]
    fn is_local_only_returns_false_for_parse_errors() {
        let diags = vec![Diagnostic::error("E0001", "parse error")];
        assert!(!is_local_only(&diags));
    }

    #[test]
    fn is_local_only_returns_false_when_no_errors() {
        let diags: Vec<Diagnostic> = vec![];
        assert!(!is_local_only(&diags));
    }

    #[test]
    fn is_local_only_ignores_warnings() {
        let diags = vec![
            Diagnostic::warning("W9999", "style nit"),
            Diagnostic::error("E0103", "wrong type"),
        ];
        assert!(is_local_only(&diags));
    }

    #[test]
    fn parse_edit_blocks_single_block() {
        let resp = "<<<<<<< SEARCH\nfoo\n=======\nbar\n>>>>>>> REPLACE\n";
        let blocks = parse_edit_blocks(resp).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].search, "foo");
        assert_eq!(blocks[0].replace, "bar");
    }

    #[test]
    fn parse_edit_blocks_multiple_blocks() {
        let resp = "<<<<<<< SEARCH\na\n=======\nA\n>>>>>>> REPLACE\n\
                    <<<<<<< SEARCH\nb\n=======\nB\n>>>>>>> REPLACE\n";
        let blocks = parse_edit_blocks(resp).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].search, "a");
        assert_eq!(blocks[1].search, "b");
        assert_eq!(blocks[1].replace, "B");
    }

    #[test]
    fn parse_edit_blocks_tolerates_markdown_fences() {
        let resp = "```\n<<<<<<< SEARCH\nx\n=======\ny\n>>>>>>> REPLACE\n```";
        let blocks = parse_edit_blocks(resp).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].search, "x");
        assert_eq!(blocks[0].replace, "y");
    }

    #[test]
    fn parse_edit_blocks_preserves_multiline_bodies() {
        let resp = "<<<<<<< SEARCH\nbox \"a\" (size=[1,1,1])\n  child {}\n=======\nbox \"a\" (size=[2,2,2])\n  child {}\n>>>>>>> REPLACE\n";
        let blocks = parse_edit_blocks(resp).unwrap();
        assert_eq!(blocks[0].search, "box \"a\" (size=[1,1,1])\n  child {}");
        assert_eq!(blocks[0].replace, "box \"a\" (size=[2,2,2])\n  child {}");
    }

    #[test]
    fn parse_edit_blocks_no_blocks_returns_no_blocks_error() {
        let resp = "scene { box \"b\" (size=[1,1,1]) }\n";
        assert_eq!(parse_edit_blocks(resp), Err(EditError::NoBlocks));
    }

    #[test]
    fn parse_edit_blocks_missing_separator_is_malformed() {
        let resp = "<<<<<<< SEARCH\nfoo\n>>>>>>> REPLACE\n";
        assert_eq!(parse_edit_blocks(resp), Err(EditError::Malformed));
    }

    #[test]
    fn parse_edit_blocks_missing_terminator_is_malformed() {
        let resp = "<<<<<<< SEARCH\nfoo\n=======\nbar\n";
        assert_eq!(parse_edit_blocks(resp), Err(EditError::Malformed));
    }

    #[test]
    fn apply_edit_blocks_unique_match() {
        let prev = "scene { box \"b\" (size=[1,1,1]) }\n";
        let blocks = vec![EditBlock {
            search: "size=[1,1,1]".to_string(),
            replace: "size=[2,2,2]".to_string(),
        }];
        let out = apply_edit_blocks(prev, &blocks).unwrap();
        assert_eq!(out, "scene { box \"b\" (size=[2,2,2]) }\n");
    }

    #[test]
    fn apply_edit_blocks_chain_can_match_text_introduced_by_prior_block() {
        let prev = "a\n";
        let blocks = vec![
            EditBlock { search: "a".to_string(), replace: "b".to_string() },
            EditBlock { search: "b".to_string(), replace: "c".to_string() },
        ];
        assert_eq!(apply_edit_blocks(prev, &blocks).unwrap(), "c\n");
    }

    #[test]
    fn apply_edit_blocks_ambiguous_match_aborts() {
        let prev = "x x";
        let blocks = vec![EditBlock { search: "x".to_string(), replace: "y".to_string() }];
        match apply_edit_blocks(prev, &blocks) {
            Err(EditError::SearchAmbiguous { count, .. }) => assert_eq!(count, 2),
            other => panic!("expected SearchAmbiguous, got {other:?}"),
        }
    }

    #[test]
    fn apply_edit_blocks_missing_match_aborts() {
        let prev = "hello";
        let blocks = vec![EditBlock {
            search: "world".to_string(),
            replace: "earth".to_string(),
        }];
        assert!(matches!(
            apply_edit_blocks(prev, &blocks),
            Err(EditError::SearchNotFound(_))
        ));
    }

    #[test]
    fn apply_edit_blocks_empty_search_aborts() {
        let prev = "anything";
        let blocks = vec![EditBlock {
            search: String::new(),
            replace: "x".to_string(),
        }];
        assert_eq!(apply_edit_blocks(prev, &blocks), Err(EditError::EmptySearch));
    }

    #[test]
    fn span_excerpt_handles_first_line() {
        let src = "box \"a\" (size=[1])";
        let span = Span::new(0, 3);
        let out = render_span_excerpt(src, span).unwrap();
        // Line 1, column 1, 3-wide caret under `box`.
        assert!(out.contains("  1 | box"));
        assert!(out.contains("^^^"));
    }

    #[test]
    fn span_excerpt_clips_multiline_spans_to_first_line() {
        let src = "scene {\n  box\n}";
        let span = Span::new(0, src.len()); // full-file span
        let out = render_span_excerpt(src, span).unwrap();
        // Shows only line 1's content.
        assert!(out.contains("  1 | scene {"));
        assert!(!out.contains("box\n"), "should not bleed into line 2 content");
    }

    #[test]
    fn span_excerpt_returns_none_on_out_of_bounds() {
        let src = "abc";
        assert!(render_span_excerpt(src, Span::new(100, 200)).is_none());
    }

    #[test]
    fn fix_hint_covers_every_validator_error_code() {
        // If a validator starts emitting a code, the hint map should either
        // cover it or deliberately fall through. This catches silent drift.
        for code in [
            "E0001", "E0101", "E0103", "E0104",
            "E0201", "E0203", "E0206",
            "E0301", "E0302", "E0303", "E0304", "E0305",
            "E0401", "E0402", "E0403", "E0404",
            "E0411", "E0412", "E0413", "E0414", "E0421",
            "E0501", "E0502", "E0503", "E0504", "E0505",
            "E0601", "E0602", "E0701",
            "E1001", "E1002", "E1003", "E1004", "E1005", "E1006", "E1007",
            "E1101",
        ] {
            assert!(fix_hint(code).is_some(), "missing hint for {code}");
        }
        // Warnings and unknowns fall through.
        assert!(fix_hint("W0102").is_none());
        assert!(fix_hint("E9999").is_none());
    }
}
