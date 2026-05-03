//! Background thumbnail engine for the file picker.
//!
//! The picker shows rendered 192×192 PNG previews of every `.mog` file in
//! the current directory. Producing one of those previews has three stages,
//! and each one runs on the cheapest thread that can do it:
//!
//! 1. **Compile** (background): a worker thread reads the `.mog` off disk
//!    and runs the normal `pipeline::compile`. CPU-bound, doesn't touch
//!    GL, so we keep it off the UI thread.
//! 2. **Render** (UI thread): the live `Viewer`'s capture pipeline takes
//!    one scene at a time, swaps it in, fires an offscreen render to a
//!    PNG on disk. Has to be on the UI thread because the GL context lives
//!    there. Other UI work (the picker itself) is allowed to paint while
//!    this is in flight — the capture system already cooperates with
//!    egui's frame loop.
//! 3. **Decode + upload** (background → UI): a second worker thread loads
//!    the PNG bytes and decodes to RGBA. Texture upload happens back on
//!    the UI thread because `egui::Context::load_texture` requires it.
//!
//! Cache key is `(absolute path, mtime)`. PNGs land under
//! `~/.cache/mogen/thumbs/` (or `$MOGEN_CACHE_DIR/thumbs/`) so they survive
//! across sessions; in-memory we hold the egui texture handle once it's
//! uploaded so the picker repaints aren't re-decoding the file every frame.
//!
//! Only one render is ever in flight (the live viewer can only render one
//! scene at a time). Compiles can pile up freely — the worker just chews
//! through the queue.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui;
use mogen_core::SceneGraph;

use crate::pipeline::{self, Stage};
use crate::viewer::{CaptureFrame, CaptureKind, CaptureRequest, Viewer};

/// Edge length (px) of the rendered thumbnail. Small enough to fit a few
/// per row in the picker grid, big enough to read at a glance.
pub(super) const THUMB_SIZE: u32 = 192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ThumbStatus {
    /// Submitted to the compile worker but no result yet.
    Compiling,
    /// Compiled, queued for the GL render pass.
    Rendering,
    /// PNG produced, decoding bytes into a texture.
    Loading,
    /// Texture uploaded; `handle` is set.
    Ready,
    /// Compile, render, or decode failed. Cell falls back to a placeholder.
    Failed,
}

pub(super) struct ThumbEntry {
    pub(super) status: ThumbStatus,
    pub(super) handle: Option<egui::TextureHandle>,
    /// File mtime captured when the entry was created. Re-stat'd by
    /// [`ThumbnailManager::request`] to detect on-disk changes.
    pub(super) mtime: Option<SystemTime>,
}

struct CompileJob {
    path: PathBuf,
}

struct CompileResultMsg {
    path: PathBuf,
    mtime: Option<SystemTime>,
    /// Pre-rendered PNG already on disk (cache hit). When `Some`, skip the
    /// GL render entirely and decode straight from disk.
    cached_png: Option<PathBuf>,
    /// Compiled scene to render. `None` when the file failed to compile or
    /// the cached PNG carried the result.
    scene: Option<Arc<SceneGraph>>,
    /// Source dir for texture path resolution. Mirrors what the editor
    /// passes to the live viewer so material textures load against the
    /// `.mog`'s own folder.
    source_dir: Option<PathBuf>,
    error: Option<String>,
}

struct DecodeJob {
    /// Picker entry path used as the cache key. NOT the on-disk PNG path.
    path: PathBuf,
    mtime: SystemTime,
    png_path: PathBuf,
}

struct DecodeResult {
    path: PathBuf,
    mtime: SystemTime,
    image: Option<egui::ColorImage>,
}

pub(super) struct ThumbnailManager {
    cache_dir: PathBuf,
    entries: HashMap<PathBuf, ThumbEntry>,
    compile_tx: Sender<CompileJob>,
    compile_rx: Receiver<CompileResultMsg>,
    decode_tx: Sender<DecodeJob>,
    decode_rx: Receiver<DecodeResult>,
    /// Scenes ready for the live viewer to render. Drained one per frame.
    render_queue: VecDeque<RenderJob>,
    /// `(path, mtime, png_path)` for the render currently going through the
    /// viewer's capture system. `None` when the renderer is idle.
    in_flight_render: Option<RenderJob>,
}

