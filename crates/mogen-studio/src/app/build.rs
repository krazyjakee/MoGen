use std::sync::Arc;

use eframe::egui;

use crate::pipeline::Stage;

use super::types::BuildOutcome;
use super::util::run_build;
use super::MogenStudioApp;

impl MogenStudioApp {
    /// Open the Build GLB modal with the active file's last-used options. If
    /// a build is already running, no-op — the modal stays on the in-flight
    /// one until it completes or is cancelled.
    pub(super) fn open_build_dialog(&mut self) {
        if self.build_rx.is_some() {
            self.show_export = true;
            return;
        }
        self.compile_active();
        let i = self.active;
        self.export_opts_draft = self.files[i].export_opts.clone();
        self.show_export = true;
    }

    /// Kick off a build on a worker thread. The scene is cloned into the
    /// thread so the editor can keep recompiling without racing the exporter.
    pub(super) fn spawn_build(&mut self, ctx: egui::Context) {
        // Always re-compile before spawning so we export exactly what the
        // user sees — the debounce could leave `last_result` stale by a few
        // frames otherwise.
        self.compile_active();
        let i = self.active;
        let Some(result) = &self.files[i].last_result else {
            self.files[i].status = "build: nothing compiled yet".into();
            self.show_export = false;
            return;
        };
        if result.stage != Stage::Ok {
            self.files[i].status = format!(
                "build failed at stage {:?} — see diagnostics",
                result.stage
            );
            self.show_export = false;
            return;
        }
        // Deep-clone for the worker thread: `last_result.scene` is now an
        // `Arc<SceneGraph>` shared with the viewer, but `run_build` mutates
        // (merge pass) and the worker needs an owned, independent copy.
        let scene = (**result
            .scene
            .as_ref()
            .expect("Ok implies Some(scene)"))
        .clone();
        let out = self.files[i]
            .path
            .as_ref()
            .map(|p| p.with_extension("glb"))
            .unwrap_or_else(|| self.project_root.join("untitled.glb"));
        let source_dir = self.files[i]
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());

        // Commit the draft opts back to the file so subsequent builds remember
        // them without having to reopen the modal.
        let opts = self.export_opts_draft.clone();
        self.files[i].export_opts = opts.clone();

        let (tx, rx) = std::sync::mpsc::channel();
        self.build_rx = Some(rx);
        *self.build_stage.lock().unwrap() = "starting".into();
        self.files[i].status = "building glb…".into();
        let stage = Arc::clone(&self.build_stage);
        let file_index = i;

        std::thread::spawn(move || {
            let outcome = run_build(scene, out, source_dir, opts, stage, file_index);
            let _ = tx.send(outcome);
            ctx.request_repaint();
        });
    }

    /// Drop the receiver for the in-flight build. The worker keeps running
    /// but its result is discarded — same pattern as `cancel_active_llm`.
    pub(super) fn cancel_build(&mut self) {
        if self.build_rx.is_none() {
            return;
        }
        self.build_rx = None;
        *self.build_stage.lock().unwrap() = String::new();
        let i = self.active;
        self.files[i].status =
            "build: cancelled (background worker may still finish but result is dropped)".into();
    }

    pub(super) fn poll_build(&mut self) {
        let outcome = match self.build_rx.as_ref() {
            Some(rx) => match rx.try_recv() {
                Ok(o) => o,
                Err(_) => return,
            },
            None => return,
        };
        self.build_rx = None;
        *self.build_stage.lock().unwrap() = String::new();
        self.apply_build_outcome(outcome);
    }

    pub(super) fn apply_build_outcome(&mut self, outcome: BuildOutcome) {
        let BuildOutcome { file_index, path, exported_scene, bytes, error } = outcome;
        if file_index >= self.files.len() {
            // File was closed while the build ran — nothing to do.
            return;
        }
        let msg = if let Some(err) = error {
            format!("export failed: {err}")
        } else {
            let size = bytes.unwrap_or(0);
            format!("wrote {} ({})", path.display(), format_bytes(size))
        };
        self.files[file_index].status = msg;

        // If merge was on, show the merged scene in the viewer so the user
        // sees exactly what they exported. Otherwise leave the viewer alone —
        // it already reflects the current compile result.
        if file_index == self.active {
            if let Some(scene) = exported_scene {
                let base_dir = self.files[file_index].path.as_deref().and_then(|p| p.parent());
                self.viewer.set_scene(Arc::new(scene), base_dir, false);
            }
        }

        // Leave the modal open so the user sees the result; the ui code
        // shows a Close button once `build_rx` is None.
    }
}

fn format_bytes(n: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let f = n as f64;
    if f >= GIB {
        format!("{:.2} GiB", f / GIB)
    } else if f >= MIB {
        format!("{:.2} MiB", f / MIB)
    } else if f >= KIB {
        format!("{:.1} KiB", f / KIB)
    } else {
        format!("{n} B")
    }
}
