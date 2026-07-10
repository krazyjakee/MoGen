//! App-side glue for the remote-control web UI (`crate::remote`).
//!
//! Everything runs on the UI thread inside `update()`: reconcile the server
//! lifecycle with the persisted settings, drain the command queue the HTTP
//! thread filled, publish a fresh state snapshot when anything observable
//! changed, and drive the turntable preview captures while a browser is
//! watching. The HTTP thread itself never touches `MogenStudioApp`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use mogen_core::Severity;

use crate::pipeline::Stage;
use crate::remote::{DiagInfo, RemoteCommand, RemoteServer, Snapshot, TabInfo};
use crate::viewer::{CaptureFrame, CaptureKind, CaptureRequest};

use super::types::UndoKey;
use super::MogenStudioApp;

/// Minimum gap between remote preview captures. Two frames a second is
/// plenty for a dashboard turntable and keeps the GL cost invisible next to
/// normal viewport painting.
const PREVIEW_INTERVAL: Duration = Duration::from_millis(500);

/// Edge length of the square preview render. Matches the thumbnail size —
/// small enough to encode fast, big enough to look crisp in the dashboard.
const PREVIEW_SIZE: u32 = 512;

/// Yaw advance per captured frame (radians). One full turn every ~30 s at
/// the capture cadence — slow enough to read the model, alive enough to
/// show it's a live view.
const PREVIEW_YAW_STEP: f32 = 0.105;

impl MogenStudioApp {
    /// One per-frame tick of everything remote. Called from `update()`.
    pub(super) fn drive_remote(&mut self, ctx: &egui::Context) {
        self.reconcile_remote(ctx);
        if self.remote.is_none() {
            return;
        }
        self.apply_remote_commands(ctx);
        self.publish_remote_state();
        self.drive_remote_preview(ctx);
        self.poll_remote_preview();
    }

    /// Start / stop / restart the server so it matches the settings. Bind
    /// errors land in `remote_error` for Preferences to display.
    fn reconcile_remote(&mut self, ctx: &egui::Context) {
        let want = self.settings.remote_enabled();
        let port = self.settings.remote_port();
        let lan = self.settings.remote_allow_lan();

        let needs_restart = self
            .remote
            .as_ref()
            .map(|s| s.port() != port || s.allow_lan() != lan)
            .unwrap_or(false);

        if !want || needs_restart {
            if self.remote.take().is_some() {
                // Force a fresh snapshot after a restart so a rebound server
                // doesn't serve `null` until something changes.
                self.remote_last_snapshot = None;
            }
            if !want {
                self.remote_error = None;
                return;
            }
        }
        if self.remote.is_some() {
            return;
        }
        match RemoteServer::start(port, lan, ctx.clone()) {
            Ok(server) => {
                self.remote = Some(server);
                self.remote_error = None;
            }
            Err(e) => {
                // Latch the error and flip the setting back off so the app
                // doesn't retry the failing bind every frame. The user sees
                // the message in Preferences › Remote and can pick another
                // port.
                self.remote_error = Some(e);
                self.settings.set_remote_enabled(false);
            }
        }
    }