#[derive(Clone)]
struct RenderJob {
    path: PathBuf,
    mtime: SystemTime,
    scene: Arc<SceneGraph>,
    source_dir: Option<PathBuf>,
    png_path: PathBuf,
}

impl ThumbnailManager {
    /// Build the manager and spawn the background workers. `ctx` is cloned
    /// into each worker so they can call `request_repaint()` when a result
    /// lands — without that wakeup the UI thread can sit idle between
    /// renders (when `is_busy()` is false but a compile is still in flight)
    /// and the picker grid stalls until the next user input.
    ///
    /// Compile and decode each get a small thread pool. Compile is the
    /// expensive step (parser + lower + validate per file); a directory of
    /// 50 `.mog` files compiled serially takes long enough to be visible
    /// before the first thumbnail can even start rendering. Multiple
    /// workers let the GL render pipeline start chewing on results while
    /// the compile pool is still busy on later files.
    pub(super) fn new(ctx: egui::Context) -> Self {
        let cache_dir = thumb_cache_dir();
        let _ = fs::create_dir_all(&cache_dir);
        let worker_count = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 6);

        let (compile_tx, compile_in) = channel::<CompileJob>();
        let compile_in = Arc::new(Mutex::new(compile_in));
        let (compile_out, compile_rx) = channel::<CompileResultMsg>();
        for i in 0..worker_count {
            let compile_in = compile_in.clone();
            let compile_out = compile_out.clone();
            let cache_dir = cache_dir.clone();
            let ctx = ctx.clone();
            thread::Builder::new()
                .name(format!("mogen-thumb-compile-{i}"))
                .spawn(move || compile_worker(compile_in, compile_out, cache_dir, ctx))
                .expect("spawn thumb compile worker");
        }

