use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use eframe::egui;
use glam::{Mat4, Quat, Vec3};
use mogen_core::{NodeId, SceneGraph, Transform};

use super::anim::{apply_animation, world_transforms_from_locals};
use super::camera::OrbitCamera;
use super::cinema::CinemaDirector;
use super::environment::Environment;
use super::flatten::{flatten, update_palettes, FlatMesh};
use super::lights::{collect_lights, ResolvedLight};
use super::shadows::ShadowQuality;
use crate::preview_shader::PreviewShader;

/// Stable, recompile-survivable handle for a scene node. Each entry is a
/// `(name, sibling_disambiguator)` pair: walk root→leaf, picking the
/// `disambiguator`-th sibling that matches `name` under the running parent.
/// The disambiguator counts only siblings with the same name — unique names
/// always carry `0`, so adding/removing differently-named siblings doesn't
/// invalidate unrelated paths. Without it, `array(...)`/`mirror`/etc.
/// replicas (which all share a name) would all collapse onto the first
/// match after a recompile, so a gizmo drag on the second copy would
/// re-select the first one when the rebuild lands.
pub type SelectionPath = Vec<(String, u32)>;

/// Shared state between the egui main thread and the render-time paint callback.
pub struct ViewerState {
    pub camera: OrbitCamera,
    pub mesh: FlatMesh,
    /// Set when vertex/index data must be re-uploaded to the GPU (scene
    /// recompiles, scene clears). Animation ticks and gizmo drags do NOT set
    /// this — they only refresh `mesh.palettes` and set `palettes_dirty`.
    pub mesh_dirty: bool,
    /// Set when only the per-batch matrix palettes have changed (animation
    /// tick, gizmo drag, clip toggle, time scrub). The paint callback uploads
    /// just the palette uniforms when this is set, leaving the VBO/EBO alone
    /// — the whole point of the rest-pose-baked vertex stream.
    pub palettes_dirty: bool,
    pub scene: Option<Arc<SceneGraph>>,
    /// Directory of the source `.mog` file — used to resolve relative texture
    /// paths declared on materials. `None` for unsaved buffers.
    pub base_dir: Option<PathBuf>,
    /// Parallel to `scene.clips`. A `true` entry means that clip contributes
    /// to the pose this frame.
    pub clip_active: Vec<bool>,
    /// Parallel to `scene.clips`. Each clip advances its own timer, wrapped
    /// to its own duration, so clips with different durations stay in phase
    /// with themselves (not with each other).
    pub anim_times: Vec<f32>,
    pub anim_playing: bool,
    /// Multiplier on per-frame `dt` when advancing clip timers. 1.0 = real
    /// time, 0.0 freezes playback (different from pausing — clips remain
    /// active and gizmo edits still rebuild the posed mesh), negative values
    /// rewind. Default 1.0.
    pub playback_speed: f32,
    /// Static-pose bounding sphere captured at `set_scene` time. The Frame
    /// button refits the camera against this so the view doesn't bounce with
    /// active animations.
    pub static_center: Vec3,
    pub static_radius: f32,
    /// Currently-selected scene nodes. Empty vec = no selection. The **last
    /// entry is the primary** node (the one the gizmo and inspector follow);
    /// earlier entries are secondary selections added via shift/cmd-click.
    /// Transient UI state — wiped when a compile fails, re-resolved by path
    /// across successful recompiles.
    pub selected: Vec<NodeId>,
    /// Stable name-paths parallel to `selected` (same length, same order).
    /// Used to re-resolve `NodeId`s after a recompile renumbers scene nodes.
    /// Kept separate from `selected` so `set_scene` can refresh ids without
    /// app code having to track both.
    pub selected_paths: Vec<SelectionPath>,
    /// Viewport gizmo mode. Toggled by toolbar buttons; the drag handler
    /// looks at this to pick which axis-hit/drag-math function to call.
    pub gizmo_mode: crate::gizmo::GizmoMode,
    /// Pending live-preview transform applied on top of the selected node's
    /// current transform while a gizmo drag is in progress. Cleared when the
    /// drag commits (or cancels) and the DSL writeback triggers a recompile.
    pub gizmo_drag: Option<GizmoDrag>,
    /// Edit intent produced this frame (gizmo drag commit, or
    /// app-level delete/duplicate). Polled by the app after `show`.
    pub pending_edits: Vec<PendingEdit>,
    /// If set, the app should jump the text editor caret to this byte
    /// offset. Cleared after the caret jump is applied.
    pub pending_caret: Option<usize>,
    /// Current View → Shader preview style. Never propagates to the exported
    /// GLB — it only parameterises the OpenGL draw path.
    pub preview_shader: PreviewShader,
    /// Cinema mode director. When `cinema.active`, `Viewer::show` ignores
    /// orbit/pan/zoom/click input and the paint callback skips the grid +
    /// gizmo handles so the framing reads as a clean presentation.
    pub cinema: CinemaDirector,
    /// User toggle for the ground-plane reference grid. Cinema mode hides the
    /// grid regardless of this flag.
    pub show_grid: bool,
    /// User toggle for the light-indicator overlay (point sphere / spot cone /
    /// directional arrow drawn at each `light` node's pose). Cinema mode hides
    /// these regardless of this flag.
    pub show_light_gizmos: bool,
    /// User toggle for the translate/rotate/scale handles drawn on the selected
    /// node. Cinema mode hides them regardless of this flag.
    pub show_transform_gizmo: bool,
    /// User toggle for the AABB collider wireframe overlay. Off by default —
    /// this overlay is opt-in noise for users actively working on collision.
    /// Cinema mode hides them regardless of this flag.
    pub show_colliders: bool,
    /// Active environment-lighting preset. Drives the analytic sky probe and
    /// the fallback key/fill rig used when the scene declares no `light`
    /// nodes. Persisted in the studio settings file via the matching string
    /// key in [`super::environment::environment_key`].
    pub environment: Environment,
    /// Active shadow-mapping quality preset. Drives the depth pre-pass
    /// resolution and per-frame caster cap; `Off` skips the entire pre-pass
    /// so older GPUs sit out the work. Persisted globally via
    /// `Settings::shadow_quality`.
    pub shadows: ShadowQuality,
    /// Cap on continuous viewport repaints (animation tick, cinema pan, gizmo
    /// drag). `None` = uncapped — the per-frame `request_repaint()` call goes
    /// through unchanged. `Some(fps)` routes through `request_repaint_after(
    /// 1 / fps)` instead so the next animated frame can't fire sooner than
    /// the cap permits. Input-driven repaints are unaffected.
    pub max_fps: Option<u32>,
    /// Pending offscreen capture the next paint callback should service.
    /// Cleared after processing — `capture_outcome` gets the result.
    pub capture_request: Option<CaptureRequest>,
    /// Last completed capture awaiting the app to drain. The app polls
    /// every frame via `take_capture_outcome`.
    pub capture_outcome: Option<CaptureOutcome>,
    /// Worker pool that PNG-encodes captured frames off the GL thread.
    /// Lazily created when a capture starts queueing pixels and torn down
    /// once all in-flight encodes have drained at finalisation.
    pub encode_pool: Option<EncodePool>,
    /// Figma-style click drill-down. Records the cursor and raw leaf hit
    /// from the previous viewport click so a repeat click on the same
    /// target can advance the selection one ancestor closer to the leaf.
    /// Cleared by Esc, modifier-clicks, scene recompiles, gizmo commits —
    /// anything that would make the recorded NodeId stale.
    pub pick_cycle: Option<PickCycle>,
}

