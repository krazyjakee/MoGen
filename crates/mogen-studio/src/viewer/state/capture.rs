//! Offscreen frame capture: types the viewer hands to its GL paint
//! callback for thumbnails / videos plus the PNG-encoding worker pool
//! that pumps results back to disk without stalling the render thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use mogen_core::SceneGraph;
use mogen_render::imposter::ImposterAtlas;

/// What kind of capture the user requested. Carried alongside the per-frame
/// rendering instructions so the app can route the result to the right
/// completion handler (write a thumbnail PNG, kick off ffmpeg).
///
/// `PickerThumb` is a separate variant from `Thumbnail` so the file-picker's
/// background thumbnail engine can pump captures through the viewer without
/// `poll_generate` stealing the outcome and treating it as the user-driven
/// "Generate Thumbnail" menu action. `Publish` does the same for the publish
/// dialog's preview capture. `ModifyScreenshot` does the same for the LLM
/// modify-with-screenshot path: the renderer treats it identically to a
/// thumbnail (single static frame, no animation override), the variant only
/// tells `app/llm/modify_screenshot.rs::on_modify_screenshot_render_done`
/// "this PNG is mine". All four single-frame kinds behave identically inside
/// the GL paint callback (no animation override, single frame); the variant
/// only carries "who owns this outcome".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureKind {
    Thumbnail,
    Video,
    PickerThumb,
    Publish,
    ModifyScreenshot,
    /// Single-frame isometric screenshot owned by the Scene Wizard. The
    /// renderer treats it identically to a `Thumbnail`; the variant only
    /// tells `poll_generate` to route the outcome back into the wizard
    /// instead of the active tab's status line.
    WizardThumb,
    /// Single-frame render owned by the remote-control web UI's live
    /// preview. Same GL path as a `Thumbnail`; the variant keeps
    /// `poll_generate` from draining outcomes that belong to
    /// `poll_remote_preview`.
    RemotePreview,
}

/// One frame the renderer should produce as part of a capture. Yaw/pitch
/// override the user's live camera so video frames orbit cleanly around the
/// scene regardless of how the user has the viewport framed. `time` is the
/// timestamp (seconds) at which active clips should be sampled for this
/// frame, so the rendered video plays the animation across the rotation.
/// Thumbnails pass `0.0`; the capture path leaves clip state untouched in
/// that case.
#[derive(Clone, Debug)]
pub struct CaptureFrame {
    pub yaw: f32,
    pub pitch: f32,
    pub time: f32,
    pub path: PathBuf,
}

/// A queued capture. Posted from the app via [`super::super::Viewer::submit_capture`]
/// and consumed across multiple paint callbacks — each paint pops one frame
/// off `frames`, renders it, and pushes the result to `written`. When
/// `frames` is drained (or `error` is set), the paint callback finalises
/// the request as a [`CaptureOutcome`]. Spreading the work across paints is
/// what keeps the UI responsive and the progress modal animating during a
/// 180-frame video render — packing all frames into one paint freezes the
/// whole window for several seconds.
#[derive(Clone, Debug)]
pub struct CaptureRequest {
    pub kind: CaptureKind,
    pub size: u32,
    pub bg: [u8; 3],
    /// Frames still to render. Drained from the front, one per paint.
    pub frames: Vec<CaptureFrame>,
    /// Initial frame count, used as the progress denominator. Stays fixed
    /// while `frames` shrinks.
    pub total: u32,
    /// Paths the GL worker has already written PNGs to. Becomes the
    /// `frame_paths` of the eventual [`CaptureOutcome`].
    pub written: Vec<PathBuf>,
    /// First fatal error, if any. Set causes the paint loop to short-
    /// circuit and finalise the outcome on the next tick.
    pub error: Option<String>,
}

/// Result the paint callback writes back into [`super::ViewerState::capture_outcome`]
/// after processing a `CaptureRequest`. The app polls for it on the next frame.
#[derive(Clone, Debug)]
pub struct CaptureOutcome {
    pub kind: CaptureKind,
    pub frame_paths: Vec<PathBuf>,
    pub error: Option<String>,
}

/// Pending imposter atlas bake. The paint callback consumes this on its
/// next firing, runs `mogen_render::imposter::bake_yaw_atlas_on_gl` against
/// the live `glow::Context`, and writes the result into
/// [`super::ViewerState::imposter_outcome`].
///
/// Separate from `CaptureRequest` because the bake renders an *arbitrary*
/// `SceneGraph` (the active scene, or the post-merge scene the export
/// pipeline computed) rather than the live viewer mesh, and the result is
/// kept in memory rather than going through the on-disk PNG encoder.
pub struct ImposterRequest {
    pub scene: Arc<SceneGraph>,
    pub cell_size: u32,
    pub view_count: u32,
    pub pitch: f32,
    /// Directory of the source `.mog` file. Passed through to
    /// `flatten` so relative material `*_texture` paths resolve
    /// correctly — without it the bake falls back to PBR scalars and
    /// every cell renders flat-coloured silhouettes.
    pub base_dir: Option<std::path::PathBuf>,
}

/// Result of an imposter bake. `Ok` carries the atlas plus its yaw / cell
/// dimensions; `Err` is a flattened error message so the receiver can
/// surface it in a label without re-wrapping.
pub type ImposterOutcome = Result<ImposterAtlas, String>;