        let (decode_tx, decode_in) = channel::<DecodeJob>();
        let decode_in = Arc::new(Mutex::new(decode_in));
        let (decode_out, decode_rx) = channel::<DecodeResult>();
        for i in 0..worker_count {
            let decode_in = decode_in.clone();
            let decode_out = decode_out.clone();
            let ctx = ctx.clone();
            thread::Builder::new()
                .name(format!("mogen-thumb-decode-{i}"))
                .spawn(move || decode_worker(decode_in, decode_out, ctx))
                .expect("spawn thumb decode worker");
        }
        Self {
            cache_dir,
            entries: HashMap::new(),
            compile_tx,
            compile_rx,
            decode_tx,
            decode_rx,
            render_queue: VecDeque::new(),
            in_flight_render: None,
        }
    }

    /// Read API for the picker grid. Returns the cached texture handle if
    /// rendering finished; `None` while the job is still in flight or has
    /// failed.
    pub(super) fn texture(&self, path: &Path) -> Option<egui::TextureHandle> {
        self.entries.get(path).and_then(|e| e.handle.clone())
    }

    pub(super) fn status(&self, path: &Path) -> Option<ThumbStatus> {
        self.entries.get(path).map(|e| e.status)
    }

    /// Enqueue a thumbnail render for `path` if we don't already have one
    /// at the file's current mtime. Idempotent — calling on every frame for
    /// every visible cell is fine.
    pub(super) fn request(&mut self, path: &Path) {
        let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some(entry) = self.entries.get(path) {
            if entry.mtime == mtime {
                return;
            }
        }
        self.entries.insert(
            path.to_path_buf(),
            ThumbEntry {
                status: ThumbStatus::Compiling,
                handle: None,
                mtime,
            },
        );
        let _ = self.compile_tx.send(CompileJob {
            path: path.to_path_buf(),
        });
    }

    /// True while a render is going through the live viewer or queued
    /// behind one. Drives the picker's "scrim the viewport" decision —
    /// while the engine is busy, the live viewport's scene is being
    /// swapped under our feet.
    pub(super) fn is_busy(&self) -> bool {
        self.in_flight_render.is_some() || !self.render_queue.is_empty()
    }

    /// Run one tick of the pipeline: drain compile / decode results,
    /// promote ready scenes onto the render queue, and submit the next
    /// render to the live viewer when one isn't already in flight. Called
    /// from `MogenStudioApp::update` while the picker is open.
    pub(super) fn tick(&mut self, viewer: &Viewer, ctx: &egui::Context) {
        // 1. Drain compile results — promote to either decode (cache hit)
        //    or render (cache miss).
        while let Ok(msg) = self.compile_rx.try_recv() {
            let entry = self
                .entries
                .entry(msg.path.clone())
                .or_insert_with(|| ThumbEntry {
                    status: ThumbStatus::Compiling,
                    handle: None,
                    mtime: msg.mtime,
                });
            entry.mtime = msg.mtime;
            if msg.error.is_some() {
                entry.status = ThumbStatus::Failed;
                continue;
            }
            let Some(mtime) = msg.mtime else {
                entry.status = ThumbStatus::Failed;
                continue;
            };
            if let Some(png) = msg.cached_png {
                entry.status = ThumbStatus::Loading;
                let _ = self.decode_tx.send(DecodeJob {
                    path: msg.path,
                    mtime,
                    png_path: png,
                });
                continue;
            }
            if let Some(scene) = msg.scene {
                entry.status = ThumbStatus::Rendering;
                self.render_queue.push_back(RenderJob {
                    path: msg.path.clone(),
                    mtime,
                    scene,
                    source_dir: msg.source_dir,
                    png_path: self.cache_path_for(&msg.path, mtime),
                });
            } else {
                entry.status = ThumbStatus::Failed;
            }
        }

        // 2. Drain decode results — upload the egui texture and mark Ready.
        while let Ok(decoded) = self.decode_rx.try_recv() {
            let Some(entry) = self.entries.get_mut(&decoded.path) else {
                continue;
            };
            // mtime gate: a faster decode might land for an old version
            // that was superseded by a `request` for a newer mtime.
            if entry.mtime != Some(decoded.mtime) {
                continue;
            }
            if let Some(img) = decoded.image {
                let handle = ctx.load_texture(
                    format!("mogen-thumb:{}", decoded.path.display()),
                    img,
                    egui::TextureOptions::LINEAR,
                );
                entry.handle = Some(handle);
                entry.status = ThumbStatus::Ready;
            } else {
                entry.status = ThumbStatus::Failed;
            }
        }

        // 3. Pick up the in-flight render if it finished.
        //    Filter on `PickerThumb` so a stray user-driven Thumbnail or
        //    Video outcome (left over from before the picker opened) isn't
        //    mis-attributed to our queue. Symmetrical with the inverse
        //    filter in `poll_generate`.
        if let Some(job) = self.in_flight_render.clone() {
            if let Some(outcome) =
                viewer.take_capture_outcome_if(|kind| matches!(kind, CaptureKind::PickerThumb))
            {
                self.in_flight_render = None;
                if outcome.error.is_none() && !outcome.frame_paths.is_empty() {
                    if let Some(entry) = self.entries.get_mut(&job.path) {
                        entry.status = ThumbStatus::Loading;
                    }
                    let _ = self.decode_tx.send(DecodeJob {
                        path: job.path,
                        mtime: job.mtime,
                        png_path: job.png_path,
                    });
                } else if let Some(entry) = self.entries.get_mut(&job.path) {
                    entry.status = ThumbStatus::Failed;
                }
            }
        }

        // 4. Submit the next render if the viewer is idle.
        if self.in_flight_render.is_none() {
            if let Some(job) = self.render_queue.pop_front() {
                viewer.set_scene(job.scene.clone(), job.source_dir.as_deref(), true);
                let request = CaptureRequest {
                    kind: CaptureKind::PickerThumb,
                    size: THUMB_SIZE,
                    bg: crate::settings::DEFAULT_VIEWER_BG_RGB,
                    // Single frame at a 3/4 angle — close to what the user
                    // would see if they pressed Frame on a freshly loaded
                    // scene. `time = 0` so the capture renders the rest
                    // pose for animated scenes (deterministic, doesn't
                    // depend on how long the picker has been open).
                    frames: vec![CaptureFrame {
                        yaw: 0.55,
                        pitch: 0.45,
                        time: 0.0,
                        path: job.png_path.clone(),
                    }],
                    total: 0,
                    written: Vec::new(),
                    error: None,
                };
                viewer.submit_capture(request);
                self.in_flight_render = Some(job);
            }
        }
    }

    /// Cache filename for a `(path, mtime)` pair. Stable across runs so
    /// previous-session thumbnails are reused.
    fn cache_path_for(&self, path: &Path, mtime: SystemTime) -> PathBuf {
        cache_path(&self.cache_dir, path, mtime)
    }
}

