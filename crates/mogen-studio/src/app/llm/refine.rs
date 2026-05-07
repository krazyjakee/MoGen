//! Visual auto-refinement loop for the studio LLM inspector.
//!
//! Mirrors `mogen generate --auto-refine N` / `mogen modify --auto-refine N`
//! from the CLI: render the active scene to a thumbnail, hand the PNG +
//! current DSL to a vision-capable provider as a Reviewer turn, replay the
//! corrected DSL through the validate+repair loop, and apply the refined
//! source to the active buffer.
//!
//! Why a multi-step driver instead of one worker thread:
//!
//! - Each iteration needs a *fresh* render of the just-applied DSL — the
//!   Reviewer critiques what its previous pass actually produced, not the
//!   pre-refine geometry.
//! - The renderer (`mogen-render::headless::render_thumbnail`) requires the
//!   process **main thread** on Windows because winit's `EventLoop::new`
//!   can only be constructed there. The Studio main thread is the egui
//!   paint loop. The CLI works around this by being single-threaded; the
//!   Studio works around it by reusing its existing main-thread capture
//!   pipeline (`Viewer::submit_capture` + `CaptureKind`) instead of
//!   pulling `mogen-render` in directly.
//!
//! Each iteration therefore spans a few paint frames:
//!
//! 1. UI click → [`MogenStudioApp::start_llm_refine`] sets up
//!    [`crate::app::types::RefineSession`] + posts a [`CaptureKind::Refine`]
//!    request through the viewer.
//! 2. Paint loop services the capture and writes a PNG to disk.
//! 3. [`MogenStudioApp::on_refine_render_done`] reads the bytes off disk
//!    and dispatches a worker via
//!    [`MogenStudioApp::spawn_llm_refine`].
//! 4. Worker calls `mogen_llm::visual_refine`, returns
//!    [`crate::app::types::LlmOutcome`].
//! 5. `apply_llm_outcome` applies the new DSL, recompiles, and (if
//!    `iters_remaining > 1`) loops back to step 1 for the next pass.
//! 6. `cancel_active_llm` clears the session at any step so an Esc
//!    mid-iteration does not silently kick a follow-up pass.

use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

use crate::app::types::{
    LlmEvent, LlmEventTone, LlmKind, LlmProgress, RefineSession,
};
use crate::app::MogenStudioApp;
use crate::viewer::{CaptureFrame, CaptureKind, CaptureOutcome, CaptureRequest};

/// Edge length (px) of the refine source render. Matches the user-facing
/// `Generate Thumbnail` action and the CLI's `ThumbnailOptions::default`,
/// so a CLI `--auto-refine` and a Studio Refine of the same `.mog` see the
/// same image and produce comparable critiques.
const REFINE_SIZE: u32 = 512;
/// Pitch (radians) used for the refine render. Matches the thumbnail/CLI
/// 3/4-angle framing — picked so silhouettes read at a glance instead of
/// dead-on flatness, which the Reviewer would dismiss as un-critiquable.
const REFINE_PITCH: f32 = 0.5;
/// Yaw (radians) of the refine render. 45° matches the viewer's "Frame"
/// button + the thumbnail action; consistent framing across iterations
/// stops the Reviewer chasing imagined orientation changes.
const REFINE_YAW: f32 = std::f32::consts::FRAC_PI_4;
/// Hard cap on iterations exposed to the user. Mirrors the CLI's
/// `clap::value_parser!(u32).range(0..=10)` cap for `--auto-refine`. We
/// floor at 1 in the start path; the UI uses [`MIN_REFINE_ITERS`] for the
/// drag-value range.
pub(in crate::app) const MAX_REFINE_ITERS: u32 = 5;
/// UI floor on iterations. `0` would be a no-op so the spinbox starts at
/// 1; the CLI accepts `0` only as "skip refinement" which is meaningless
/// for an explicit button click.
pub(in crate::app) const MIN_REFINE_ITERS: u32 = 1;