    /// Apply every queued browser command. Tab indices are re-validated here
    /// because tabs can open/close between enqueue and drain.
    fn apply_remote_commands(&mut self, ctx: &egui::Context) {
        let Some(server) = self.remote.as_ref() else {
            return;
        };
        let commands = server.take_commands();
        for cmd in commands {
            match cmd {
                RemoteCommand::ActivateTab(i) => {
                    if i < self.files.len() && i != self.active {
                        self.activate(i);
                    }
                }
                RemoteCommand::SetSource { tab, source } => {
                    if tab >= self.files.len() {
                        continue;
                    }
                    // Same guard as the gizmo path: an in-flight LLM call
                    // overwrites `source` on completion, so a remote edit
                    // now would silently vanish.
                    if self.files[tab].llm_in_flight.is_some() {
                        self.files[tab].status =
                            "remote edit ignored — an LLM call is rewriting this file".into();
                        continue;
                    }
                    let before = self.files[tab].source.clone();
                    if before == source {
                        continue;
                    }
                    {
                        let f = &mut self.files[tab];
                        f.source = source;
                        f.dirty = f.source != f.last_saved_source;
                        f.needs_compile = true;
                        f.last_edit_at = Some(Instant::now());
                        f.status = "remote: source updated".into();
                    }
                    // Remote replaces are discrete actions — never coalesce
                    // them with in-app edits.
                    self.break_undo_chain(tab);
                    self.push_undo(
                        tab,
                        before,
                        UndoKey {
                            surface: "remote",
                            attr: None,
                            node_path: Vec::new(),
                        },
                    );
                    if tab == self.active {
                        self.compile_active();
                    } else {
                        self.compile_file(tab);
                    }
                }
                RemoteCommand::Save { tab } => {
                    if tab >= self.files.len() {
                        continue;
                    }
                    match self.files[tab].path.clone() {
                        Some(path) => self.save_index_to(tab, &path),
                        None => {
                            // Popping a native Save As dialog from a remote
                            // command would ambush whoever is at the desk.
                            self.files[tab].status =
                                "remote save skipped — untitled buffer needs Save As in Studio"
                                    .into();
                        }
                    }
                }
                RemoteCommand::Build { tab } => {
                    if tab >= self.files.len() {
                        continue;
                    }
                    if self.build_rx.is_some() || self.imposter_export_pending.is_some() {
                        self.files[tab].status =
                            "remote build skipped — a build is already running".into();
                        continue;
                    }
                    if tab != self.active {
                        self.activate(tab);
                    }
                    // Reuse the tab's remembered export options — the modal
                    // draft is what `spawn_build` commits back to the file.
                    self.export_opts_draft = self.files[self.active].export_opts.clone();
                    self.spawn_build(ctx.clone());
                }
                RemoteCommand::Recompile { tab } => {
                    if tab < self.files.len() {
                        self.compile_file(tab);
                    }
                }
            }
        }
    }