fn compile_worker(
    in_rx: Arc<Mutex<Receiver<CompileJob>>>,
    out_tx: Sender<CompileResultMsg>,
    cache_dir: PathBuf,
    ctx: egui::Context,
) {
    loop {
        // Hold the recv lock only long enough to grab one job; the actual
        // compile must run unlocked so siblings can pull their own jobs in
        // parallel. Mirrors the `EncodePool` worker pattern.
        let job = {
            let rx = in_rx.lock().unwrap();
            match rx.recv() {
                Ok(j) => j,
                Err(_) => return,
            }
        };
        let result = compile_one(&job.path, &cache_dir);
        if out_tx.send(result).is_err() {
            // Manager dropped — exit quietly.
            return;
        }
        // Wake the UI thread so `tick` runs and drains the result. Without
        // this, the loop stalls between renders whenever the queue is empty
        // and `in_flight_render` is None — `is_busy()` returns false, the
        // app stops requesting repaints, and freshly-compiled results sit
        // in the channel until unrelated input fires the next frame.
        ctx.request_repaint();
    }
}

fn compile_one(path: &Path, cache_dir: &Path) -> CompileResultMsg {
    let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
    if let Some(t) = mtime {
        let cache_png = cache_path(cache_dir, path, t);
        if cache_png.exists() {
            return CompileResultMsg {
                path: path.to_path_buf(),
                mtime,
                cached_png: Some(cache_png),
                scene: None,
                source_dir: None,
                error: None,
            };
        }
    }
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return CompileResultMsg {
                path: path.to_path_buf(),
                mtime,
                cached_png: None,
                scene: None,
                source_dir: None,
                error: Some(format!("read: {e}")),
            };
        }
    };
    let result = pipeline::compile(&src, path.parent());
    let scene = match result.stage {
        Stage::Ok => result.scene,
        _ => None,
    };
    let error = match result.stage {
        Stage::Ok => None,
        s => Some(format!("compile failed at {:?}", s)),
    };
    CompileResultMsg {
        path: path.to_path_buf(),
        mtime,
        cached_png: None,
        scene,
        source_dir: path.parent().map(|p| p.to_path_buf()),
        error,
    }
}

fn decode_worker(
    in_rx: Arc<Mutex<Receiver<DecodeJob>>>,
    out_tx: Sender<DecodeResult>,
    ctx: egui::Context,
) {
    loop {
        let job = {
            let rx = in_rx.lock().unwrap();
            match rx.recv() {
                Ok(j) => j,
                Err(_) => return,
            }
        };
        let result = decode_one(job);
        if out_tx.send(result).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

fn decode_one(job: DecodeJob) -> DecodeResult {
    let bytes = match fs::read(&job.png_path) {
        Ok(b) => b,
        Err(_) => {
            return DecodeResult {
                path: job.path,
                mtime: job.mtime,
                image: None,
            };
        }
    };
    let img = match image::load_from_memory(&bytes) {
        Ok(i) => i,
        Err(_) => {
            return DecodeResult {
                path: job.path,
                mtime: job.mtime,
                image: None,
            };
        }
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    DecodeResult {
        path: job.path,
        mtime: job.mtime,
        image: Some(egui::ColorImage::from_rgba_unmultiplied(size, &pixels)),
    }
}

/// Resolve the on-disk cache directory. Honours `MOGEN_CACHE_DIR` (the
/// project-wide override defined in CLAUDE.md) before falling back to the
/// platform's user cache dir, then to `/tmp/` as the final safety net.
fn thumb_cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("MOGEN_CACHE_DIR") {
        return PathBuf::from(p).join("thumbs");
    }
    if let Some(d) = dirs::cache_dir() {
        return d.join("mogen").join("thumbs");
    }
    std::env::temp_dir().join("mogen-thumbs")
}

fn cache_path(cache_dir: &Path, path: &Path, mtime: SystemTime) -> PathBuf {
    let secs = mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = fnv1a_64(path.display().to_string().as_bytes());
    cache_dir.join(format!("{h:016x}_{secs}.png"))
}

/// Stable 64-bit hash for cache filenames. Rust's `DefaultHasher` doesn't
/// promise stability across compiler versions, so we hand-roll FNV-1a here
/// — cache files written today need to remain locatable after a toolchain
/// upgrade.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}