/// In-flight refine capture metadata, parked on the app while the GL
/// worker writes the PNG. Carried separately from [`RefineSession`] (which
/// lives on the file) because the capture-outcome routing path needs to
/// recover the file index from the *outcome*, not from the active tab —
/// the user can switch tabs while the GL worker is running.
pub(in crate::app) struct PendingRefineCapture {
    /// File the capture was submitted for. The outcome routes back to
    /// this index even if the user switched tabs in the meantime; the
    /// driver then bails the session if the active tab changed (we don't
    /// want to silently feed a render of file A into a Reviewer turn for
    /// file B).
    pub file_index: usize,
    /// Disk path the GL worker writes the PNG to. Read back in
    /// `on_refine_render_done` and unlinked after the bytes are loaded
    /// so a stale file from a previous iteration can't be picked up.
    pub png_path: PathBuf,
}

impl MogenStudioApp {
    /// Kick off a multi-iteration visual auto-refinement loop on the
    /// active file. Validates UI gates, captures the original prompt
    /// header to critique against, and submits the first
    /// [`CaptureKind::Refine`] render. Subsequent iterations chain
    /// through `apply_llm_outcome` → `submit_refine_capture` until
    /// `iters_remaining` hits zero.
    pub(in crate::app) fn start_llm_refine(
        &mut self,
        ctx: egui::Context,
        iters: u32,
    ) {
        // Mirror the CLI's `--auto-refine 0 → no-op` semantics, just
        // surfaced as a click that does nothing instead of a parse error.
        let iters = iters.clamp(MIN_REFINE_ITERS, MAX_REFINE_ITERS);

        // Same gate ordering as the UI button — the UI disables the
        // button on each of these, but we re-check here so a
        // keyboard-driven retry path (which doesn't go through the
        // gating UI) can't bypass them.
        let provider = self.settings.provider();
        if !provider.supports_images() {
            self.active_mut().status = format!(
                "refine: {} does not support image input — switch to Gemini in Edit \
                 → Preferences (only vision-capable providers can refine from a render)",
                provider.label(),
            );
            return;
        }
        if self.resolve_credential().is_none() {
            self.active_mut().status =
                format!("refine: no {} credential — set one in Edit → Preferences…", provider.label());
            return;
        }
        if self.active().llm_in_flight.is_some() {
            self.active_mut().status = "refine: another LLM call is already in flight on this file".into();
            return;
        }
        let src_empty = self.active().source.trim().is_empty();
        if src_empty {
            self.active_mut().status = "refine: nothing to render — load or generate a scene first".into();
            return;
        }
        // The Reviewer needs a renderable scene. If the buffer doesn't
        // compile we'd render the *previous* scene (or nothing), which
        // would mislead the Reviewer about the current state of the
        // file. Bail until validation passes — the user can hit Repair
        // first, then Refine.
        let scene_ok = self
            .active()
            .last_result
            .as_ref()
            .map(|r| r.scene.is_some() && !mogen_core::has_errors(&r.diagnostics))
            .unwrap_or(false);
        if !scene_ok {
            self.active_mut().status = "refine: fix validation errors first — Reviewer needs a renderable scene".into();
            return;
        }

        // Capture the original prompt the Reviewer will critique against.
        // Prefer the `meta(prompt=…)` header (set by every prior LLM
        // generate/modify pass and by hand-edited scenes that copy the
        // CLI convention) over the modify-prompt textarea, so a chain of
        // Modify-then-Refine still hands the *original* asset
        // description to the Reviewer rather than the most recent
        // verb-phrase edit.
        let original_prompt = mogen_llm::parse_prompt_header(&self.active().source)
            .unwrap_or_else(|| {
                let mp = self.active().mod_prompt.trim();
                if mp.is_empty() {
                    "Recreate the scene this DSL describes".to_string()
                } else {
                    mp.to_string()
                }
            });

        let i = self.active;
        let f = self.active_mut();
        f.refine_session = Some(RefineSession {
            iters_remaining: iters,
            iters_total: iters,
            original_prompt,
            file_index: i,
        });
        // Clear any leftover error banner from a prior run on this file
        // — the next failure (if any) should belong to this fresh
        // session, not to a previous Modify/Repair pass.
        f.llm_error = None;

        self.submit_refine_capture(&ctx);
    }

