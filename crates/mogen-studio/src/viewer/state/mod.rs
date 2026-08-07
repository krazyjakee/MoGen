//! Shared `ViewerState` plus the four focused submodules that operate on
//! it. `state/mod.rs` keeps the struct definition + impl methods and
//! re-exports every helper the rest of the crate (and the existing tests)
//! used to see at `viewer::state::*`.

use std::path::PathBuf;
use std::sync::Arc;

use glam::{Mat4, Vec3};
use mogen_core::{NodeId, SceneGraph, Transform};

use super::anim::{apply_animation, world_transforms_from_locals};
use super::camera::OrbitCamera;
use super::cinema::CinemaDirector;
use super::free_cam::FreeCam;
use super::environment::Environment;
use super::flatten::{flatten, update_palettes, FlatMesh};
use super::lights::{collect_lights, ResolvedLight};
use super::shadows::ShadowQuality;
use crate::preview_shader::PreviewShader;

mod capture;
mod gizmo;
mod path;
mod selection;

pub use capture::{
    CaptureFrame, CaptureKind, CaptureOutcome, CaptureRequest, EncodePool, ImposterOutcome,
    ImposterRequest, ImposterViewOverlay,
};
#[allow(unused_imports)]
pub use gizmo::{GizmoDrag, GizmoTarget, PendingEdit, TrackBinding};
#[allow(unused_imports)]
pub use selection::{is_import_wrapper, PickCycle};

// Re-export everything `viewer.rs` and the in-crate tests expect to see at
// the `viewer::state::*` path. Visibility tracks what was on the original
// `state.rs` so this refactor is invisible to call sites.
#[allow(unused_imports)]
pub(crate) use gizmo::{
    apply_gizmo_drag, apply_gizmo_drag_to, aspect_for, begin_gizmo_drag, commit_gizmo_drag,
    find_active_constant_track, gizmo_handles_supported, snap_rotate_delta, snap_scale_factor,
    snap_translate_delta, track_property_for_gizmo, update_gizmo_drag, ROTATE_SNAP_STEP_DEG,
    SCALE_SNAP_STEP, TRANSLATE_SNAP_STEP,
};
#[allow(unused_imports)]
pub(crate) use path::{
    find_deepest_node_at_offset, find_use_at_offset, node_path, resolve_node_path,
};
#[allow(unused_imports)]
pub(crate) use selection::{
    redirect_pick, replace_selection, replace_selection_cycling, toggle_selection,
    PICK_CYCLE_RADIUS_PX,
};

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
    /// Last user-shader failure as `(shader name, message)` — a `.glsl` that
    /// wouldn't load or wouldn't compile. Written by the paint callback (the
    /// only place with a GL context) and read by the overlay, so a shader that
    /// falls back to standard PBR explains itself instead of just looking wrong.
    pub shader_error: Option<(String, String)>,
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
    /// Free-fly camera. When `free_cam.active`, `Viewer::show` drives the
    /// camera from WASD/arrow keys + right-drag mouse-look instead of orbit,
    /// and writes the equivalent orbit pose back so the renderer and picking
    /// keep working through `camera`.
    pub free_cam: FreeCam,
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
    /// Pending imposter atlas bake. The paint callback consumes this and
    /// posts the result to `imposter_outcome`. Studio's preview modal and
    /// the bundle-LODs export both submit through this slot so the bake
    /// runs on the existing GL context instead of trying to spin up a
    /// second winit event loop.
    pub imposter_request: Option<ImposterRequest>,
    /// Last completed imposter bake awaiting the app to drain. Polled each
    /// frame by `poll_imposter_preview` / the export pipeline.
    pub imposter_outcome: Option<ImposterOutcome>,
    /// Whether the viewport is in imposter-preview mode. When set, the
    /// paint callback skips the main scene draw and renders the cached
    /// billboard instead.
    pub imposter_view_active: bool,
    /// Set when the cached overlay (if any) is stale and needs re-baking
    /// next paint. Flipped by `set_imposter_view_scene` on every scene
    /// recompile so live edits propagate without the user toggling the
    /// mode off and back on.
    pub imposter_view_dirty: bool,
    /// Scene the next imposter view bake should run against. Cloned cheaply
    /// (it's an `Arc`), kept on the state because the paint callback owns
    /// no app references.
    pub imposter_view_scene: Option<Arc<SceneGraph>>,
    /// Cached atlas texture + extent for the active billboard preview.
    /// Built in the paint callback the first time the mode is entered (or
    /// after a re-bake); freed when the mode is left.
    pub imposter_view_overlay: Option<ImposterViewOverlay>,
    /// Figma-style click drill-down. Records the cursor and raw leaf hit
    /// from the previous viewport click so a repeat click on the same
    /// target can advance the selection one ancestor closer to the leaf.
    /// Cleared by Esc, modifier-clicks, scene recompiles, gizmo commits —
    /// anything that would make the recorded NodeId stale.
    pub pick_cycle: Option<PickCycle>,
}

