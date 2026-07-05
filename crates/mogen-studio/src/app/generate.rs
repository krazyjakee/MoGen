use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Instant;

use eframe::egui;

use crate::viewer::{CaptureFrame, CaptureKind, CaptureOutcome, CaptureRequest};

use super::MogenStudioApp;

/// Output resolution of generated thumbnails. Square so the file shows up
/// cleanly in OS thumbnail grids regardless of the model's aspect ratio.
const THUMBNAIL_SIZE: u32 = 512;
/// Number of frames captured for the rotating video. At [`VIDEO_FPS`] this
/// gives a 6-second loop, which is long enough to read the model from every
/// angle without dragging.
const VIDEO_FRAMES: u32 = 180;
/// Frame rate of the generated mp4 — fed both to the capture cadence and to
/// ffmpeg as `-framerate` / `-r`.
const VIDEO_FPS: u32 = 30;
/// Pitch (radians) used for both thumbnail and video so the model is read
/// from a slight 3/4 angle instead of dead-on. Matches `OrbitCamera::default`.
const CAPTURE_PITCH: f32 = 0.5;
/// Yaw (radians) of the static thumbnail. 45° gives the same 3/4 framing the
/// viewer's "Frame" button uses.
const THUMBNAIL_YAW: f32 = std::f32::consts::FRAC_PI_4;

/// One in-flight ffmpeg encode. `frames_dir` is owned so we can clean it up
/// after the worker completes whether the encode succeeded or failed.
pub(super) struct VideoEncode {
    pub(super) rx: Receiver<Result<PathBuf, String>>,
    pub(super) frames_dir: PathBuf,
    pub(super) output: PathBuf,
    pub(super) file_index: usize,
    pub(super) started_at: Instant,
}

/// Resolution the user picked in the video options modal. Square output, even
/// in both axes (libx264 + yuv420p requires it). 1080 keeps thin geometry
/// readable on a desktop monitor; 720 is the lighter option for previews.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoQuality {
    P720,
    P1080,
}

impl VideoQuality {
    pub(crate) fn size(self) -> u32 {
        match self {
            Self::P720 => 720,
            Self::P1080 => 1080,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::P720 => "720p",
            Self::P1080 => "1080p",
        }
    }
}

/// Camera behaviour for the captured video. Rotating sweeps yaw a full 2π
/// across the clip; Static holds yaw at the thumbnail framing while still
/// stepping through animation time so authored clips play.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoCameraMode {
    Rotating,
    Static,
}

impl VideoCameraMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Rotating => "Rotating",
            Self::Static => "Static",
        }
    }
}

/// Options chosen in the "Rotating video (MP4)" modal before render starts.
/// Defaults match the previous hard-coded behaviour (1080p, full rotation).
#[derive(Clone, Copy)]
pub(crate) struct VideoOptions {
    pub(crate) quality: VideoQuality,
    pub(crate) camera: VideoCameraMode,
}

impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            quality: VideoQuality::P1080,
            camera: VideoCameraMode::Rotating,
        }
    }
}

impl MogenStudioApp {
    /// Kick off a thumbnail render for the active file. Saves to
    /// `<basename>.thumb.png` next to the .mog (or under the project root for
    /// untitled buffers). Renders happen on the GL thread inside the next
    /// paint callback; this just queues the request.
    pub(super) fn generate_thumbnail(&mut self, ctx: &egui::Context) {
        if !self.ensure_ready_for_capture() {
            return;
        }
        let i = self.active;
        let path = match self.thumbnail_default_path(i) {
            Some(p) => p,
            None => {
                self.files[i].status =
                    "thumbnail: nothing to render — load or generate a scene first".into();
                return;
            }
        };
        let bg = self.settings.viewer_bg_rgb();
        self.viewer.submit_capture(CaptureRequest {
            kind: CaptureKind::Thumbnail,
            size: THUMBNAIL_SIZE,
            bg,
            frames: vec![CaptureFrame {
                yaw: THUMBNAIL_YAW,
                pitch: CAPTURE_PITCH,
                time: 0.0,
                path,
            }],
            // submit_capture overwrites these — placeholder values keep the
            // struct literal honest without leaking the bookkeeping detail
            // here.
            total: 0,
            written: Vec::new(),
            error: None,
        });
        self.files[i].status = "thumbnail: rendering…".into();
        ctx.request_repaint();
    }

