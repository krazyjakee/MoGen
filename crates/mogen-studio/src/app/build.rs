use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use mogen_core::SceneGraph;
use mogen_export::{ExportOptions, ImposterAtlas};

use crate::pipeline::Stage;

use super::preview::{
    IMPOSTER_PREVIEW_CELL_SIZE, IMPOSTER_PREVIEW_PITCH, IMPOSTER_PREVIEW_VIEW_COUNT,
};
use super::types::BuildOutcome;
use super::util::run_build;
use super::MogenStudioApp;

/// Build state held while the viewer pre-bakes the imposter atlas. Once
/// the bake lands, [`MogenStudioApp::poll_imposter_export`] drains the
/// outcome and spawns the build worker with the atlas in hand. We hold
/// the full set of build args here so the spawn doesn't have to re-derive
/// any of them from the (possibly mutated) app state.
pub(in crate::app) struct PendingImposterExport {
    pub scene: SceneGraph,
    pub out: PathBuf,
    pub source_dir: Option<PathBuf>,
    pub opts: ExportOptions,
    pub file_index: usize,
    pub ctx: egui::Context,
}

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

        // When the bundle-LODs option is on we must pre-bake the imposter
        // atlas on the viewer's GL thread before launching the build
        // worker: eframe owns the only winit `EventLoop` in this process,
        // so the writer's own headless bake would fail with
        // `EventLoopError::RecreationAttempt`. Stash the build args, queue
        // the bake on the viewer, and let `poll_imposter_export` spawn
        // the worker once the atlas lands.
        if opts.bundle_lods_and_imposter {
            *self.build_stage.lock().unwrap() = "baking imposter atlas".into();
            self.files[i].status = "baking imposter atlas…".into();
            let scene_arc = Arc::new(scene.clone());
            self.viewer
                .submit_imposter_request(crate::viewer::ImposterRequest {
                    scene: scene_arc,
                    cell_size: IMPOSTER_PREVIEW_CELL_SIZE,
                    view_count: IMPOSTER_PREVIEW_VIEW_COUNT,
                    pitch: IMPOSTER_PREVIEW_PITCH,
                    base_dir: source_dir.clone(),
                });
            self.imposter_export_pending = Some(PendingImposterExport {
                scene,
                out,
                source_dir,
                opts,
                file_index: i,
                ctx: ctx.clone(),
            });
            ctx.request_repaint();
            return;
        }

        spawn_build_worker(self, ctx, scene, out, source_dir, opts, None, i);
    }

    /// Drain a finished imposter bake belonging to an in-flight export.
    /// On success, spawns the build worker with the pre-baked atlas; on
    /// failure, surfaces the bake error as the build outcome (no GLB is
    /// written, the user sees the error in the status line and the build
    /// dialog).
    pub(super) fn poll_imposter_export(&mut self) {
        if self.imposter_export_pending.is_none() {
            return;
        }
        let Some(outcome) = self.viewer.take_imposter_outcome() else {
            return;
        };
        let pending = self
            .imposter_export_pending
            .take()
            .expect("checked above");
        match outcome {
            Ok(atlas) => {
                let PendingImposterExport {
                    scene,
                    out,
                    source_dir,
                    opts,
                    file_index,
                    ctx,
                } = pending;
                spawn_build_worker(
                    self,
                    ctx,
                    scene,
                    out,
                    source_dir,
                    opts,
                    Some(atlas),
                    file_index,
                );
            }
            Err(err) => {
                *self.build_stage.lock().unwrap() = String::new();
                if pending.file_index < self.files.len() {
                    self.files[pending.file_index].status =
                        format!("imposter bake failed: {err}");
                }
            }
        }
    }

    /// Drop the receiver for the in-flight build. The worker keeps running
    /// but its result is discarded — same pattern as `cancel_active_llm`.
    pub(super) fn cancel_build(&mut self) {
        // Clear a queued pre-bake too — otherwise a cancelled bundle-LODs
        // build would still spawn a worker once the bake lands next paint.
        let had_pending_bake = self.imposter_export_pending.take().is_some();
        if self.build_rx.is_none() && !had_pending_bake {
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

/// Launch the GLB build worker. Used both by `spawn_build` (no imposter
/// pre-bake required) and by `poll_imposter_export` (after the viewer has
/// finished baking the atlas). Owning this in one place keeps the worker
/// setup — channel + stage label + status string + repaint nudge —
/// identical across the two call paths.
fn spawn_build_worker(
    app: &mut MogenStudioApp,
    ctx: egui::Context,
    scene: SceneGraph,
    out: PathBuf,
    source_dir: Option<PathBuf>,
    opts: ExportOptions,
    prebaked_imposter: Option<ImposterAtlas>,
    file_index: usize,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    app.build_rx = Some(rx);
    *app.build_stage.lock().unwrap() = "starting".into();
    app.files[file_index].status = "building glb…".into();
    let stage = Arc::clone(&app.build_stage);
    std::thread::spawn(move || {
        let outcome = run_build(
            scene,
            out,
            source_dir,
            opts,
            prebaked_imposter,
            stage,
            file_index,
        );
        let _ = tx.send(outcome);
        ctx.request_repaint();
    });
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
