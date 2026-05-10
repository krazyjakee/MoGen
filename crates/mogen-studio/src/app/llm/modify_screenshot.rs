//! Multi-step driver for "Modify with screenshot attached".
//!
//! When the user has the "Include screenshot" toggle on, a Modify click
//! has to render the current scene to a PNG before the LLM call so the
//! model gets the rendered image alongside the edit prompt and existing
//! DSL. The render needs the **main thread** on Windows (winit's
//! `EventLoop::new` is main-thread-only), so it can't live in the
//! `spawn_llm` worker — it has to ride the existing
//! `Viewer::submit_capture` + `CaptureKind` pipeline instead.
//!
//! Each click therefore spans a few paint frames:
//!
//! 1. UI click → [`crate::app::MogenStudioApp::start_llm_modify`]
//!    detects `mod_include_screenshot` is on, the provider supports
//!    images, and the scene is renderable, then routes through
//!    [`MogenStudioApp::submit_modify_screenshot_capture`].
//! 2. Paint loop services the capture and writes a PNG to disk.
//! 3. [`MogenStudioApp::on_modify_screenshot_render_done`] reads the
//!    bytes off disk, deletes the temp file, and dispatches the
//!    standard `spawn_llm` worker with the image attached.
//! 4. The worker calls the regular Modify path (`run_llm` →
//!    `generate_edits_with_repair`); the image rides through
//!    `cfg.user_images` and the model can see what it's editing.
//! 5. `apply_llm_outcome` applies the new DSL exactly as it would for a
//!    text-only modify call — there is no second iteration to chain.

use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

use crate::app::types::{LlmEvent, LlmEventTone, LlmKind, LlmProgress};
use crate::app::MogenStudioApp;
use crate::viewer::{CaptureFrame, CaptureKind, CaptureOutcome, CaptureRequest};

/// Edge length (px) of the modify-screenshot render. Matches the
/// user-facing `Generate Thumbnail` action and the CLI's
/// `ThumbnailOptions::default`, so the model sees the same framing it
/// would see in a CLI auto-refine pass.
const MODIFY_SCREENSHOT_SIZE: u32 = 512;
/// Pitch (radians) used for the modify-screenshot render. Matches the
/// thumbnail/CLI 3/4-angle framing — picked so silhouettes read at a
/// glance instead of dead-on flatness.
const MODIFY_SCREENSHOT_PITCH: f32 = 0.5;
/// Yaw (radians) of the modify-screenshot render. 45° matches the
/// viewer's "Frame" button + the thumbnail action.
const MODIFY_SCREENSHOT_YAW: f32 = std::f32::consts::FRAC_PI_4;

/// In-flight capture metadata, parked on the app while the GL worker
/// writes the PNG. Carries the file index + edit prompt + existing
/// source so the outcome handler can route the LLM spawn to the right
/// file even if the user switched tabs in the meantime, and so the
/// edit prompt isn't lost if the user clears the textarea after
/// clicking Modify.
pub(in crate::app) struct PendingModifyCapture {
    pub file_index: usize,
    /// Disk path the GL worker writes the PNG to. Read back in
    /// `on_modify_screenshot_render_done` and unlinked after the bytes
    /// are loaded.
    pub png_path: PathBuf,
    /// Edit prompt the user typed at click time. Snapshotted so a
    /// concurrent edit of the textarea while the render runs can't
    /// silently change what gets sent.
    pub prompt: String,
    /// DSL source as it stood at click time. Same rationale as
    /// `prompt` — the file is paired with the screenshot, so we want
    /// the version that produced the render, not whatever's in the
    /// buffer when the GL worker hands us back the PNG.
    pub existing: String,
}

impl MogenStudioApp {
    /// Push a single-frame [`CaptureKind::ModifyScreenshot`] request
    /// through the viewer for the active file's currently-compiled
    /// scene. Sets `llm_in_flight = Modify` + the headline progress +
    /// parks a [`PendingModifyCapture`] on the app so the outcome can
    /// route back to the right file.
    pub(in crate::app) fn submit_modify_screenshot_capture(
        &mut self,
        ctx: &egui::Context,
        prompt: String,
        existing: String,
    ) {
        let file_index = self.active;
        let png_path = match modify_png_path(self.active().tab_id) {
            Ok(p) => p,
            Err(e) => {
                self.active_mut().status =
                    format!("modify: cannot create capture dir — {e}");
                return;
            }
        };

        let bg = self.settings.viewer_bg_rgb();
        let label = "modify — rendering screenshot…".to_string();

        // Park the routing metadata on the app. Drained in
        // `on_modify_screenshot_render_done` regardless of success/error
        // so the slot can't leak across calls.
        self.pending_modify_capture = Some(PendingModifyCapture {
            file_index,
            png_path: png_path.clone(),
            prompt: prompt.clone(),
            existing,
        });

        let f = self.active_mut();
        f.llm_in_flight = Some(LlmKind::Modify);
        f.llm_rx = None;
        f.llm_progress = Some(LlmProgress::Status(label.clone()));
        f.llm_started_at = Some(Instant::now());
        f.llm_events.clear();
        f.llm_events.push(LlmEvent {
            at: Instant::now(),
            text: "starting modify (with screenshot)".into(),
            tone: LlmEventTone::Info,
        });
        f.llm_events.push(LlmEvent {
            at: Instant::now(),
            text: "rendering scene for the model".into(),
            tone: LlmEventTone::Info,
        });
        f.llm_last_prompt = Some((LlmKind::Modify, prompt));
        f.texture_retry_filter = None;
        f.llm_error = None;
        f.status = label;

        self.viewer.submit_capture(CaptureRequest {
            kind: CaptureKind::ModifyScreenshot,
            size: MODIFY_SCREENSHOT_SIZE,
            bg,
            frames: vec![CaptureFrame {
                yaw: MODIFY_SCREENSHOT_YAW,
                pitch: MODIFY_SCREENSHOT_PITCH,
                time: 0.0,
                path: png_path,
            }],
            total: 0,
            written: Vec::new(),
            error: None,
        });
        ctx.request_repaint();
    }