    /// Rebuild the dashboard snapshot and publish it if anything changed.
    /// The rebuild is cheap (string clones of hand-sized `.mog` sources);
    /// the `PartialEq` gate keeps idle frames from re-serializing.
    fn publish_remote_state(&mut self) {
        let Some(server) = self.remote.as_ref() else {
            return;
        };
        let f = &self.files[self.active];
        let result = f.last_result.as_ref();

        let stage = match result.map(|r| r.stage) {
            None => "none",
            Some(Stage::Ok) => "ok",
            Some(Stage::Parse) => "parse",
            Some(Stage::ValidateAst) => "validate-ast",
            Some(Stage::Lower) => "lower",
            Some(Stage::ValidateGraph) => "validate-graph",
        };

        let diagnostics = result
            .map(|r| {
                r.diagnostics
                    .iter()
                    .map(|d| DiagInfo {
                        severity: match d.severity {
                            Severity::Error => "error",
                            Severity::Warning => "warning",
                            Severity::Info => "info",
                        }
                        .to_string(),
                        code: d.code.clone(),
                        message: d.message.clone(),
                        line: d.span.map(|s| line_of_offset(&f.source, s.start)),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let (nodes, triangles) = result
            .and_then(|r| r.scene.as_ref())
            .map(|s| {
                let tris: usize = s
                    .nodes
                    .iter()
                    .filter_map(|n| n.mesh.as_ref())
                    .map(|m| m.indices.len() / 3)
                    .sum();
                (s.nodes.len(), tris)
            })
            .unwrap_or((0, 0));

        let snapshot = Snapshot {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            tabs: self
                .files
                .iter()
                .map(|f| TabInfo {
                    name: f.display_name(),
                    path: f.path.as_ref().map(|p| p.display().to_string()),
                    dirty: f.dirty,
                })
                .collect(),
            active: self.active,
            source: f.source.clone(),
            status: f.status.clone(),
            stage: stage.to_string(),
            diagnostics,
            nodes,
            triangles,
            building: self.build_rx.is_some() || self.imposter_export_pending.is_some(),
            build_stage: self.build_stage.lock().unwrap().clone(),
            llm: f.llm_in_flight.map(|k| k.label().to_string()),
        };

        if self.remote_last_snapshot.as_ref() != Some(&snapshot) {
            server.publish_state(&snapshot);
            self.remote_last_snapshot = Some(snapshot);
        }
    }

    /// Submit the next turntable frame while a browser is watching the
    /// preview. Skips whenever the single capture slot is busy with real
    /// user work (thumbnails, video, publish, picker thumbs) — the remote
    /// preview is strictly lowest-priority.
    fn drive_remote_preview(&mut self, ctx: &egui::Context) {
        let Some(server) = self.remote.as_ref() else {
            return;
        };
        if !server.preview_watchers_active() {
            return;
        }
        // Keep the loop ticking even with no local input: captures complete
        // inside paint callbacks, so somebody has to keep requesting frames.
        ctx.request_repaint_after(PREVIEW_INTERVAL);

        if !self.viewer.has_scene()
            || self.generate_in_flight()
            || self.picker.is_some()
            || self.thumbnail_mgr.is_busy()
            || self.pending_modify_capture.is_some()
            || self.imposter_export_pending.is_some()
        {
            return;
        }
        let due = self
            .remote_preview_last_submit
            .map(|t| t.elapsed() >= PREVIEW_INTERVAL)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.remote_preview_last_submit = Some(Instant::now());
        self.remote_preview_yaw =
            (self.remote_preview_yaw + PREVIEW_YAW_STEP) % std::f32::consts::TAU;
        self.viewer.submit_capture(CaptureRequest {
            kind: CaptureKind::RemotePreview,
            size: PREVIEW_SIZE,
            bg: self.settings.viewer_bg_rgb(),
            frames: vec![CaptureFrame {
                yaw: self.remote_preview_yaw,
                pitch: 0.5,
                time: 0.0,
                path: remote_preview_path(),
            }],
            // submit_capture overwrites these bookkeeping fields.
            total: 0,
            written: Vec::new(),
            error: None,
        });
    }

    /// Drain a finished remote-preview capture and hand the PNG bytes to the
    /// server. Errors are dropped silently — the dashboard keeps its last
    /// good frame and the next tick retries.
    fn poll_remote_preview(&mut self) {
        let Some(server) = self.remote.as_ref() else {
            return;
        };
        let Some(outcome) = self
            .viewer
            .take_capture_outcome_if(|kind| matches!(kind, CaptureKind::RemotePreview))
        else {
            return;
        };
        if outcome.error.is_some() {
            return;
        }
        let Some(path) = outcome.frame_paths.last() else {
            return;
        };
        if let Ok(bytes) = std::fs::read(path) {
            server.publish_preview(bytes);
        }
    }
}

/// 1-based line number of a byte offset. Offsets past EOF clamp to the last
/// line — diagnostics can outlive the exact source they were minted from by
/// a frame.
fn line_of_offset(source: &str, offset: usize) -> usize {
    let end = offset.min(source.len());
    source[..end].bytes().filter(|&b| b == b'\n').count() + 1
}

/// Scratch path the GL worker writes preview frames to. Fixed per-process
/// so successive captures overwrite instead of accumulating.
fn remote_preview_path() -> PathBuf {
    std::env::temp_dir().join(format!("mogen-remote-preview-{}.png", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::line_of_offset;

    #[test]
    fn line_of_offset_basics() {
        let src = "a\nbb\nccc\n";
        assert_eq!(line_of_offset(src, 0), 1);
        assert_eq!(line_of_offset(src, 2), 2);
        assert_eq!(line_of_offset(src, 5), 3);
        // Past-EOF clamps instead of panicking.
        assert_eq!(line_of_offset(src, 999), 4);
    }
}
