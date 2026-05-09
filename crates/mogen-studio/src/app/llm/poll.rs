use std::fs;
use std::time::Instant;

use eframe::egui;
use mogen_core::Severity;
use mogen_llm::textures::TextureStage;

use crate::app::pricing::{cost_images, cost_text, format_usd, image_pricing, text_pricing};
use crate::app::types::{
    LlmEvent, LlmEventTone, LlmKind, LlmMessage, LlmOutcome, LlmProgress, LLM_EVENT_CAP,
};
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Drain any messages from every open file's LLM worker. Both progress
    /// updates and the final `Done` flow on the same channel, so we handle
    /// them together: progress updates the spinner caption; `Done` applies
    /// the outcome to the file.
    pub(in crate::app) fn poll_llm(&mut self, ctx: &egui::Context) {
        // Collect each file's pending messages without holding a borrow on
        // `self.files` across the apply step (which reborrows mutably).
        let mut to_apply: Vec<(usize, LlmOutcome)> = Vec::new();
        for i in 0..self.files.len() {
            let mut progress_updates: Vec<LlmProgress> = Vec::new();
            let mut done: Option<LlmOutcome> = None;
            if let Some(rx) = self.files[i].llm_rx.as_ref() {
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        LlmMessage::Progress(p) => progress_updates.push(p),
                        LlmMessage::Done(o) => {
                            done = Some(o);
                            // Drop any remaining progress after Done — it's
                            // from the same run and the outcome already knows
                            // the final state.
                            break;
                        }
                    }
                }
            }
            // Accumulate every progress event into the file's timeline so the
            // progress card can show a short history, not just the latest
            // line. `llm_progress` still tracks the most recent event since
            // it drives the headline stage caption.
            for p in &progress_updates {
                let (text, tone) = event_for_progress(p);
                let now = Instant::now();
                // Coalesce spammy duplicates (same text back-to-back) — the
                // texture worker emits Deriving events per-slot which compress
                // to one line in the UI.
                let f = &mut self.files[i];
                if f.llm_events.last().map(|e| e.text == text).unwrap_or(false) {
                    if let Some(last) = f.llm_events.last_mut() {
                        last.at = now;
                    }
                } else {
                    f.llm_events.push(LlmEvent { at: now, text, tone });
                    if f.llm_events.len() > LLM_EVENT_CAP {
                        let drop = f.llm_events.len() - LLM_EVENT_CAP;
                        f.llm_events.drain(0..drop);
                    }
                }
            }
            if let Some(last) = progress_updates.pop() {
                self.files[i].llm_progress = Some(last);
            }
            if let Some(o) = done {
                to_apply.push((i, o));
            }
        }
        for (i, outcome) in to_apply {
            self.apply_llm_outcome(ctx, i, outcome);
        }
    }

    pub(in crate::app) fn apply_llm_outcome(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        outcome: LlmOutcome,
    ) {
        // Snapshot the source as it stood before the LLM result lands so we
        // can splice the change into the editor's native undo history below.
        let pre_source = self.files[i].source.clone();
        let editor_id = egui::Id::new(("mog_editor_textedit", self.files[i].tab_id));

        let f = &mut self.files[i];
        f.llm_rx = None;
        f.llm_in_flight = None;
        f.llm_progress = None;
        f.llm_started_at = None;
        f.llm_events.clear();

        // Roll the usage into the session meter regardless of success: a
        // failed repair iteration still consumed tokens.
        let price = text_pricing(&outcome.model);
        let text_cost = cost_text(&outcome.usage, price);
        let image_price = image_pricing(&outcome.model);
        let image_cost = cost_images(outcome.image_calls, image_price);
        if outcome.calls > 0 || outcome.usage.total_tokens > 0 {
            self.session_usage
                .add_text(&outcome.usage, outcome.calls, text_cost);
        }
        if outcome.image_calls > 0 {
            self.session_usage.add_image(outcome.image_calls, image_cost);
        }

        // Textures partial-success: the run finished but some materials
        // failed. `outcome.dsl` already contains the splice for the materials
        // that did succeed and `outcome.calls` is the count of slots written
        // — so we still want to apply the DSL and save it to disk, then
        // surface the failure list as a banner. Any other kind, or a textures
        // run that produced zero edits, falls through the existing
        // hard-failure path.
        let textures_partial_success = matches!(outcome.kind, LlmKind::Textures)
            && outcome.error.is_some()
            && outcome.calls > 0;

        if let Some(info) = outcome.error.clone() {
            if !textures_partial_success {
                // Preserve retry_prompt so the user can re-submit without re-typing.
                if let Some(p) = outcome.retry_prompt {
                    f.llm_last_prompt = Some((outcome.kind, p));
                }
                let short = info.headline.clone();
                f.llm_error = Some(info);
                f.status = format!("{}: {short}", outcome.kind.label());
                return;
            }
        }

        // Remember the prompt so a post-success Retry (e.g. to re-roll the
        // seed) can reuse it.
        if let Some(p) = outcome.retry_prompt {
            f.llm_last_prompt = Some((outcome.kind, p));
        }

        let kind_label = outcome.kind.label();

        // Drop the returned DSL into the file's buffer so the user can inspect
        // / save it even when validation later fails.
        f.source = outcome.dsl;
        f.dirty = f.source != f.last_saved_source;
        f.needs_compile = true;
        f.last_edit_at = Some(Instant::now());

        // Resync the per-file thinking override from the freshly-written
        // header so the dropdown reflects whatever the run actually used
        // (covers CLI/global fallback too).
        f.thinking_override = mogen_llm::parse_thinking_header(&f.source);

        // Splice the LLM-driven replacement into the code editor's native
        // TextEdit undo history so Cmd+Z reverts to pre-LLM source and a
        // following Cmd+Shift+Z replays the LLM change. Without this push,
        // re-focusing the editor lets egui's undoer observe the changed
        // buffer mid-flight and clear the redo stack as a side-effect — so
        // changes the user undoes before the LLM runs become unreachable.
        push_llm_change_to_editor_history(ctx, editor_id, pre_source, f.source.clone());

        // Textures wrote PNG files next to the .mog; persist the spliced DSL
        // there too so the texture paths resolve on the next GLB export.
        if matches!(outcome.kind, LlmKind::Textures) {
            if let Some(p) = f.path.clone() {
                let src = f.source.clone();
                match fs::write(&p, &src) {
                    Ok(()) => {
                        f.last_saved_source = src;
                        f.dirty = false;
                    }
                    Err(e) => {
                        f.status = format!("textures: wrote PNGs but saving DSL failed: {e}");
                        return;
                    }
                }
            }
        }

        // Only refit the camera when the file that just completed is the one
        // currently on screen — otherwise a background job would yank the
        // user's view out from under them. Generate produces brand-new
        // geometry, so flag the file for a fresh fit; `compile_file` below
        // will re-frame against the new bounding sphere.
        if matches!(outcome.kind, LlmKind::Generate) && i == self.active {
            self.files[i].first_render = true;
        }

        // The editor-side undo history captured the LLM change above. Reset
        // the programmatic (gizmo/inspector) coalesce window so a subsequent
        // transform edit doesn't merge into a stack entry whose `before`
        // predates the LLM run — the two undo surfaces stay independent.
        self.break_undo_chain(i);

        self.compile_file(i);

        let has_errors = outcome
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, Severity::Error));

        let total_tokens = outcome.usage.total_tokens;
        let status = if has_errors {
            format!(
                "{kind_label}: DSL invalid after {} call(s), {} tokens ({}) — see diagnostics",
                outcome.calls,
                total_tokens,
                format_usd(text_cost),
            )
        } else if matches!(outcome.kind, LlmKind::Textures) {
            if textures_partial_success {
                format!(
                    "textures: partial — wrote {} PNG{} ({} image call(s), {}); see banner",
                    outcome.calls,
                    if outcome.calls == 1 { "" } else { "s" },
                    outcome.image_calls,
                    format_usd(image_cost),
                )
            } else {
                format!(
                    "textures: wrote {} PNG{} ({} image call(s), {})",
                    outcome.calls,
                    if outcome.calls == 1 { "" } else { "s" },
                    outcome.image_calls,
                    format_usd(image_cost),
                )
            }
        } else {
            format!(
                "{kind_label}: ready ({} call(s), {} tokens, {})",
                outcome.calls,
                total_tokens,
                format_usd(text_cost),
            )
        };
        self.files[i].status = status;

        // Surface the partial-failure banner *after* the DSL has been written
        // and the file recompiled, so the user sees the spliced PNGs in the
        // viewer alongside the "N material(s) failed" notice. `outcome.error`
        // is `Some(_)` on this branch by definition (textures_partial_success).
        // Downgrade the error class to Partial so the banner reads as a soft
        // warning rather than a red hard-failure. The headline gets a
        // "Partial:" prefix so users see at a glance that some materials did
        // succeed before reading the detail.
        if textures_partial_success {
            if let Some(mut info) = outcome.error {
                info.class = crate::app::types::LlmErrorClass::Partial;
                if !info.headline.to_lowercase().starts_with("partial") {
                    info.headline = format!(
                        "Partial: {} material PNG{} written — {}",
                        outcome.calls,
                        if outcome.calls == 1 { "" } else { "s" },
                        info.headline,
                    );
                }
                self.files[i].llm_error = Some(info);
            }
        }
    }
}