    /// Capture-outcome handler for [`CaptureKind::ModifyScreenshot`].
    /// Loads the PNG off disk, dispatches the standard Modify worker
    /// with the image attached, and surfaces any IO error through the
    /// same status line / error banner the LLM call's own failures use.
    pub(in crate::app) fn on_modify_screenshot_render_done(
        &mut self,
        ctx: &egui::Context,
        outcome: CaptureOutcome,
    ) {
        let Some(pending) = self.pending_modify_capture.take() else {
            // Outcome arrived without us asking for it. Possible if a
            // cancel cleared the slot between submit and outcome —
            // silently drop.
            return;
        };

        let i = pending.file_index;
        if i >= self.files.len() {
            // Tab closed mid-render. Nothing to write a status into.
            return;
        }

        // If the user cancelled while the GL worker was running, the
        // file's `llm_in_flight` slot was already cleared. Treat the
        // render as discarded and clean up the temp PNG.
        if self.files[i].llm_in_flight != Some(LlmKind::Modify) {
            let _ = std::fs::remove_file(&pending.png_path);
            return;
        }

        if let Some(err) = outcome.error {
            self.fail_modify_screenshot(i, format!("render failed — {err}"));
            return;
        }
        if outcome.frame_paths.is_empty() {
            self.fail_modify_screenshot(i, "render produced no output".into());
            return;
        }

        let png_bytes = match std::fs::read(&pending.png_path) {
            Ok(b) => b,
            Err(e) => {
                self.fail_modify_screenshot(
                    i,
                    format!("read render PNG {}: {e}", pending.png_path.display()),
                );
                return;
            }
        };
        let _ = std::fs::remove_file(&pending.png_path);

        // Clear the in-flight slot on the originating file before
        // calling `spawn_llm` — that function targets `self.active` and
        // refuses to start a new call if the file is already marked
        // busy. The render itself is what we were tracking; `spawn_llm`
        // re-arms the slot with `LlmKind::Modify` and the worker picks
        // up from there.
        self.files[i].llm_in_flight = None;
        self.files[i].llm_progress = None;
        self.files[i].llm_started_at = None;
        self.files[i].llm_events.clear();

        // If the user switched tabs while the GL worker was running we
        // can't sensibly route the spawn — `spawn_llm` writes through
        // `self.active`, and silently sending a modify result to the
        // wrong tab would clobber unrelated work. Surface the bail on
        // the originating file's status line and drop the render.
        if self.active != i {
            self.files[i].status =
                "modify: active tab changed mid-render — discarded screenshot".into();
            return;
        }

        let image = crate::app::types::GenImageInput {
            path: pending.png_path.clone(),
            mime_type: "image/png".to_string(),
            data: png_bytes,
            thumbnail: None,
        };
        self.spawn_llm(
            ctx.clone(),
            LlmKind::Modify,
            pending.prompt,
            Some(pending.existing),
            Some(image),
        );
    }

    /// Tear down a modify-screenshot render that failed before the LLM
    /// worker could spawn (no PNG, IO failure). Worker-side failures
    /// route through the standard `apply_llm_outcome` error path.
    fn fail_modify_screenshot(&mut self, file_index: usize, reason: String) {
        let f = &mut self.files[file_index];
        f.llm_in_flight = None;
        f.llm_rx = None;
        f.llm_progress = None;
        f.llm_started_at = None;
        f.llm_events.clear();
        f.status = format!("modify: {reason}");
    }
}

/// Resolve the per-tab PNG path the modify-screenshot capture writes
/// to. Lives under `<temp>/mogen-studio-modify-<pid>/<tab_id>.png` —
/// process-scoped so concurrent Studio instances don't trample each
/// other, tab-scoped so parallel modify-with-screenshot calls on
/// different files don't either.
fn modify_png_path(tab_id: u64) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "mogen-studio-modify-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{tab_id}.png")))
}