/// Per-click cycle state for Figma-style drill-down. `cursor` and `leaf`
/// must both match the next click for the cycle to advance — different
/// cursor (>[`PICK_CYCLE_RADIUS_PX`] away) or a different deepest hit
/// resets the depth to 0.
#[derive(Clone, Copy, Debug)]
pub struct PickCycle {
    pub cursor: egui::Pos2,
    pub leaf: NodeId,
    /// 0 = the default `redirect_pick` target (top of the editable chain).
    /// Each repeat click adds 1, walking one node closer to `leaf`.
    pub depth: usize,
}

/// Pixel radius within which a follow-up click counts as "the same click"
/// for cycle purposes. Slightly forgiving so a hand-held mouse twitch
/// between clicks doesn't reset the cycle.
pub(super) const PICK_CYCLE_RADIUS_PX: f32 = 4.0;

/// Snapshot captured at `pointer_down` on a gizmo handle plus the running
/// delta as the user drags. Applied to the selected node's local transform
/// every time the mesh is rebuilt, so the preview tracks the mouse without
/// thrashing the source text.
#[derive(Clone, Debug)]
pub struct GizmoDrag {
    pub node: NodeId,
    pub axis: crate::gizmo::Axis,
    pub mode: crate::gizmo::GizmoMode,
    /// Node's local transform at drag start, so we can compose the delta
    /// freshly every frame rather than accumulating floating-point error.
    pub start_transform: Transform,
    /// Origin (node world translation) captured at drag start. Gizmo math
    /// runs in world space and we want a stable pivot even if the mesh
    /// rebuild moves it.
    pub start_origin: Vec3,
    /// Parent's world transform at drag start. The gizmo math is in world
    /// space (handles align to world axes) but the writeback lands on the
    /// node's *local* transform — we need this matrix's inverse to convert
    /// world deltas into local space so a node parented inside a rotated /
    /// scaled hierarchy doesn't move the wrong direction when its handle is
    /// dragged. `Mat4::IDENTITY` for roots.
    pub parent_start_world: Mat4,
    /// Cursor ray at drag start. Used by each per-frame delta calculation
    /// together with the current ray.
    pub start_ray_origin: Vec3,
    pub start_ray_dir: Vec3,
    /// Running delta to apply, expressed in WORLD space along the chosen
    /// world axis. Translate: world-units. Rotate: radians. Scale:
    /// multiplicative factor. Already snap-rounded if the user was holding
    /// Ctrl at the last update. The commit path checks this against an
    /// effective-zero threshold to skip pure-click taps where the cursor
    /// never moved off the handle.
    pub delta: f32,
}

/// Snap step for translate: drag deltas that land the node's world-axis
/// position on this grid are preferred when Ctrl is held.
pub(crate) const TRANSLATE_SNAP_STEP: f32 = 0.25;
/// Snap step for rotate: 15° feels coarse enough to be useful without
/// forcing the user to fight the handle for in-between angles.
pub(crate) const ROTATE_SNAP_STEP_DEG: f32 = 15.0;
/// Snap step for scale: 25% increments land on nice factors (0.25, 0.5,
/// 0.75, 1.0, 1.25, …) when Ctrl is held.
pub(crate) const SCALE_SNAP_STEP: f32 = 0.25;

/// Apply axis-grid snapping to a translate drag so `start + delta` lands
/// on a multiple of [`TRANSLATE_SNAP_STEP`]. Keeps the preview and the
/// committed value consistent: the live mesh and the DSL writeback both
/// see the same snapped delta.
pub(super) fn snap_translate_delta(axis_start: f32, raw_delta: f32) -> f32 {
    let step = TRANSLATE_SNAP_STEP;
    let absolute = axis_start + raw_delta;
    let snapped = (absolute / step).round() * step;
    snapped - axis_start
}

/// Round an incremental rotation (in radians) to the nearest snap step.
pub(super) fn snap_rotate_delta(raw_delta: f32) -> f32 {
    let step = ROTATE_SNAP_STEP_DEG.to_radians();
    (raw_delta / step).round() * step
}

/// Quantize a scale factor to clean 25% ticks and keep it well above zero
/// so the commit math never produces a degenerate scale.
pub(super) fn snap_scale_factor(factor: f32) -> f32 {
    let step = SCALE_SNAP_STEP;
    ((factor / step).round() * step).max(SCALE_SNAP_STEP)
}

