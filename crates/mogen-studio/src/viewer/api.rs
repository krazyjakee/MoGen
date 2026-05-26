//! Small accessor / mutator methods on [`Viewer`]. Split out of
//! `viewer.rs` so the main file can focus on construction and the
//! per-frame `show()` callback. None of these methods touch GL — they
//! all just take the state lock and read or update one field.

use std::path::Path;
use std::sync::Arc;

use mogen_core::{NodeId, SceneGraph};

use super::flatten::FlatMesh;
use super::shadows;
use super::state::{
    find_deepest_node_at_offset, find_use_at_offset, node_path, resolve_node_path, ViewerState,
};
use super::{
    CaptureKind, CaptureOutcome, CaptureRequest, ClipSummary, Environment, ImposterOutcome,
    ImposterRequest, PendingEdit, SelectionPath, Viewer,
};
use crate::preview_shader::PreviewShader;

impl Viewer {
    pub fn set_scene(&self, scene: Arc<SceneGraph>, base_dir: Option<&Path>, fit_camera: bool) {
        let mut st = self.state.lock().unwrap();
        let viewer_was_empty = st.scene.is_none();
        st.base_dir = base_dir.map(|p| p.to_path_buf());
        // Re-resolve every saved path against the new scene; drop any that
        // no longer resolve (a node deleted by the latest edit just falls
        // out of selection silently).
        let mut new_selected = Vec::with_capacity(st.selected_paths.len());
        let mut new_paths = Vec::with_capacity(st.selected_paths.len());
        for path in &st.selected_paths {
            if let Some(id) = resolve_node_path(&scene, path) {
                new_selected.push(id);
                new_paths.push(path.clone());
            }
        }
        st.selected = new_selected;
        st.selected_paths = new_paths;
        st.gizmo_drag = None;
        // NodeIds may have been renumbered by the recompile, so the
        // recorded leaf in `pick_cycle` no longer refers to a stable
        // node. Reset the cycle so the next click starts fresh.
        st.pick_cycle = None;

        let prev_active: Vec<String> = match &st.scene {
            Some(prev) => prev
                .clips
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    (*st.clip_active.get(i).unwrap_or(&false)).then(|| c.name.clone())
                })
                .collect(),
            None => Vec::new(),
        };
        // Only treat the incoming scene as a recompile of the current scene
        // (and preserve user toggles) when at least one clip name is shared
        // with the previous scene. A swap to an unrelated scene — including
        // the cascade of `set_scene` calls when the studio restores multiple
        // tabs at startup — has no overlap, so default its clips to active
        // instead of leaving them all off and hiding the user's animations.
        let prev_clip_names: std::collections::HashSet<&str> = match &st.scene {
            Some(prev) => prev.clips.iter().map(|c| c.name.as_str()).collect(),
            None => std::collections::HashSet::new(),
        };
        let names_overlap = scene
            .clips
            .iter()
            .any(|c| prev_clip_names.contains(c.name.as_str()));

        let mut clip_active: Vec<bool> = vec![false; scene.clips.len()];
        if viewer_was_empty || (!names_overlap && !scene.clips.is_empty()) {
            for a in &mut clip_active {
                *a = true;
            }
            st.anim_playing = !scene.clips.is_empty();
        } else {
            for (i, clip) in scene.clips.iter().enumerate() {
                if prev_active.iter().any(|n| n == &clip.name) {
                    clip_active[i] = true;
                }
            }
        }
        st.clip_active = clip_active;
        st.anim_times = vec![0.0; scene.clips.len()];
        st.scene = Some(scene);
        // Single flatten via rebuild_mesh — vertex positions are rest-pose
        // (animation is applied by the joint-palette uniform in the shader),
        // so the resulting mesh AABB is the static framing bound. Pulling
        // it from `st.mesh` here avoids a redundant flatten() purely for
        // the camera-fit numbers.
        st.rebuild_mesh();
        st.static_center = st.mesh.center;
        st.static_radius = st.mesh.radius;
        // Camera fit is caller-driven: the App knows whether this is the first
        // time a given file's scene is being shown. Subsequent compiles
        // (gizmo release, keystroke debounce) pass `false` so `camera.target` /
        // `camera.fit_distance` stay put — recentering on every compile would
        // make it look like the user's edit was rejected. First-render uses
        // the same helper as the Frame button so a freshly-loaded model
        // composes identically to pressing Frame (yaw/pitch reset included).
        if fit_camera {
            Self::frame_camera_to_static(&mut st);
        }
    }

    pub fn clear(&self) {
        let mut st = self.state.lock().unwrap();
        st.mesh = FlatMesh::default();
        st.mesh_dirty = true;
        st.scene = None;
        st.base_dir = None;
        st.clip_active.clear();
        st.anim_times.clear();
        st.selected.clear();
        st.selected_paths.clear();
        st.gizmo_drag = None;
        st.pick_cycle = None;
    }

    /// True when no scene is loaded yet (fresh launch with an empty buffer).
    /// Used by the viewport overlay to render a help message.
    pub fn has_scene(&self) -> bool {
        self.state.lock().unwrap().scene.is_some()
    }

    /// Replace the selection with a single node (or clear it). Equivalent
    /// to a plain click in the viewport.
    pub fn set_primary_selection(&self, id: Option<NodeId>) {
        let mut st = self.state.lock().unwrap();
        st.selected.clear();
        st.selected_paths.clear();
        if let Some(n) = id {
            st.selected.push(n);
            if let Some(path) = st.scene.as_ref().and_then(|s| node_path(s, n)) {
                st.selected_paths.push(path);
            }
        }
        st.gizmo_drag = None;
        st.pick_cycle = None;
    }

    /// Most-recently-selected node — the one the gizmo, inspector, and
    /// caret-jump follow. `None` when no node is selected.
    pub fn primary_selection(&self) -> Option<NodeId> {
        self.state.lock().unwrap().selected.last().copied()
    }

    /// Full selection set in click order; the last entry is the primary.
    pub fn all_selected(&self) -> Vec<NodeId> {
        self.state.lock().unwrap().selected.clone()
    }

    /// All stable name-paths in click order, parallel to `all_selected()`.
    /// Used by the undo stack to capture / restore the full selection across
    /// recompiles when raw `NodeId` indices may have shifted.
    pub fn all_selected_paths(&self) -> Vec<SelectionPath> {
        self.state.lock().unwrap().selected_paths.clone()
    }

    /// Reverse of the viewport-pick → caret jump: update the primary
    /// selection to whichever user-authored node's `source_span` contains
    /// `byte_offset`, so clicking in the code editor highlights the matching
    /// 3D node. Does **not** queue a `pending_caret` — the caret is already
    /// where the user moved it, and writing one back would re-trigger this
    /// path next frame and pingpong forever. Returns `true` when the
    /// selection actually changed; callers can short-circuit when nothing
    /// moved. `None` from the offset lookup (caret in a comment, blank line,
    /// or otherwise outside any span) preserves the existing selection.
    pub fn select_node_at_source_offset(&self, byte_offset: usize, source: &str) -> bool {
        let mut st = self.state.lock().unwrap();
        let Some(scene) = st.scene.clone() else {
            return false;
        };
        // A click on a `use "X" (...)` line maps to the imported root rather
        // than whatever container would otherwise win the deepest-span tiebreak
        // (typically `scene`, since imported nodes' spans live in another
        // file and are skipped). Try this first so the editor click lands a
        // meaningful viewport selection.
        let target = find_use_at_offset(&scene, source, byte_offset)
            .or_else(|| find_deepest_node_at_offset(&scene, byte_offset));
        let Some(target) = target else {
            return false;
        };
        if st.selected.last().copied() == Some(target) {
            return false;
        }
        st.selected.clear();
        st.selected_paths.clear();
        st.selected.push(target);
        if let Some(path) = node_path(&scene, target) {
            st.selected_paths.push(path);
        }
        st.gizmo_drag = None;
        st.pick_cycle = None;
        true
    }

    /// Set the desired selection by stable paths. Live `NodeId`s are cleared
    /// so the inspector doesn't render against stale indices; the next
    /// `set_scene` call resolves the paths back to `NodeId`s once the
    /// recompile lands.
    pub fn set_selected_paths(&self, paths: Vec<SelectionPath>) {
        let mut st = self.state.lock().unwrap();
        st.selected_paths = paths;
        st.selected.clear();
        st.gizmo_drag = None;
    }

    pub fn gizmo_mode(&self) -> crate::gizmo::GizmoMode {
        self.state.lock().unwrap().gizmo_mode
    }

    pub fn set_preview_shader(&self, shader: PreviewShader) {
        self.state.lock().unwrap().preview_shader = shader;
    }

    pub fn set_show_grid(&self, on: bool) {
        self.state.lock().unwrap().show_grid = on;
    }

    pub fn set_show_light_gizmos(&self, on: bool) {
        self.state.lock().unwrap().show_light_gizmos = on;
    }

    pub fn set_show_transform_gizmo(&self, on: bool) {
        self.state.lock().unwrap().show_transform_gizmo = on;
    }

    pub fn set_show_colliders(&self, on: bool) {
        self.state.lock().unwrap().show_colliders = on;
    }

    /// Swap in a fresh environment-lighting preset. The next viewport paint
    /// pulls `state.environment` and forwards its resolved params to the
    /// renderer's sky-probe + key/fill uniforms.
    pub fn set_environment(&self, env: Environment) {
        self.state.lock().unwrap().environment = env;
    }

    pub fn environment(&self) -> Environment {
        self.state.lock().unwrap().environment
    }

    /// Update the shadow-quality preset. The actual GPU-resource resize
    /// happens lazily in the paint callback where a `glow::Context` is in
    /// scope; this just stashes the desired quality. The renderer compares
    /// against its own cached quality each paint and reallocates only when
    /// they diverge.
    pub fn set_shadows(&self, quality: shadows::ShadowQuality) {
        self.state.lock().unwrap().shadows = quality;
    }

    pub fn shadows(&self) -> shadows::ShadowQuality {
        self.state.lock().unwrap().shadows
    }

    /// Cap continuous viewport repaints (animation, cinema, gizmo drag).
    /// `None` = uncapped. The cap is applied by routing the per-frame
    /// repaint request through `request_repaint_after(1 / fps)` instead of
    /// the immediate variant — input-driven paints still fire on demand.
    pub fn set_max_fps(&self, max_fps: Option<u32>) {
        self.state.lock().unwrap().max_fps = max_fps;
    }

    pub fn set_gizmo_mode(&self, mode: crate::gizmo::GizmoMode) {
        let mut st = self.state.lock().unwrap();
        st.gizmo_mode = mode;
        st.gizmo_drag = None;
    }

    pub fn take_pending_edits(&self) -> Vec<PendingEdit> {
        std::mem::take(&mut self.state.lock().unwrap().pending_edits)
    }

    pub fn take_pending_caret(&self) -> Option<usize> {
        self.state.lock().unwrap().pending_caret.take()
    }

    pub fn push_pending_edit(&self, edit: PendingEdit) {
        self.state.lock().unwrap().pending_edits.push(edit);
    }

    pub fn clips_snapshot(&self) -> Vec<ClipSummary> {
        let st = self.state.lock().unwrap();
        st.scene
            .as_ref()
            .map(|s| {
                s.clips
                    .iter()
                    .map(|c| ClipSummary {
                        name: c.name.clone(),
                        duration: c.duration,
                        origin: c.origin.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn active_clips(&self) -> Vec<bool> {
        self.state.lock().unwrap().clip_active.clone()
    }

    pub fn set_clip_active(&self, idx: usize, active: bool) {
        let mut st = self.state.lock().unwrap();
        if idx >= st.clip_active.len() || st.clip_active[idx] == active {
            return;
        }
        st.clip_active[idx] = active;
        st.anim_times[idx] = 0.0;
        st.update_palettes();
    }

    pub fn set_all_clips_active(&self, active: bool) {
        let mut st = self.state.lock().unwrap();
        let mut changed = false;
        let n = st.clip_active.len();
        for i in 0..n {
            if st.clip_active[i] != active {
                st.clip_active[i] = active;
                st.anim_times[i] = 0.0;
                changed = true;
            }
        }
        if changed {
            st.update_palettes();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.state.lock().unwrap().anim_playing
    }

    pub fn set_playing(&self, playing: bool) {
        self.state.lock().unwrap().anim_playing = playing;
    }

    pub fn playback_speed(&self) -> f32 {
        self.state.lock().unwrap().playback_speed
    }

    pub fn set_playback_speed(&self, speed: f32) {
        self.state.lock().unwrap().playback_speed = speed;
    }

    pub fn reset_anim_times(&self) {
        let mut st = self.state.lock().unwrap();
        for t in st.anim_times.iter_mut() {
            *t = 0.0;
        }
        st.update_palettes();
    }

    pub fn anim_times(&self) -> Vec<f32> {
        self.state.lock().unwrap().anim_times.clone()
    }

    pub fn seek_clip(&self, idx: usize, t: f32) {
        let mut st = self.state.lock().unwrap();
        if idx >= st.anim_times.len() {
            return;
        }
        let duration = st
            .scene
            .as_ref()
            .and_then(|s| s.clips.get(idx))
            .map(|c| c.duration)
            .unwrap_or(0.0);
        let clamped = if duration > 0.0 {
            t.rem_euclid(duration)
        } else {
            0.0
        };
        if (st.anim_times[idx] - clamped).abs() < 1e-5 {
            return;
        }
        st.anim_times[idx] = clamped;
        let active = *st.clip_active.get(idx).unwrap_or(&false);
        if active {
            st.update_palettes();
        }
    }

    pub fn frame_view(&self) {
        let mut st = self.state.lock().unwrap();
        Self::frame_camera_to_static(&mut st);
    }

    pub(super) fn frame_camera_to_static(st: &mut ViewerState) {
        let center = st.static_center;
        let radius = st.static_radius.max(0.001);
        st.camera.target = center;
        st.camera.fit_distance = radius * 2.8;
        st.camera.zoom = 1.0;
        st.camera.yaw = std::f32::consts::FRAC_PI_4;
        st.camera.pitch = 0.5;
    }

    pub fn camera_snapshot(&self) -> super::CameraSnapshot {
        self.state.lock().unwrap().camera.snapshot()
    }

    pub fn restore_camera(&self, snap: super::CameraSnapshot) {
        self.state.lock().unwrap().camera.restore(snap);
    }

    pub fn is_cinema_active(&self) -> bool {
        self.state.lock().unwrap().cinema.active
    }

    pub fn cinema_shot_label(&self) -> Option<&'static str> {
        self.state.lock().unwrap().cinema.shot_label()
    }

    /// Toggle cinema mode. On enable, latches the current camera pose and
    /// re-frames against the static bounding sphere so each shot composes
    /// around the model itself. On disable, restores the latched pose.
    ///
    /// `force_play` controls whether animations should be force-started when
    /// cinema activates. Pass `true` to match the previous default (the
    /// model performs while the camera pans); `false` lets the user enable
    /// cinema on a static model without surprise animation playback.
    pub fn set_cinema_active(&self, on: bool, force_play: bool) {
        let mut st = self.state.lock().unwrap();
        if on == st.cinema.active {
            return;
        }
        if on {
            let center = st.static_center;
            let radius = st.static_radius.max(0.001);
            st.camera.target = center;
            st.camera.fit_distance = radius * 2.8;
            // Split the guard's deref into a single &mut ViewerState so the
            // borrow checker can prove `cinema` and `camera` are disjoint.
            let st = &mut *st;
            st.cinema.activate(&st.camera);
            st.gizmo_drag = None;
            if force_play {
                st.anim_playing = true;
            }
        } else if let Some(snap) = st.cinema.deactivate() {
            st.camera.restore(snap);
        }
    }

    /// Queue an offscreen render. The next paint callback consumes the
    /// request, renders each frame at `request.size × request.size` to a
    /// fresh FBO, and writes a `CaptureOutcome` back into the viewer state.
    /// The app drains it via [`Self::take_capture_outcome`] on subsequent
    /// frames. Replaces any earlier in-flight request — the studio wires a
    /// single capture-in-flight slot at the app level so this never collides
    /// in practice.
    pub fn submit_capture(&self, mut request: CaptureRequest) {
        // Re-initialise progress bookkeeping every submission so callers
        // don't have to know about `total` / `written` / `error` — they just
        // hand us the work and we record the denominator here.
        request.total = request.frames.len() as u32;
        request.written = Vec::with_capacity(request.frames.len());
        request.error = None;
        let mut st = self.state.lock().unwrap();
        st.capture_request = Some(request);
        st.capture_outcome = None;
    }

    /// Drain the completed capture only when its kind matches `predicate`.
    /// Used to keep `poll_generate` and the picker's background thumbnail
    /// engine from racing over the single `capture_outcome` slot — each
    /// caller passes a filter that matches only the kinds it owns. Without
    /// this discrimination, whichever caller polls first (currently
    /// `poll_generate`) drains every outcome, including the picker's, and
    /// the picker's thumbnail pipeline stalls after one render.
    pub fn take_capture_outcome_if(
        &self,
        predicate: impl FnOnce(CaptureKind) -> bool,
    ) -> Option<CaptureOutcome> {
        let mut st = self.state.lock().unwrap();
        let matches = st
            .capture_outcome
            .as_ref()
            .map(|o| predicate(o.kind))
            .unwrap_or(false);
        if matches {
            st.capture_outcome.take()
        } else {
            None
        }
    }

    /// Whether a capture request is queued or actively rendering. The app
    /// uses this to gate menu items so the user can't pile up captures.
    pub fn is_capturing(&self) -> bool {
        self.state.lock().unwrap().capture_request.is_some()
    }

    /// Queue an imposter atlas bake. The next paint callback runs the bake
    /// on the live GL context and writes the result to `imposter_outcome`.
    /// Replaces any prior queued request — Studio gates this at the app
    /// level so two callers never collide.
    pub fn submit_imposter_request(&self, request: ImposterRequest) {
        let mut st = self.state.lock().unwrap();
        st.imposter_request = Some(request);
        st.imposter_outcome = None;
    }

    /// Drain the most recent completed imposter bake, if any.
    pub fn take_imposter_outcome(&self) -> Option<ImposterOutcome> {
        self.state.lock().unwrap().imposter_outcome.take()
    }

    /// Whether an imposter request is queued or in-flight.
    pub fn imposter_in_flight(&self) -> bool {
        let st = self.state.lock().unwrap();
        st.imposter_request.is_some() && st.imposter_outcome.is_none()
    }

    /// Enter or leave the viewport imposter-preview mode. When `scene` is
    /// `Some`, the paint callback bakes (or re-bakes) the yaw-grid atlas
    /// off it and renders a billboard quad in its place. When `scene` is
    /// `None` (or `active` is false), the cached overlay texture is freed
    /// next paint and the normal scene draw resumes.
    pub fn set_imposter_view(&self, active: bool, scene: Option<Arc<SceneGraph>>) {
        let mut st = self.state.lock().unwrap();
        st.imposter_view_active = active;
        if active {
            // A fresh scene Arc invalidates any cached atlas — flip dirty
            // so the next paint re-bakes against the latest geometry.
            // Identity-compare the Arc so unrelated repaints (camera
            // orbit, etc.) don't trigger a needless re-bake.
            let same = match (&st.imposter_view_scene, &scene) {
                (Some(prev), Some(next)) => Arc::ptr_eq(prev, next),
                _ => false,
            };
            if !same {
                st.imposter_view_dirty = true;
                st.imposter_view_scene = scene;
            }
        } else {
            // Leaving the mode — drop the scene reference; the texture is
            // freed in the paint callback (needs &gl).
            st.imposter_view_scene = None;
            st.imposter_view_dirty = true;
        }
    }

    /// Snapshot the in-flight capture's progress for the modal: the kind
    /// (so the dialog can title itself "thumbnail" vs "video"), how many
    /// frames have already been written, and the original frame count.
    /// Returns `None` when no capture is queued.
    pub fn capture_progress(&self) -> Option<(CaptureKind, u32, u32)> {
        let st = self.state.lock().unwrap();
        st.capture_request
            .as_ref()
            .map(|r| (r.kind, r.written.len() as u32, r.total))
    }
}