    /// Kick off a video render for the active file with the user-picked
    /// options. Frames are written into a temp directory; once the GL worker
    /// reports back, we hand them to ffmpeg on a worker thread and write the
    /// final mp4 next to the .mog.
    pub(super) fn generate_video(&mut self, ctx: &egui::Context, opts: VideoOptions) {
        if !self.ensure_ready_for_capture() {
            return;
        }
        let i = self.active;
        let output = match self.video_default_path(i) {
            Some(p) => p,
            None => {
                self.files[i].status =
                    "video: nothing to render — load or generate a scene first".into();
                return;
            }
        };
        // ffmpeg on PATH is the gate: warn the user up-front rather than
        // letting them sit through a 6-second render and then fail.
        if !ffmpeg_available() {
            self.files[i].status =
                "video: ffmpeg not found in PATH — install ffmpeg to render mp4 rotations".into();
            return;
        }
        let frames_dir = match prepare_frames_dir() {
            Ok(d) => d,
            Err(e) => {
                self.files[i].status = format!("video: {e}");
                return;
            }
        };
        let bg = self.settings.viewer_bg_rgb();
        let size = opts.quality.size();
        let mut frames = Vec::with_capacity(VIDEO_FRAMES as usize);
        // Yaw sweeps a full 2π for the rotating mode — last frame is one step
        // shy of the first so the encoded loop seams cleanly on repeat. Static
        // mode pins yaw at the thumbnail framing.
        let two_pi = std::f32::consts::TAU;
        for f in 0..VIDEO_FRAMES {
            let yaw = match opts.camera {
                VideoCameraMode::Rotating => {
                    THUMBNAIL_YAW + (f as f32 / VIDEO_FRAMES as f32) * two_pi
                }
                VideoCameraMode::Static => THUMBNAIL_YAW,
            };
            let path = frames_dir.join(format!("frame_{:05}.png", f));
            // Sample the animation at the frame's wall-clock time so clips
            // play across the clip rather than being frozen at t=0.
            let time = f as f32 / VIDEO_FPS as f32;
            frames.push(CaptureFrame {
                yaw,
                pitch: CAPTURE_PITCH,
                time,
                path,
            });
        }
        // Stash the encode parameters on the app so `poll_generate` can pick
        // them up the moment the GL worker reports the frames are written.
        self.pending_video = Some(PendingVideo {
            frames_dir: frames_dir.clone(),
            output,
            file_index: i,
        });
        self.viewer.submit_capture(CaptureRequest {
            kind: CaptureKind::Video,
            size,
            bg,
            frames,
            total: 0,
            written: Vec::new(),
            error: None,
        });
        let mode = opts.camera.label().to_ascii_lowercase();
        self.files[i].status = format!(
            "video: rendering {VIDEO_FRAMES} {mode} frames at {size}px…"
        );
        ctx.request_repaint();
    }

    /// Drain any completed capture from the viewer and any completed ffmpeg
    /// encode. Called once per frame from `update`.
    ///
    /// Filters out `PickerThumb` and `Publish` outcomes — those belong to the
    /// file-picker's background thumbnail engine and the publish dialog
    /// respectively, both of which poll the viewer on their own tick. Without
    /// the filter, their captures would be drained here and lost before the
    /// owner could read them.
    pub(super) fn poll_generate(&mut self, ctx: &egui::Context) {
        if let Some(outcome) = self.viewer.take_capture_outcome_if(|kind| {
            !matches!(
                kind,
                CaptureKind::PickerThumb
                    | CaptureKind::Publish
                    | CaptureKind::WizardThumb
                    | CaptureKind::RemotePreview
            )
        }) {
            self.handle_capture_outcome(ctx, outcome);
        }
        if let Some(encode) = self.video_encode.as_ref() {
            match encode.rx.try_recv() {
                Ok(result) => {
                    let encode = self.video_encode.take().expect("just checked");
                    self.handle_video_encode_result(encode, result);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Nudge a repaint so the spinner-y status keeps the user
                    // oriented while ffmpeg runs.
                    ctx.request_repaint_after(std::time::Duration::from_millis(250));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let encode = self.video_encode.take().expect("just checked");
                    self.handle_video_encode_result(
                        encode,
                        Err("ffmpeg worker thread vanished without reporting".into()),
                    );
                }
            }
        }
    }