/// What kind of capture the user requested. Carried alongside the per-frame
/// rendering instructions so the app can route the result to the right
/// completion handler (write a thumbnail PNG, kick off ffmpeg).
///
/// `PickerThumb` is a separate variant from `Thumbnail` so the file-picker's
/// background thumbnail engine can pump captures through the viewer without
/// `poll_generate` stealing the outcome and treating it as the user-driven
/// "Generate Thumbnail" menu action. `Publish` does the same for the publish
/// dialog's preview capture. `Refine` does the same for the LLM auto-refine
/// loop's source-image capture: the renderer treats it identically to a
/// thumbnail (single static frame, no animation override), the variant only
/// tells `app/llm.rs::on_refine_render_done` "this PNG is mine". All four
/// single-frame kinds behave identically inside the GL paint callback (no
/// animation override, single frame); the variant only carries "who owns
/// this outcome".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureKind {
    Thumbnail,
    Video,
    PickerThumb,
    Publish,
    Refine,
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

/// A queued capture. Posted from the app via [`super::Viewer::submit_capture`]
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

/// Result the paint callback writes back into [`ViewerState::capture_outcome`]
/// after processing a `CaptureRequest`. The app polls for it on the next frame.
#[derive(Clone, Debug)]
pub struct CaptureOutcome {
    pub kind: CaptureKind,
    pub frame_paths: Vec<PathBuf>,
    pub error: Option<String>,
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

/// Edit intent the viewport has produced this frame. The app polls this via
/// [`super::Viewer::take_pending_edits`] and, if present, maps it through the
/// span-aware text mutator in `edit.rs` to rewrite the `.mog` source.
#[derive(Clone, Debug)]
pub enum PendingEdit {
    /// Set the canonical transform attr (`pos` / `rot` / `scale`) AND delete
    /// any shadowing shortcut attrs listed in `delete` — per-axis (`x` /
    /// `y` / `z`, `rx` / `ry` / `rz`) or corner-form (`from` / `to`) — that
    /// would otherwise win on recompile and silently defeat the writeback.
    /// Pass an empty `delete` for attrs that have no DSL shortcut.
    SetAttrCanonical {
        node: NodeId,
        attr: String,
        value: String,
        delete: Vec<String>,
    },
    /// Remove the node entirely from the source. Emitted by the viewport
    /// Backspace/Delete shortcut; the drain looks up the node's source span
    /// and splices it out via `edit::delete_node`.
    DeleteNode { node: NodeId },
}

impl Default for ViewerState {
    fn default() -> Self {
        Self {
            camera: Default::default(),
            mesh: Default::default(),
            mesh_dirty: false,
            palettes_dirty: false,
            scene: None,
            base_dir: None,
            clip_active: Vec::new(),
            anim_times: Vec::new(),
            anim_playing: false,
            playback_speed: 1.0,
            static_center: Vec3::ZERO,
            static_radius: 0.0,
            selected: Vec::new(),
            selected_paths: Vec::new(),
            gizmo_mode: Default::default(),
            gizmo_drag: None,
            pending_edits: Vec::new(),
            pending_caret: None,
            preview_shader: Default::default(),
            cinema: Default::default(),
            show_grid: true,
            show_light_gizmos: true,
            show_transform_gizmo: true,
            show_colliders: false,
            environment: Environment::default(),
            shadows: ShadowQuality::default(),
            max_fps: None,
            capture_request: None,
            capture_outcome: None,
            encode_pool: None,
            pick_cycle: None,
        }
    }
}

impl ViewerState {
    pub(super) fn any_active(&self) -> bool {
        self.clip_active.iter().any(|&b| b)
    }

    /// Most-recently-selected node — the one the gizmo, inspector, and
    /// caret-jump follow. `None` when no node is selected.
    pub(super) fn primary_selected(&self) -> Option<NodeId> {
        self.selected.last().copied()
    }

    /// Full vertex+index+palette rebuild. Use only when scene topology or
    /// material data has changed (recompile, scene swap). Per-frame animation
    /// and gizmo drags should call [`Self::update_palettes`] instead — that
    /// path leaves the VBO/EBO alone and just refreshes the per-batch matrix
    /// palettes the shader applies.
    pub(super) fn rebuild_mesh(&mut self) {
        let Some(scene) = &self.scene else {
            return;
        };
        let base_dir = self.base_dir.as_deref();
        self.mesh = flatten(scene, base_dir);
        self.mesh_dirty = true;
        // The fresh mesh already carries rest-pose palettes; if any clips are
        // active or a drag is in progress, fold them in so the next paint
        // shows the live pose without waiting for the next anim tick.
        if self.any_active() || self.gizmo_drag.is_some() {
            self.update_palettes();
        }
    }

    /// Cheap per-frame refresh: rebuild only the per-batch matrix palettes
    /// from the current animation time + active clips + live gizmo drag.
    /// Sets `palettes_dirty` so the paint callback uploads the new uniforms;
    /// leaves `mesh_dirty` untouched so vertex data stays put.
    pub(super) fn update_palettes(&mut self) {
        let Some(scene) = &self.scene else {
            return;
        };
        if self.mesh.palette_sources.is_empty() {
            return;
        }
        let locals = self.live_locals();
        update_palettes(scene, &locals, &mut self.mesh);
        self.palettes_dirty = true;
    }

    /// Build the per-node local transforms for the *current* moment —
    /// rest-pose for static scenes, sampled-clip pose with a live gizmo
    /// drag overlaid when applicable. Pulled out of `update_palettes` so
    /// the light resolver (and any future per-frame consumer) can share
    /// the same composed pose without duplicating the animation/drag
    /// blending rules.
    fn live_locals(&self) -> Vec<Transform> {
        let scene = self
            .scene
            .as_ref()
            .expect("live_locals called without a scene");
        let mut locals: Vec<Transform> = scene.nodes.iter().map(|n| n.transform).collect();
        if self.any_active() {
            for (i, &active) in self.clip_active.iter().enumerate() {
                if !active {
                    continue;
                }
                if let Some(clip) = scene.clips.get(i) {
                    apply_animation(clip, self.anim_times[i], &mut locals);
                }
            }
        }
        // Live gizmo drag: overlay a preview transform on the selected node
        // so the mesh follows the cursor without rewriting the DSL every
        // frame. Applied AFTER animation so the user sees their drag offset
        // even on an animated rig (the writeback lands on the rest-pose).
        if let Some(drag) = &self.gizmo_drag {
            if let Some(t) = locals.get_mut(drag.node.0 as usize) {
                *t = apply_gizmo_drag(drag);
            }
        }
        locals
    }