/// Cached GPU state for the viewport imposter preview mode. Built on the
/// fly in the paint callback after a fresh bake, freed when the mode is
/// left or the atlas is replaced. The CPU-side scene extent is captured
/// here so the billboard can size itself without re-walking the scene
/// every frame.
pub struct ImposterViewOverlay {
    /// GL texture for the baked atlas. Owned by the overlay — must be
    /// freed via `Renderer::destroy_imposter_texture` when replaced.
    pub texture: glow::Texture,
    /// Atlas cell count along the horizontal axis. Drives the shader's
    /// yaw → cell snap.
    pub view_count: u32,
    /// World-space centre of the source scene's AABB — the billboard
    /// stands here. Matches the bake's framing target so the silhouette
    /// in every cell sits at the right place on the quad.
    pub center: [f32; 3],
    /// Horizontal half-extent of the billboard (worst-yaw silhouette
    /// radius). Used as the half-width of the camera-facing quad.
    pub half_width: f32,
    /// Vertical half-extent of the billboard — half the model's actual
    /// AABB height, so the quad occupies the same volume as the
    /// original mesh and the imposter doesn't float above where the
    /// model lives.
    pub half_height: f32,
    /// V-coordinate range inside one cell that contains the silhouette.
    /// The shader maps the quad's V from `[0, 1]` onto
    /// `[uv_y_top, uv_y_bottom]` so the cell's transparent margins
    /// crop out and the silhouette stretches across the full quad.
    pub uv_y_top: f32,
    pub uv_y_bottom: f32,
}

/// One PNG-encode-and-write job pushed onto [`EncodePool`]. Owns its pixel
/// buffer so the GL thread can drop the FBO read-back and immediately render
/// the next frame instead of blocking on deflate.
struct EncodeJob {
    pixels: Vec<u8>,
    size: u32,
    path: PathBuf,
}

/// Result of one [`EncodeJob`]. Workers always send the path back so the
/// completion handler can record either a written-path or a per-file error
/// without holding extra state on the worker side.
type EncodeResult = (PathBuf, Result<(), String>);

/// Bounded pool of background threads that PNG-encode captured frames so
/// `process_capture_step` doesn't spend its paint-callback budget on
/// deflate. The GL thread submits raw RGBA buffers; workers pull from a
/// shared queue, write the PNG, and report back on `result_rx`.
pub struct EncodePool {
    /// Queue feeding the workers. `Some` while we may still submit new jobs;
    /// taken at finalisation so workers see the channel close and exit.
    job_tx: Option<Sender<EncodeJob>>,
    /// Stream of completed (or failed) encodes drained on each paint by the
    /// completion-tracking phase of `process_capture_step`.
    pub result_rx: Receiver<EncodeResult>,
    /// Outstanding encodes the pool has accepted but not yet reported back
    /// on. Finalisation waits for this to reach zero so the outcome's
    /// `frame_paths` reflect every successful PNG actually on disk.
    pub in_flight: usize,
    /// Worker join handles. Held so `Drop` can wait them out cleanly once
    /// the job channel is closed; otherwise a stray worker could outlive
    /// the pool and try to send on a dropped result channel (harmless, but
    /// noisy in tests and on shutdown).
    workers: Vec<JoinHandle<()>>,
}

impl EncodePool {
    /// Spin up workers. Cap at six because PNG encoding is bottlenecked on
    /// deflate (single-threaded per file) and we want to leave the GL +
    /// main threads room to keep the UI responsive — at six workers the
    /// pool already keeps up with GL render rate at 720p.
    pub fn new() -> Self {
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 6);
        let (job_tx, job_rx) = mpsc::channel::<EncodeJob>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (result_tx, result_rx) = mpsc::channel::<EncodeResult>();
        let mut workers = Vec::with_capacity(n);
        for _ in 0..n {
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            workers.push(std::thread::spawn(move || loop {
                // Hold the recv lock only long enough to grab a job; the
                // encode itself must run unlocked so siblings can pull
                // their own jobs in parallel.
                let job = {
                    let rx = job_rx.lock().unwrap();
                    match rx.recv() {
                        Ok(j) => j,
                        Err(_) => break,
                    }
                };
                let res = encode_png(&job.pixels, job.size, &job.path);
                // Receiver may already be gone if the pool was dropped
                // mid-encode (capture cancelled, app shutting down). Drop
                // the result on the floor — there's nothing to report to.
                let _ = result_tx.send((job.path, res));
            }));
        }
        Self {
            job_tx: Some(job_tx),
            result_rx,
            in_flight: 0,
            workers,
        }
    }

    /// Hand a rendered frame to the pool. Increments the in-flight counter
    /// so the capture loop knows to wait for one more result before it can
    /// finalise the outcome.
    pub fn submit(&mut self, pixels: Vec<u8>, size: u32, path: PathBuf) {
        if let Some(tx) = self.job_tx.as_ref() {
            // Send only fails if every worker has panicked, which we treat
            // as a fatal error path — the in-flight counter would then
            // never decrement, but the capture's outer error handling will
            // catch that on the next paint when the worker count drops to
            // zero.
            let _ = tx.send(EncodeJob {
                pixels,
                size,
                path,
            });
            self.in_flight += 1;
        }
    }
}

impl Drop for EncodePool {
    fn drop(&mut self) {
        // Closing `job_tx` makes each worker's `recv` return `Err` and exit.
        // Joining afterwards is best-effort: workers should be near-idle by
        // the time we're dropping (capture loop only drops the pool once
        // `in_flight == 0`), but we wait anyway so a stray scheduling delay
        // can't keep a worker alive past the egui shutdown tear-down.
        self.job_tx.take();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

fn encode_png(pixels: &[u8], size: u32, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create_dir_all {}: {e}", parent.display()))?;
    }
    image::save_buffer(path, pixels, size, size, image::ColorType::Rgba8)
        .map_err(|e| format!("write {}: {e}", path.display()))
}