/// Collapse an `LlmProgress` into the string + tone that the timeline renders.
/// Kept outside the impl so `ui_llm.rs` can't accidentally call it — the UI
/// reads baked `LlmEvent`s rather than formatting progress variants inline.
fn event_for_progress(p: &LlmProgress) -> (String, LlmEventTone) {
    match p {
        LlmProgress::Status(s) => {
            let tone = if s.contains("done") {
                LlmEventTone::Done
            } else {
                LlmEventTone::Info
            };
            (s.clone(), tone)
        }
        LlmProgress::Repair { iter, max, errors } => (
            format!(
                "repair {iter}/{max} · {errors} error{}",
                if *errors == 1 { "" } else { "s" }
            ),
            LlmEventTone::Repair,
        ),
        LlmProgress::Texture {
            current,
            total,
            material,
            stage,
        } => {
            let verb = match stage {
                TextureStage::Generating => "generating",
                TextureStage::Existing => "using existing PNG",
                TextureStage::Deriving => "deriving PBR",
                TextureStage::Done => "finished",
                TextureStage::Failed => "failed",
            };
            (
                format!("{current}/{total} · {verb} — {material}"),
                LlmEventTone::Texture,
            )
        }
    }
}

/// Splice a wholesale LLM-driven source replacement into the editor
/// `TextEdit`'s native undo stack so users can Cmd+Z back to the pre-LLM
/// buffer and Cmd+Shift+Z to replay the LLM change. Two `add_undo`s with a
/// `feed_state` between them ensure pre-LLM is the latest entry, the stale
/// redo stack from any prior in-editor undos is cleared, and post-LLM lands
/// as the new tip — egui's undoer does this automatically only while the
/// widget owns focus, which it does not during an LLM call.
fn push_llm_change_to_editor_history(
    ctx: &egui::Context,
    editor_id: egui::Id,
    pre: String,
    post: String,
) {
    if pre == post {
        return;
    }
    let mut state = egui::TextEdit::load_state(ctx, editor_id).unwrap_or_default();
    let cursor = state.cursor.char_range().unwrap_or_default();
    let mut undoer = state.undoer();
    let now = ctx.input(|i| i.time);
    undoer.add_undo(&(cursor, pre));
    // `feed_state` clears `redos` whenever the new state differs from
    // `undos.back()` — exploited here to drop redo entries that pointed at a
    // timeline the LLM just forked away from. The follow-up `add_undo` is
    // what actually pushes the post-LLM tip onto the stack.
    undoer.feed_state(now, &(cursor, post.clone()));
    undoer.add_undo(&(cursor, post));
    state.set_undoer(undoer);
    state.store(ctx, editor_id);
}