    /// Resolve every `light` carrier in the active scene against the live
    /// pose. Returns an empty vec when no scene is loaded — the renderer
    /// falls back to its hard-coded key/fill rig in that case.
    pub(super) fn resolve_lights(&self) -> Vec<ResolvedLight> {
        let Some(scene) = self.scene.as_ref() else {
            return Vec::new();
        };
        // Skip the locals/world walk entirely when no node carries a light,
        // which is the common case for the bulk of example scenes.
        if scene.nodes.iter().all(|n| n.light.is_none()) {
            return Vec::new();
        }
        let locals = self.live_locals();
        let worlds = world_transforms_from_locals(scene, &locals);
        collect_lights(scene, &worlds)
    }
}

pub(super) fn aspect_for(rect: egui::Rect) -> f32 {
    (rect.width() / rect.height()).max(0.01)
}

/// Whether the gizmo for `mode` should be drawn AND respond to drags on
/// `node_id`. The render path and `begin_gizmo_drag` both consult this so
/// the visual affordance never lies — if the input layer would refuse the
/// drag, no handles are drawn and clicks fall through to camera orbit
/// without first looking active. Keep this in sync with the commit path
/// in [`commit_gizmo_drag`].
pub(super) fn gizmo_handles_supported(
    scene: &SceneGraph,
    node_id: NodeId,
    mode: crate::gizmo::GizmoMode,
) -> bool {
    let Some(node) = scene.nodes.get(node_id.0 as usize) else {
        return false;
    };
    if !node.editable {
        return false;
    }
    // Imported subtree (`use_id != None`): the node's source span points at
    // the imported file, not the active source. `replace_selection` redirects
    // picks to the nearest user-authored wrapper, but a stale selection
    // (set before the redirect existed, or restored from a path that now
    // resolves into an imported subtree) can still land here. Refusing the
    // gizmo handles is the same affordance as for replicators: no draggable
    // handle, so the user can't initiate an edit that would be silently
    // dropped or corrupt the wrong file.
    //
    // Exception: the synthesised wrapper group of `use "X" (pos=...)` for
    // an imported file also has `use_id = Some(...)`, but its source span
    // is the `use` line in the active source — set_attr can write the
    // `pos=`/`rot=`/`scale=` back through it cleanly, so allow the gizmo.
    if node.use_id.is_some() && !is_import_wrapper(scene, node_id) {
        return false;
    }
    // Relative placement (`above`/`below`/`left_of`/...) re-shifts one axis
    // of `transform.translation` every compile from the target's AABB. A
    // plain `pos=[…]` writeback would get overwritten on the snap axis when
    // the resolved value happens to be 0 (the layout pass treats `pos.y == 0`
    // as "not set" and re-runs the snap). The Translate commit handles this
    // by emitting per-axis shortcuts (`x=`, `y=`, `z=`) — see
    // `commit_gizmo_drag`. Rotation and scale are not touched by the layout
    // pass, so they round-trip through the canonical writeback unchanged.
    let _ = mode;
    // Attach-bound nodes get their transform composed as `attach + user`,
    // so a `pos=` / `rot=` writeback survives the next compile as long as
    // we subtract the attach contribution before writing. The commit path
    // does that by reading `attach_binding.anchor` / `rotation`.
    true
}

/// If the cursor is over a gizmo handle on the selected node, build a
/// [`GizmoDrag`] describing the drag-start state. Returns `None` otherwise.
pub(super) fn begin_gizmo_drag(
    st: &ViewerState,
    selected: NodeId,
    rect: egui::Rect,
    cursor: egui::Pos2,
    aspect: f32,
) -> Option<GizmoDrag> {
    let scene = st.scene.as_ref()?;
    if !gizmo_handles_supported(scene, selected, st.gizmo_mode) {
        return None;
    }
    let node = scene.nodes.get(selected.0 as usize)?;
    let worlds = scene.world_transforms();
    let world = worlds.get(selected.0 as usize).copied().unwrap_or(Mat4::IDENTITY);
    let origin = world.w_axis.truncate();
    let parent_start_world = node
        .parent
        .and_then(|p| worlds.get(p.0 as usize).copied())
        .unwrap_or(Mat4::IDENTITY);
    let viewproj = st.camera.view_proj(aspect);
    let eye = st.camera.eye();
    let (ro, rd) = crate::gizmo::screen_ray(viewproj, eye, rect, cursor);
    let scale = crate::gizmo::handle_scale(origin, eye, rect.height());
    let axis = crate::gizmo::hit_axis(st.gizmo_mode, origin, scale, ro, rd)?;
    Some(GizmoDrag {
        node: selected,
        axis,
        mode: st.gizmo_mode,
        start_transform: node.transform,
        start_origin: origin,
        parent_start_world,
        start_ray_origin: ro,
        start_ray_dir: rd,
        delta: match st.gizmo_mode {
            crate::gizmo::GizmoMode::Scale => 1.0,
            _ => 0.0,
        },
    })
}

pub(super) fn update_gizmo_drag(
    st: &mut ViewerState,
    rect: egui::Rect,
    cursor: egui::Pos2,
    aspect: f32,
    snap: bool,
) {
    let viewproj = st.camera.view_proj(aspect);
    let eye = st.camera.eye();
    let (cur_ro, cur_rd) = crate::gizmo::screen_ray(viewproj, eye, rect, cursor);
    let Some(drag_ref) = &st.gizmo_drag else { return };
    let raw_delta = match drag_ref.mode {
        crate::gizmo::GizmoMode::Translate => crate::gizmo::translate_delta(
            drag_ref.start_origin,
            drag_ref.axis,
            drag_ref.start_ray_origin,
            drag_ref.start_ray_dir,
            cur_ro,
            cur_rd,
        ),
        crate::gizmo::GizmoMode::Rotate => crate::gizmo::rotate_delta(
            drag_ref.start_origin,
            drag_ref.axis,
            drag_ref.start_ray_origin,
            drag_ref.start_ray_dir,
            cur_ro,
            cur_rd,
        ),
        crate::gizmo::GizmoMode::Scale => crate::gizmo::scale_factor(
            drag_ref.start_origin,
            drag_ref.axis,
            drag_ref.start_ray_origin,
            drag_ref.start_ray_dir,
            cur_ro,
            cur_rd,
        ),
    };
    // Apply snap (if Ctrl held) against the *world-space* starting position
    // so the grid the user lands on matches the world axes the handles are
    // drawn against. Using the local translation here would snap to a grid
    // tilted by every parent rotation in the chain — invisible to the user
    // and surprising on release.
    let new_delta = if snap {
        match drag_ref.mode {
            crate::gizmo::GizmoMode::Translate => {
                let start_axis = match drag_ref.axis {
                    crate::gizmo::Axis::X => drag_ref.start_origin.x,
                    crate::gizmo::Axis::Y => drag_ref.start_origin.y,
                    crate::gizmo::Axis::Z => drag_ref.start_origin.z,
                };
                snap_translate_delta(start_axis, raw_delta)
            }
            crate::gizmo::GizmoMode::Rotate => snap_rotate_delta(raw_delta),
            crate::gizmo::GizmoMode::Scale => snap_scale_factor(raw_delta),
        }
    } else {
        raw_delta
    };
    if let Some(drag) = st.gizmo_drag.as_mut() {
        drag.delta = new_delta;
    }
    // Drag changes one node's local transform → palette only. Vertex data
    // stays at rest-pose so no VBO/EBO upload is needed.
    st.update_palettes();
}

pub(super) fn commit_gizmo_drag(st: &mut ViewerState) -> Vec<PendingEdit> {
    let Some(drag) = st.gizmo_drag.as_ref() else {
        return Vec::new();
    };
    // Skip commit only when the numerical delta is truly zero (exact
    // press-and-release with no cursor movement). Even a sub-degree drag
    // should round-trip through the DSL so the user's intent sticks.
    let trivial = match drag.mode {
        crate::gizmo::GizmoMode::Translate | crate::gizmo::GizmoMode::Rotate => {
            drag.delta.abs() < 1e-6
        }
        crate::gizmo::GizmoMode::Scale => (drag.delta - 1.0).abs() < 1e-6,
    };
    if trivial {
        return Vec::new();
    }
    use crate::gizmo::GizmoMode;
    // Shortcut/corner-form attrs that the DSL lets override the canonical
    // transform field. The gizmo must strip these on commit or the author's
    // original shortcuts silently defeat the writeback on recompile.
    // `dir` belongs in ROT_SHADOWS only for `light` nodes — its presence on
    // a light forces the lower step to recompute rotation from the direction
    // vector and silently discards the gizmo's `rot=` writeback. No other
    // top-level scene node consumes `dir`, so stripping it unconditionally
    // is safe (connectors carry their own `dir` but are children, not
    // selectable scene nodes — the source-span writeback only touches the
    // selected node's own attrs).
    const POS_SHADOWS: &[&str] = &["x", "y", "z", "from", "to"];
    const ROT_SHADOWS: &[&str] = &["rx", "ry", "rz", "dir"];
    let final_local = apply_gizmo_drag(drag);
    // For attach-bound nodes the post-compile transform is `attach + user`,
    // so the `pos=` we write back must be `final_local - attach_anchor`.
    // Likewise for `rot=`: the composed rotation is `user_rot * attach_rot`,
    // so user_rot = final_rot * attach_rot⁻¹.
    let attach_binding = st
        .scene
        .as_ref()
        .and_then(|s| s.nodes.get(drag.node.0 as usize))
        .and_then(|n| n.attach_binding.clone());
    let user_pos = match &attach_binding {
        Some(b) => final_local.translation - b.anchor_vec3(),
        None => final_local.translation,
    };
    let user_rot = match &attach_binding {
        Some(b) => final_local.rotation * b.rotation_quat().inverse(),
        None => final_local.rotation,
    };
    let relative_placed = st
        .scene
        .as_ref()
        .and_then(|s| s.nodes.get(drag.node.0 as usize))
        .map(|n| n.relative_placed)
        .unwrap_or(false);
    match drag.mode {
        GizmoMode::Translate if relative_placed => {
            // The layout pass (`above`/`below`/`left_of`/...) re-shifts one
            // axis of `transform.translation` from the target's AABB unless
            // `pos_axis_explicit` finds a non-zero shortcut/`pos` component
            // for that axis. A single `pos=[…]` writeback would lose to the
            // snap when the resolved value happens to be 0 — so emit per-axis
            // shortcuts (`x=`/`y=`/`z=`) instead. Whichever axis is the snap
            // axis, the matching shortcut takes precedence in `resolve_pos`
            // and trips `pos_axis_explicit` at recompile, freezing the node
            // at the dragged position. Setting a shortcut to `0` is the
            // documented "release the snap" gesture, so values that happen
            // to land at 0 still hand control back to the layout pass.
            let mut edits = Vec::with_capacity(3);
            // First edit also strips `pos`/`from`/`to` so the canonical and
            // corner-form attrs don't fight the new shortcuts on recompile.
            edits.push(PendingEdit::SetAttrCanonical {
                node: drag.node,
                attr: "x".to_string(),
                value: format_scalar(user_pos.x),
                delete: ["pos", "from", "to"].iter().map(|s| s.to_string()).collect(),
            });
            edits.push(PendingEdit::SetAttrCanonical {
                node: drag.node,
                attr: "y".to_string(),
                value: format_scalar(user_pos.y),
                delete: Vec::new(),
            });
            edits.push(PendingEdit::SetAttrCanonical {
                node: drag.node,
                attr: "z".to_string(),
                value: format_scalar(user_pos.z),
                delete: Vec::new(),
            });
            edits
        }
        GizmoMode::Translate => {
            // Write the full vec3 under `pos=` rather than a lone axis
            // shortcut (`x=`, `y=`, `z=`). Dragging the X handle, then Y,
            // then Z would otherwise smear three redundant attrs across
            // the header — this keeps the round-trip clean and matches
            // how most .mog sources are authored. The new value is the
            // node's *local* translation after the world-space drag has
            // been pulled back through the parent inverse, so children of
            // a rotated/scaled parent move along the world axis the user
            // grabbed instead of the parent's tilted axis.
            vec![PendingEdit::SetAttrCanonical {
                node: drag.node,
                attr: "pos".to_string(),
                value: format!(
                    "[{}, {}, {}]",
                    format_scalar(user_pos.x),
                    format_scalar(user_pos.y),
                    format_scalar(user_pos.z)
                ),
                delete: POS_SHADOWS.iter().map(|s| s.to_string()).collect(),
            }]
        }
        GizmoMode::Rotate => {
            // `apply_gizmo_drag` already conjugated the world-axis rotation
            // through the parent's world rotation, so `user_rot` is the
            // local-space quaternion to write. Decompose to Euler XYZ
            // degrees for the existing `rot=[x,y,z]` DSL surface.
            let (rx, ry, rz) = user_rot.to_euler(glam::EulerRot::XYZ);
            let value = format!(
                "[{}, {}, {}]",
                format_scalar(rx.to_degrees()),
                format_scalar(ry.to_degrees()),
                format_scalar(rz.to_degrees())
            );
            vec![PendingEdit::SetAttrCanonical {
                node: drag.node,
                attr: "rot".to_string(),
                value,
                delete: ROT_SHADOWS.iter().map(|s| s.to_string()).collect(),
            }]
        }
        GizmoMode::Scale => {
            let new_scale = final_local.scale;
            let value = format!(
                "[{}, {}, {}]",
                format_scalar(new_scale.x),
                format_scalar(new_scale.y),
                format_scalar(new_scale.z)
            );
            // Scale has no DSL shortcut attrs — no shadows to strip.
            vec![PendingEdit::SetAttrCanonical {
                node: drag.node,
                attr: "scale".to_string(),
                value,
                delete: Vec::new(),
            }]
        }
    }
}

/// Format a scalar for DSL writeback: four-decimal trim, strip trailing
/// zeros and a lone trailing `.` so "1.0000" becomes "1" not "1.". Also
/// normalises `-0` (which Euler decomposition readily produces for zero
/// rotations) to `0` so the source diff stays readable.
fn format_scalar(v: f32) -> String {
    let v = if v == 0.0 { 0.0 } else { v }; // collapse -0.0 → 0.0
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// Replace the selection with a single node (or clear it). Used for plain
/// (non-modifier) clicks and `Esc`. Picks that land on an imported subtree
/// (`use_id != None`) get redirected to the nearest user-authored wrapper —
/// the group whose span lives in the active source. Without this the gizmo
/// / inspector would write back at byte offsets from a different file and
/// either no-op or silently corrupt the active scene. See `redirect_pick`.
///
/// On a successful selection the editor caret jumps to the node's declaration.
pub(super) fn replace_selection(st: &mut ViewerState, id: Option<NodeId>) {
    let id = id.and_then(|n| {
        st.scene.as_ref().and_then(|s| redirect_pick(s, n))
    });
    set_primary_selection_raw(st, id);
}

/// Set the selection to exactly `id` (or clear when `None`) without any
/// `redirect_pick` rewriting. Used by the cycling drill-down where the
/// caller has already computed the precise target — re-running the
/// redirect would walk the cycle's deeper picks back up to the wrapper /
/// outer group, defeating the drill.
fn set_primary_selection_raw(st: &mut ViewerState, id: Option<NodeId>) {
    st.selected.clear();
    st.selected_paths.clear();
    if let Some(n) = id {
        st.selected.push(n);
        if let Some(path) = st.scene.as_ref().and_then(|s| node_path(s, n)) {
            st.selected_paths.push(path);
        }
    }
    st.pending_caret = id
        .and_then(|n| {
            st.scene
                .as_ref()
                .and_then(|s| s.nodes.get(n.0 as usize))
                .and_then(|node| node.source_span)
        })
        .map(|span| span.start);
}

/// Figma-style click drill-down. Replaces the selection with the node
/// `redirect_pick` would normally pick (the editable wrapper or top-level
/// group), unless this click targets the same screen point and the same
/// raw leaf as the previous click — in which case it advances one
/// ancestor closer to `leaf`. Repeat-clicking eventually lands on `leaf`
/// itself (or stops one short, when crossing into a node would land on a
/// span from an imported `.mog` file that the gizmo can't legally edit).
///
/// Resets the cycle to depth 0 whenever the cursor or the deepest hit
/// changes between consecutive clicks. The caller is responsible for
/// clearing `pick_cycle` on selection changes from other sources (Esc,
/// shift-click, scene recompile, gizmo commit).
pub(super) fn replace_selection_cycling(
    st: &mut ViewerState,
    leaf: NodeId,
    cursor: egui::Pos2,
) {
    let Some(scene) = st.scene.as_ref().map(Arc::clone) else {
        replace_selection(st, Some(leaf));
        st.pick_cycle = None;
        return;
    };

    // chain = [leaf, parent, grandparent, …, root]
    let mut chain: Vec<NodeId> = Vec::new();
    let mut cur = Some(leaf);
    while let Some(id) = cur {
        chain.push(id);
        cur = scene.nodes.get(id.0 as usize).and_then(|n| n.parent);
    }
    if chain.is_empty() {
        replace_selection(st, Some(leaf));
        st.pick_cycle = None;
        return;
    }

    // Depth-0 selection: whatever `redirect_pick` would return today.
    // For imports that's the wrapper; for plain user-authored geometry
    // it's the leaf itself, in which case cycling is a no-op (nothing
    // deeper to walk to). When `redirect_pick` returns `None` (imported
    // root with no editable ancestor anywhere) we mirror today's
    // `replace_selection` and clear the selection — landing on the
    // un-editable leaf instead would let the user grab a gizmo handle on
    // a node whose source span lives in another file.
    let Some(default_id) = redirect_pick(&scene, leaf) else {
        set_primary_selection_raw(st, None);
        st.pick_cycle = None;
        return;
    };
    let default_idx = chain.iter().position(|&n| n == default_id).unwrap_or(0);

    let same_target = match st.pick_cycle {
        Some(pc) => {
            pc.leaf == leaf
                && (pc.cursor - cursor).length() <= PICK_CYCLE_RADIUS_PX
        }
        None => false,
    };
    let prev_depth = st.pick_cycle.map(|pc| pc.depth).unwrap_or(0);
    let candidate_depth = if same_target { prev_depth + 1 } else { 0 };

    // Editability boundary: stop one short of any node whose source span
    // lives in an imported file. The depth-0 target is always safe
    // (`redirect_pick` already enforces that), so the walk-down only
    // needs to check the nodes strictly between it and the leaf.
    let max_depth = max_editable_depth(&scene, &chain, default_idx);
    let depth = candidate_depth.min(max_depth);

    let target_idx = default_idx.saturating_sub(depth);
    let target = chain[target_idx];
    set_primary_selection_raw(st, Some(target));
    st.pick_cycle = Some(PickCycle { cursor, leaf, depth });
}

/// How many steps from `default_idx` toward the leaf (index 0) we can
/// take before crossing into a node authored in another file. Caller
/// uses this to clamp the requested cycle depth.
fn max_editable_depth(scene: &SceneGraph, chain: &[NodeId], default_idx: usize) -> usize {
    let mut depth = 0usize;
    let mut idx = default_idx;
    while idx > 0 {
        let next = chain[idx - 1];
        let Some(node) = scene.nodes.get(next.0 as usize) else {
            break;
        };
        if node.origin.is_some() {
            break;
        }
        depth += 1;
        idx -= 1;
    }
    depth
}

/// Toggle a node's membership in the selection. Used for shift/cmd-click.
/// Picks are redirected through `redirect_pick` first (same reason as
/// `replace_selection`). If the node is already selected, it's removed and
/// the primary becomes whichever entry is now last (no caret jump unless the
/// primary changed). Otherwise the node is appended (becoming the new primary)
/// and the caret jumps to its declaration.
pub(super) fn toggle_selection(st: &mut ViewerState, id: NodeId) {
    let Some(target) = st.scene.as_ref().and_then(|s| redirect_pick(s, id)) else {
        return;
    };
    if let Some(pos) = st.selected.iter().position(|n| *n == target) {
        let was_primary = pos + 1 == st.selected.len();
        st.selected.remove(pos);
        if pos < st.selected_paths.len() {
            st.selected_paths.remove(pos);
        }
        if was_primary {
            // Caret follows the new primary, if there is one. No jump when
            // the selection emptied — the editor stays put.
            st.pending_caret = st
                .selected
                .last()
                .copied()
                .and_then(|n| {
                    st.scene
                        .as_ref()
                        .and_then(|s| s.nodes.get(n.0 as usize))
                        .and_then(|node| node.source_span)
                })
                .map(|span| span.start);
        }
    } else {
        st.selected.push(target);
        if let Some(path) = st.scene.as_ref().and_then(|s| node_path(s, target)) {
            st.selected_paths.push(path);
        }
        st.pending_caret = st
            .scene
            .as_ref()
            .and_then(|s| s.nodes.get(target.0 as usize))
            .and_then(|node| node.source_span)
            .map(|span| span.start);
    }
}


/// Walk from `id` up through parents to the nearest ancestor authored
/// directly in the active source (`use_id == None`). Returns the original
/// `id` when it's already user-authored. Returns `None` when the walk
/// runs out without finding one — e.g. `scene { use "desk" }` with no
/// wrapping group, where no parent has a span in the active file.
///
/// Editing a node carrying `use_id != None` would splice into the active
/// `.mog` source at byte offsets that come from the imported file, so the
/// viewport's gizmo + inspector route every interaction through this
/// redirect first. The output is what the user actually manipulates.
///
/// Import wrappers are a special case: `use "X" (pos=...)` of an imported
/// file synthesises a wrapper group whose `use_id` is set (it opens a new
/// frame) but whose `origin` is `None` (the `use` line lives in the active
/// source). For a top-level `use` the wrapper is a root with no further
/// ancestors, so the plain walk-up-to-`use_id == None` rule bottoms out at
/// `None` and the user can never select the import. The fallback below
/// detects the wrapper by the origin transition (parent `origin = None`,
/// child `origin = Some(...)`) and returns it when no fully use-free
/// ancestor exists.
pub(super) fn redirect_pick(scene: &SceneGraph, id: NodeId) -> Option<NodeId> {
    let node = scene.nodes.get(id.0 as usize)?;
    if node.use_id.is_none() {
        return Some(id);
    }
    let mut import_wrapper: Option<NodeId> = None;
    let mut prev_origin_some = node.origin.is_some();
    let mut cur = node.parent;
    while let Some(pid) = cur {
        let parent = scene.nodes.get(pid.0 as usize)?;
        if parent.use_id.is_none() {
            return Some(pid);
        }
        if parent.origin.is_none() && prev_origin_some && import_wrapper.is_none() {
            import_wrapper = Some(pid);
        }
        prev_origin_some = parent.origin.is_some();
        cur = parent.parent;
    }
    import_wrapper
}

/// True when `id` is the synthesised wrapper group of a `use "..."` of an
/// imported file. Such wrappers carry `use_id = Some(...)` (they open a
/// new frame) but `origin = None` (the `use` was authored in the active
/// source) and contain at least one descendant whose `origin` is `Some`
/// (the imported body). The viewport gizmo and inspector treat them as
/// editable even though `use_id` is set, because the wrapper's source
/// span points at the active `.mog` and a `pos=` writeback round-trips
/// cleanly through `set_attr` on the `use` line.
pub fn is_import_wrapper(scene: &SceneGraph, id: NodeId) -> bool {
    let Some(node) = scene.nodes.get(id.0 as usize) else {
        return false;
    };
    if node.use_id.is_none() || node.origin.is_some() {
        return false;
    }
    has_imported_descendant(scene, id)
}

fn has_imported_descendant(scene: &SceneGraph, id: NodeId) -> bool {
    let Some(node) = scene.nodes.get(id.0 as usize) else {
        return false;
    };
    for &cid in &node.children {
        let Some(child) = scene.nodes.get(cid.0 as usize) else {
            continue;
        };
        if child.origin.is_some() {
            return true;
        }
        if has_imported_descendant(scene, cid) {
            return true;
        }
    }
    false
}

pub(super) fn apply_gizmo_drag(drag: &GizmoDrag) -> Transform {
    use crate::gizmo::GizmoMode;
    let mut t = drag.start_transform;
    let world_axis = drag.axis.unit();
    // Decompose the parent's world matrix once. The rotation is what we
    // conjugate the gizmo through; the inverse-of-the-full-matrix is what
    // we pull translation deltas back through (so a parent that scales
    // doesn't make a 1-unit drag move the child by 1 / parent_scale).
    let parent_inv = drag.parent_start_world.inverse();
    let (parent_scale, parent_rot, _) =
        drag.parent_start_world.to_scale_rotation_translation();
    let parent_rot_inv = parent_rot.inverse();
    match drag.mode {
        GizmoMode::Translate => {
            let world_delta = world_axis * drag.delta;
            // Pull the world-space translation delta back through the
            // parent's full transform — handles parent rotation AND
            // non-uniform scale in one shot.
            let local_delta = parent_inv.transform_vector3(world_delta);
            t.translation += local_delta;
        }
        GizmoMode::Rotate => {
            // Rotate around the WORLD axis: q_local must conjugate through
            // the parent's world rotation so the post-compile world
            // rotation is `q_world * old_world_rot`.
            let q_world = Quat::from_axis_angle(world_axis, drag.delta);
            let q_local = parent_rot_inv * q_world * parent_rot;
            t.rotation = (q_local * t.rotation).normalize();
        }
        GizmoMode::Scale => {
            // Non-uniform scale doesn't compose cleanly through a rotated
            // parent (it would require shear, which TRS can't express).
            // Project the world axis into the parent's local frame and
            // apply the factor on the dominant local-axis component — for
            // a parent with no rotation this collapses to the obvious
            // per-axis scale; for a rotated parent it picks the closest
            // axis match, which is the best a TRS rig can do.
            let factor = drag.delta.max(0.01);
            let local_axis = parent_rot_inv * world_axis;
            let i = dominant_axis_index(local_axis);
            let s0 = drag.start_transform.scale;
            t.scale = match i {
                0 => Vec3::new(s0.x * factor, s0.y, s0.z),
                1 => Vec3::new(s0.x, s0.y * factor, s0.z),
                _ => Vec3::new(s0.x, s0.y, s0.z * factor),
            };
            // Reference parent_scale so a future "scale-uniform-with-parent"
            // toggle has the value to hand. Currently unused.
            let _ = parent_scale;
        }
    }
    t
}

fn dominant_axis_index(v: Vec3) -> usize {
    let ax = v.x.abs();
    let ay = v.y.abs();
    let az = v.z.abs();
    if ax >= ay && ax >= az {
        0
    } else if ay >= az {
        1
    } else {
        2
    }
}

/// Walk from `id` up to a root collecting `(name, sibling_disambiguator)`
/// in root → ... → node order. The disambiguator is the index of the node
/// among siblings under the same parent that share its `name` (in scene
/// order). Replicators (`array`, `mirror`, …) produce siblings with
/// identical names, so name-only paths would collide.
pub(super) fn node_path(scene: &SceneGraph, id: NodeId) -> Option<SelectionPath> {
    if id.0 as usize >= scene.nodes.len() {
        return None;
    }
    let mut out: SelectionPath = Vec::new();
    let mut cur = Some(id);
    while let Some(n) = cur {
        let node = scene.nodes.get(n.0 as usize)?;
        let siblings: &[NodeId] = match node.parent {
            Some(pid) => &scene.nodes.get(pid.0 as usize)?.children,
            None => &scene.roots,
        };
        let mut disamb: u32 = 0;
        for sib in siblings {
            if *sib == n {
                break;
            }
            if let Some(s) = scene.nodes.get(sib.0 as usize) {
                if s.name == node.name {
                    disamb += 1;
                }
            }
        }
        out.push((node.name.clone(), disamb));
        cur = node.parent;
    }
    out.reverse();
    Some(out)
}

/// Re-resolve a saved selection path against a freshly-lowered scene by
/// walking root → leaf and picking the `disamb`-th same-named child at each
/// step. Returns `None` when any step finds no matching sibling — usually
/// because the node was deleted by the most recent edit.
pub(super) fn resolve_node_path(scene: &SceneGraph, path: &[(String, u32)]) -> Option<NodeId> {
    let mut iter = path.iter();
    let (root_name, root_disamb) = iter.next()?;
    let mut current = pick_nth_named(scene, &scene.roots, root_name, *root_disamb)?;
    for (name, disamb) in iter {
        let children = &scene.nodes.get(current.0 as usize)?.children;
        current = pick_nth_named(scene, children, name, *disamb)?;
    }
    Some(current)
}

fn pick_nth_named(scene: &SceneGraph, ids: &[NodeId], name: &str, n: u32) -> Option<NodeId> {
    let mut count: u32 = 0;
    for &id in ids {
        let node = scene.nodes.get(id.0 as usize)?;
        if node.name == name {
            if count == n {
                return Some(id);
            }
            count += 1;
        }
    }
    None
}

/// Find the deepest user-authored scene node whose `source_span` contains
/// `byte_offset`. "Deepest" = smallest containing span, so a click inside a
/// child wins over its enclosing group. Nodes lowered from imported `.mog`
/// files (`origin = Some(...)`) are skipped: their spans index into another
/// file, not the active source. Returns `None` when the offset falls in a
/// comment, whitespace, or otherwise outside every node's authored range —
/// callers preserve the existing selection in that case rather than treating
/// it as a deselect.
pub(super) fn find_deepest_node_at_offset(
    scene: &SceneGraph,
    byte_offset: usize,
) -> Option<NodeId> {
    let mut best: Option<(NodeId, usize)> = None;
    for (idx, node) in scene.nodes.iter().enumerate() {
        if node.origin.is_some() {
            continue;
        }
        let Some(span) = node.source_span else {
            continue;
        };
        // Half-open: a caret resting at `span.end` belongs to whatever
        // structure starts there (or to none, if nothing follows). Without
        // this, two adjacent siblings with `prev.end == next.start` would
        // both claim the boundary offset and the deepest-by-length tiebreak
        // would pick whichever happened to be enumerated last.
        if byte_offset < span.start || byte_offset >= span.end {
            continue;
        }
        let len = span.end - span.start;
        match best {
            None => best = Some((NodeId(idx as u32), len)),
            Some((_, prev_len)) if len < prev_len => best = Some((NodeId(idx as u32), len)),
            _ => {}
        }
    }
    best.map(|(id, _)| id)
}