    /// Push a single-frame [`CaptureKind::Refine`] request through the
    /// viewer for the active file's currently-compiled scene. Sets
    /// `llm_in_flight` + the headline progress + parks a
    /// [`PendingRefineCapture`] on the app so the outcome can route back
    /// to the right file. Called once at session start and once at the
    /// top of each follow-up iteration.
    pub(in crate::app) fn submit_refine_capture(&mut self, ctx: &egui::Context) {
        // The session must already be set up by the caller — fail loudly
        // (status line) rather than silently kicking a render with no
        // metadata, which would leak `llm_in_flight = Refine` with no
        // exit path.
        let Some((iters_remaining, iters_total, file_index)) = self
            .active()
            .refine_session
            .as_ref()
            .map(|s| (s.iters_remaining, s.iters_total, s.file_index))
        else {
            self.active_mut().status =
                "refine: internal error — no session for capture submit".into();
            return;
        };
        if file_index != self.active {
            // The user switched tabs before we got to submit. Cancel
            // the session rather than render the wrong file.
            self.files[file_index].refine_session = None;
            self.files[file_index].llm_in_flight = None;
            self.files[file_index].status =
                "refine: active tab changed — cancelling session".into();
            return;
        }

        let png_path = match refine_png_path(self.active().tab_id) {
            Ok(p) => p,
            Err(e) => {
                let f = self.active_mut();
                f.refine_session = None;
                f.llm_in_flight = None;
                f.status = format!("refine: cannot create capture dir — {e}");
                return;
            }
        };

        let bg = self.settings.viewer_bg_rgb();
        // Compute progress label before the &mut borrow on f to keep
        // borrow scopes tidy.
        let pass_index = iters_total - iters_remaining + 1;
        let label = format!("refine {pass_index}/{iters_total} — rendering…");

        // Park the routing metadata on the app. Drained in
        // `on_refine_render_done` regardless of success/error so the
        // slot can't leak across iterations.
        self.pending_refine_capture = Some(PendingRefineCapture {
            file_index,
            png_path: png_path.clone(),
        });

        let f = self.active_mut();
        f.llm_in_flight = Some(LlmKind::Refine);
        f.llm_rx = None;
        f.llm_progress = Some(LlmProgress::Status(label.clone()));
        f.llm_started_at = Some(Instant::now());
        f.llm_events.clear();
        f.llm_events.push(LlmEvent {
            at: Instant::now(),
            text: format!("starting refine {pass_index}/{iters_total}"),
            tone: LlmEventTone::Info,
        });
        f.llm_events.push(LlmEvent {
            at: Instant::now(),
            text: "rendering scene for reviewer".into(),
            tone: LlmEventTone::Info,
        });
        // Stash the synthetic prompt for the Retry path. Refine has no
        // editable prompt field — the iteration count is the only knob
        // — so the label just records "this was a refine of N passes".
        f.llm_last_prompt = Some((LlmKind::Refine, format!("refine {iters_total}×")));
        // Clear any stale textures-retry filter. Same rationale as
        // `spawn_llm`: a textures retry should not pick up a filter
        // populated by a previous regenerate-one-material click.
        f.texture_retry_filter = None;
        f.status = label;

        self.viewer.submit_capture(CaptureRequest {
            kind: CaptureKind::Refine,
            size: REFINE_SIZE,
            bg,
            frames: vec![CaptureFrame {
                yaw: REFINE_YAW,
                pitch: REFINE_PITCH,
                time: 0.0,
                path: png_path,
            }],
            // submit_capture overwrites total/written/error before
            // queuing — placeholder values keep the literal honest.
            total: 0,
            written: Vec::new(),
            error: None,
        });
        ctx.request_repaint();
    }