impl Default for ViewerState {
    fn default() -> Self {
        Self {
            camera: Default::default(),
            mesh: Default::default(),
            mesh_dirty: false,
            palettes_dirty: false,
            shader_error: None,
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
            free_cam: Default::default(),
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
            imposter_request: None,
            imposter_outcome: None,
            imposter_view_active: false,
            imposter_view_dirty: false,
            imposter_view_scene: None,
            imposter_view_overlay: None,
            pick_cycle: None,
        }
    }
}

impl ViewerState {
    pub(crate) fn any_active(&self) -> bool {
        self.clip_active.iter().any(|&b| b)
    }

    /// Most-recently-selected node — the one the gizmo, inspector, and
    /// caret-jump follow. `None` when no node is selected.
    pub(crate) fn primary_selected(&self) -> Option<NodeId> {
        self.selected.last().copied()
    }

    /// Full vertex+index+palette rebuild. Use only when scene topology or
    /// material data has changed (recompile, scene swap). Per-frame animation
    /// and gizmo drags should call [`Self::update_palettes`] instead — that
    /// path leaves the VBO/EBO alone and just refreshes the per-batch matrix
    /// palettes the shader applies.
    pub(crate) fn rebuild_mesh(&mut self) {
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
    pub(crate) fn update_palettes(&mut self) {
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

    /// Per-node local transforms with active clips applied but **no** live
    /// gizmo drag overlaid. Used by `begin_gizmo_drag` to capture an
    /// animated start pose (so a track-bound drag composes off the visible
    /// pose) and by the gizmo renderer to align handles with the animated
    /// world position the user is actually looking at.
    pub(crate) fn animated_locals(&self) -> Vec<Transform> {
        let scene = self
            .scene
            .as_ref()
            .expect("animated_locals called without a scene");
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
        locals
    }

    /// Build the per-node local transforms for the *current* moment —
    /// rest-pose for static scenes, sampled-clip pose with a live gizmo
    /// drag overlaid when applicable. Pulled out of `update_palettes` so
    /// the light resolver (and any future per-frame consumer) can share
    /// the same composed pose without duplicating the animation/drag
    /// blending rules.
    fn live_locals(&self) -> Vec<Transform> {
        let mut locals = self.animated_locals();
        // Live gizmo drag: overlay a preview transform on the selected node
        // so the mesh follows the cursor without rewriting the DSL every
        // frame. Applied AFTER animation so the user sees their drag offset
        // even on an animated rig (the writeback lands on the rest-pose).
        if let Some(drag) = &self.gizmo_drag {
            if let Some(t) = locals.get_mut(drag.node.0 as usize) {
                *t = apply_gizmo_drag(drag);
            }
            // Multi-select: move every other selected node by the same delta
            // so the preview matches what the commit will write back.
            for target in &drag.others {
                if let Some(t) = locals.get_mut(target.node.0 as usize) {
                    *t = apply_gizmo_drag_to(drag, target.start_transform, target.parent_start_world);
                }
            }
        }
        locals
    }

    /// World transforms reflecting active clips and the live gizmo drag.
    /// Drives the gizmo handle position so it tracks the visible pose
    /// instead of jumping to the bone's rest-pose origin during a track-
    /// bound drag.
    pub(crate) fn live_worlds(&self) -> Vec<Mat4> {
        let scene = self
            .scene
            .as_ref()
            .expect("live_worlds called without a scene");
        let locals = self.live_locals();
        world_transforms_from_locals(scene, &locals)
    }

    /// Resolve every `light` carrier in the active scene against the live
    /// pose. Returns an empty vec when no scene is loaded — the renderer
    /// falls back to its hard-coded key/fill rig in that case.
    pub(crate) fn resolve_lights(&self) -> Vec<ResolvedLight> {
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