    /// Whether any kind of generate (thumbnail render, video frames, or
    /// ffmpeg encode) is currently in flight. Drives menu enable state so a
    /// second click can't pile a request onto a busy GL worker.
    pub(super) fn generate_in_flight(&self) -> bool {
        self.viewer.is_capturing() || self.video_encode.is_some()
    }

    fn handle_capture_outcome(&mut self, ctx: &egui::Context, outcome: CaptureOutcome) {
        match outcome.kind {
            CaptureKind::Thumbnail => {
                let i = self.active;
                if let Some(err) = outcome.error {
                    self.files[i].status = format!("thumbnail: {err}");
                } else if let Some(path) = outcome.frame_paths.last() {
                    self.files[i].status =
                        format!("thumbnail: wrote {}", path.display());
                } else {
                    self.files[i].status = "thumbnail: render produced no output".into();
                }
            }
            CaptureKind::PickerThumb
            | CaptureKind::Publish
            | CaptureKind::WizardThumb
            | CaptureKind::RemotePreview => {
                // PickerThumb is owned by `ThumbnailManager`; Publish by the
                // community publish dialog; WizardThumb by the Scene Wizard;
                // RemotePreview by the remote web UI's `poll_remote_preview`.
                // `poll_generate` filters all four out before reaching this
                // handler. Reaching here would only happen if the filter were
                // bypassed — drop on the floor rather than mis-attributing it
                // to the active file's status.
            }
            CaptureKind::ModifyScreenshot => {
                // Modify-with-screenshot captures are handled by the
                // driver in `app/llm/modify_screenshot.rs`. Route the
                // outcome straight through — including errors, so the
                // driver can clean up instead of leaking a stuck
                // `llm_in_flight = Modify` slot.
                self.on_modify_screenshot_render_done(ctx, outcome);
            }
            CaptureKind::Video => {
                let pending = match self.pending_video.take() {
                    Some(p) => p,
                    None => return,
                };
                if let Some(err) = outcome.error {
                    let _ = std::fs::remove_dir_all(&pending.frames_dir);
                    self.files[pending.file_index].status =
                        format!("video: render failed — {err}");
                    return;
                }
                if outcome.frame_paths.len() != VIDEO_FRAMES as usize {
                    let _ = std::fs::remove_dir_all(&pending.frames_dir);
                    self.files[pending.file_index].status = format!(
                        "video: expected {} frames, got {}",
                        VIDEO_FRAMES,
                        outcome.frame_paths.len()
                    );
                    return;
                }
                self.spawn_ffmpeg(ctx, pending);
            }
        }
    }