    /// Capture-outcome handler for [`CaptureKind::Refine`]. Loads the PNG
    /// off disk, dispatches the LLM worker, and surfaces any IO error
    /// through the same error banner the Reviewer's own failures use.
    pub(in crate::app) fn on_refine_render_done(
        &mut self,
        ctx: &egui::Context,
        outcome: CaptureOutcome,
    ) {
        // Drain the parked routing metadata even on error — leaving it
        // would bind the next non-refine capture to a stale file index
        // if the renderer's own error path produced an outcome with no
        // frame_paths.
        let Some(pending) = self.pending_refine_capture.take() else {
            // Outcome arrived without us asking for it. Possible if
            // `cancel_active_llm` cleared the session between submit
            // and outcome — silently drop. The PNG (if any) gets
            // cleaned up by the next iteration's submit overwriting it.
            return;
        };

        let i = pending.file_index;
        if i >= self.files.len() {
            // Tab closed mid-render. Drop quietly — there's no file
            // state left to write a status into.
            return;
        }

        // If the session was already cleared (cancel, switch-tab) the
        // file's refine_session is None. Treat the render as discarded
        // and clear the in-flight slot — same shape as the cancel path.
        if self.files[i].refine_session.is_none() {
            self.files[i].llm_in_flight = None;
            self.files[i].llm_progress = None;
            self.files[i].llm_started_at = None;
            self.files[i].llm_events.clear();
            return;
        }

        if let Some(err) = outcome.error {
            self.fail_refine_session(i, format!("render failed — {err}"));
            return;
        }

        // Use the path we asked the GL worker to write to, not
        // `outcome.frame_paths.last()` — they should match, but reading
        // straight from `pending` removes a round-trip dependency on
        // the capture pipeline preserving the path verbatim. Bail when
        // `frame_paths` is empty since that signals the worker never
        // produced an output even though `outcome.error` was None
        // (would happen only on a logic bug in the capture path, but
        // it's cheap to check).
        if outcome.frame_paths.is_empty() {
            self.fail_refine_session(i, "render produced no output".into());
            return;
        }
        let png_path = pending.png_path.clone();

        let png_bytes = match std::fs::read(&png_path) {
            Ok(b) => b,
            Err(e) => {
                self.fail_refine_session(
                    i,
                    format!("read render PNG {}: {e}", png_path.display()),
                );
                return;
            }
        };

        // Best-effort cleanup — the next iteration will overwrite this
        // path anyway, but unlinking now means a crash mid-call leaves
        // less debris in the temp dir. Ignore failures: read_to_end
        // already succeeded so the bytes are safely in memory.
        let _ = std::fs::remove_file(&png_path);

        // Snapshot the DSL we're handing to the Reviewer alongside the
        // image so the two are paired for this iteration. Reading the
        // buffer here is correct because:
        //   - The editor TextEdit is rate-limited to debounce edits, so
        //     a typing burst in the last few ms can't have re-rewritten
        //     the source between submit_capture and now.
        //   - apply_llm_outcome flips needs_compile + recompiles before
        //     submitting the next iteration's capture, so by the time
        //     the GL worker hands the PNG back, source matches what was
        //     drawn.
        let current_dsl = self.files[i].source.clone();
        let original_prompt = self.files[i]
            .refine_session
            .as_ref()
            .map(|s| s.original_prompt.clone())
            .unwrap_or_default();

        self.spawn_llm_refine(ctx.clone(), i, png_bytes, original_prompt, current_dsl);
    }

    /// Tear down the refine session for `file_index` with `reason`
    /// shoved into the status. Used when a render or read step fails
    /// before the worker even spawns — the worker-side failures route
    /// through the standard `apply_llm_outcome` error path, so this is
    /// exclusively the GL-pipeline pre-spawn failure shape.
    fn fail_refine_session(&mut self, file_index: usize, reason: String) {
        let f = &mut self.files[file_index];
        f.refine_session = None;
        f.llm_in_flight = None;
        f.llm_rx = None;
        f.llm_progress = None;
        f.llm_started_at = None;
        f.llm_events.clear();
        f.status = format!("refine: {reason}");
    }
}

/// Resolve the per-tab PNG path the refine capture writes to. Lives under
/// `<temp>/mogen-studio-refine-<pid>/<tab_id>.png` — process-scoped so
/// concurrent Studio instances don't trample each other, tab-scoped so
/// parallel refine sessions on different files (allowed by the
/// per-file `llm_in_flight` slot) don't either.
fn refine_png_path(tab_id: u64) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "mogen-studio-refine-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{tab_id}.png")))
}