    fn spawn_ffmpeg(&mut self, ctx: &egui::Context, pending: PendingVideo) {
        let (tx, rx) = std::sync::mpsc::channel();
        let frames_dir = pending.frames_dir.clone();
        let output = pending.output.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let result = run_ffmpeg(&frames_dir, &output);
            let _ = tx.send(result);
            ctx_clone.request_repaint();
        });
        self.files[pending.file_index].status =
            format!("video: encoding {} via ffmpeg…", pending.output.display());
        self.video_encode = Some(VideoEncode {
            rx,
            frames_dir: pending.frames_dir,
            output: pending.output,
            file_index: pending.file_index,
            started_at: Instant::now(),
        });
    }

    fn handle_video_encode_result(
        &mut self,
        encode: VideoEncode,
        result: Result<PathBuf, String>,
    ) {
        // Always clean up the frames dir; the user only ever cares about the
        // final mp4, and leaving 180 PNGs in /tmp is just noise.
        let _ = std::fs::remove_dir_all(&encode.frames_dir);
        if encode.file_index >= self.files.len() {
            // Tab was closed mid-encode — silently drop.
            return;
        }
        let elapsed = encode.started_at.elapsed();
        match result {
            Ok(path) => {
                self.files[encode.file_index].status = format!(
                    "video: wrote {} ({:.1}s)",
                    path.display(),
                    elapsed.as_secs_f32()
                );
            }
            Err(err) => {
                self.files[encode.file_index].status = format!("video: ffmpeg failed — {err}");
                let _ = encode.output;
            }
        }
    }

    fn ensure_ready_for_capture(&mut self) -> bool {
        if self.generate_in_flight() {
            let i = self.active;
            self.files[i].status =
                "generate: another render is already in flight, finish it first".into();
            return false;
        }
        true
    }

    fn thumbnail_default_path(&self, i: usize) -> Option<PathBuf> {
        let base = self.capture_basename(i)?;
        Some(base.with_extension("thumb.png"))
    }

    fn video_default_path(&self, i: usize) -> Option<PathBuf> {
        let base = self.capture_basename(i)?;
        Some(base.with_extension("mp4"))
    }

    /// Path stem next to which capture outputs land. For titled tabs we use
    /// the .mog path itself; for untitled buffers we fall back to the
    /// project root + the tab's display name so the file at least lands
    /// somewhere predictable.
    fn capture_basename(&self, i: usize) -> Option<PathBuf> {
        let f = self.files.get(i)?;
        if let Some(p) = &f.path {
            return Some(p.clone());
        }
        // Untitled buffers land in the project root with a placeholder name.
        Some(self.project_root.join("untitled"))
    }
}

/// Extra state on the app, populated when the user kicks off a video render
/// and consumed once the GL worker reports the frames are on disk.
pub(super) struct PendingVideo {
    pub(super) frames_dir: PathBuf,
    pub(super) output: PathBuf,
    pub(super) file_index: usize,
}

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn prepare_frames_dir() -> Result<PathBuf, String> {
    // Drop the frames in a per-process temp folder so concurrent Studio
    // instances don't trample each other and a crash leaves something
    // recoverable. Suffix with the pid so re-launching reliably yields a
    // fresh directory even if a previous run died with the old one in place.
    let dir = std::env::temp_dir().join(format!("mogen-studio-frames-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("clean stale frames dir {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create frames dir {}: {e}", dir.display()))?;
    Ok(dir)
}

fn run_ffmpeg(frames_dir: &Path, output: &Path) -> Result<PathBuf, String> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create_dir_all {}: {e}", parent.display()))?;
    }
    let pattern = frames_dir.join("frame_%05d.png");
    let status = std::process::Command::new("ffmpeg")
        .arg("-y")
        .arg("-framerate")
        .arg(VIDEO_FPS.to_string())
        .arg("-i")
        .arg(&pattern)
        .arg("-c:v")
        .arg("libx264")
        // `slow` + CRF 18 is the libx264 sweet spot for synthetic 3D output:
        // near-visually-lossless on flat-shaded geometry without ballooning
        // file size. Defaults (preset=medium, crf=23) leave visible mosquito
        // noise around CSG edges and texture seams on rotation playback.
        .arg("-preset")
        .arg("slow")
        .arg("-crf")
        .arg("18")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-r")
        .arg(VIDEO_FPS.to_string())
        .arg(output)
        // Hide ffmpeg's verbose progress output so a failed run reports a
        // clean exit code instead of a wall of stderr in the user's status
        // line. If users need details they can re-run from a terminal.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;
    if !status.success() {
        return Err(format!(
            "ffmpeg exited with status {}",
            status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into())
        ));
    }
    Ok(output.to_path_buf())
}
