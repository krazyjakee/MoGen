mod anim;
mod camera;
mod cinema;
mod colliders_gl;
pub mod environment;
pub(crate) mod flatten;
mod gizmo_gl;
mod gl_util;
mod grid_gl;
mod lights;
mod lights_gl;
mod renderer;
mod shaders;
pub mod shadows;
mod state;

use std::path::Path;
use std::sync::{Arc, Mutex};

use eframe::egui;
use glam::Mat4;
use mogen_core::{NodeId, SceneGraph};

pub use camera::{CameraSnapshot, OrbitCamera};
pub use environment::Environment;
pub use flatten::{ClipSummary, FlatMesh, FLOATS_PER_VERTEX};
pub use lights::ResolvedLight;
#[allow(unused_imports)]
pub use shadows::ShadowQuality;
pub use state::{
    is_import_wrapper, CaptureFrame, CaptureKind, CaptureOutcome, CaptureRequest, PendingEdit,
};

use crate::preview_shader::PreviewShader;

use renderer::Renderer;
use state::{
    aspect_for, begin_gizmo_drag, commit_gizmo_drag, gizmo_handles_supported, node_path,
    replace_selection, replace_selection_cycling, resolve_node_path, toggle_selection,
    update_gizmo_drag, ViewerState,
};

pub struct Viewer {
    pub state: Arc<Mutex<ViewerState>>,
    pub renderer: Arc<Mutex<Renderer>>,
}

impl Viewer {
    pub fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        Ok(Self {
            state: Arc::new(Mutex::new(ViewerState::default())),
            renderer: Arc::new(Mutex::new(Renderer::new(gl)?)),
        })
    }

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
    pub fn all_selected_paths(&self) -> Vec<Vec<String>> {
        self.state.lock().unwrap().selected_paths.clone()
    }

    /// Set the desired selection by stable paths. Live `NodeId`s are cleared
    /// so the inspector doesn't render against stale indices; the next
    /// `set_scene` call resolves the paths back to `NodeId`s once the
    /// recompile lands.
    pub fn set_selected_paths(&self, paths: Vec<Vec<String>>) {
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

    fn frame_camera_to_static(st: &mut ViewerState) {
        let center = st.static_center;
        let radius = st.static_radius.max(0.001);
        st.camera.target = center;
        st.camera.fit_distance = radius * 2.8;
        st.camera.zoom = 1.0;
        st.camera.yaw = std::f32::consts::FRAC_PI_4;
        st.camera.pitch = 0.5;
    }

    pub fn camera_snapshot(&self) -> CameraSnapshot {
        self.state.lock().unwrap().camera.snapshot()
    }

    pub fn restore_camera(&self, snap: CameraSnapshot) {
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
    /// around the model itself; force-plays animations so the subject moves
    /// while the camera pans. On disable, restores the latched pose.
    pub fn set_cinema_active(&self, on: bool) {
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
            st.anim_playing = true;
        } else if let Some(snap) = st.cinema.deactivate() {
            st.camera.restore(snap);
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        if let Ok(mut r) = self.renderer.lock() {
            r.destroy(gl);
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

    pub fn show(&self, ui: &mut egui::Ui) -> egui::Response {
        let available = ui.available_size();
        let desired = egui::vec2(available.x.max(64.0), available.y.max(64.0));
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());

        let dt = ui.input(|i| i.stable_dt);
        let shift_held = ui.input(|i| i.modifiers.shift);
        let ctrl_held = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
        // `command` is Cmd on macOS, Ctrl on Linux/Windows — the
        // cross-platform "additive selection" modifier. Shift is the second
        // additive modifier (matches Blender/Maya/Finder convention) and
        // stays reserved for camera pan when combined with a drag.
        let cmd_held = ui.input(|i| i.modifiers.command);
        let cursor_now = ui.input(|i| i.pointer.hover_pos());
        let (primary_pressed_raw, press_pos_raw, primary_released_raw) =
            ui.input(|i| {
                (
                    i.pointer.primary_pressed(),
                    i.pointer.press_origin(),
                    i.pointer.primary_released(),
                )
            });
        let primary_pressed_on_widget = primary_pressed_raw
            && press_pos_raw.map(|p| rect.contains(p)).unwrap_or(false);
        let primary_dragging = response.dragged_by(egui::PointerButton::Primary);
        let mut needs_repaint = false;
        let max_fps;
        {
            let mut st = self.state.lock().unwrap();

            // Cinema mode owns the camera: tick the director and skip all
            // user-input handling below (orbit, pan, zoom, gizmo, click-to-
            // select). Animations still advance — the model performs while
            // the camera pans.
            let cinema_active = st.cinema.active;
            if cinema_active {
                // Split the guard so the borrow checker sees `cinema` and
                // `camera` as disjoint fields.
                let st_ref = &mut *st;
                st_ref.cinema.tick(dt, &mut st_ref.camera);
                needs_repaint = true;
            }

            let mut gizmo_handled_primary = false;
            // Shift and Cmd/Ctrl are reserved for additive selection (and
            // shift-drag for pan); never grab a gizmo handle while either
            // is held, otherwise an extend-selection click on a node whose
            // handle happens to project under the cursor would start a drag
            // instead.
            if !cinema_active && primary_pressed_on_widget && !shift_held && !cmd_held {
                if let (Some(cursor), Some(sel)) = (press_pos_raw, st.primary_selected()) {
                    let drag_opt = begin_gizmo_drag(&st, sel, rect, cursor, aspect_for(rect));
                    if std::env::var_os("MOGEN_GIZMO_TRACE").is_some() {
                        eprintln!(
                            "[gizmo] begin mode={:?} sel={} cursor=({:.1},{:.1}) rect=({:.0},{:.0})-({:.0},{:.0}) result={}",
                            st.gizmo_mode,
                            sel.0,
                            cursor.x,
                            cursor.y,
                            rect.min.x,
                            rect.min.y,
                            rect.max.x,
                            rect.max.y,
                            drag_opt.is_some()
                        );
                    }
                    if let Some(drag) = drag_opt {
                        st.gizmo_drag = Some(drag);
                        gizmo_handled_primary = true;
                    }
                }
            }

            let gizmo_in_progress = !cinema_active && st.gizmo_drag.is_some();
            if gizmo_in_progress && primary_dragging {
                if let Some(cursor) = cursor_now {
                    update_gizmo_drag(&mut st, rect, cursor, aspect_for(rect), ctrl_held);
                    needs_repaint = true;
                }
            }

            // Suppress ALL camera input while a gizmo drag is live — pan
            // included, not just orbit. A middle/secondary/Shift-modifier
            // co-press during a gizmo gesture used to steal the camera and
            // the user saw the camera tumble alongside the model, making
            // the edit look like it was rejected.
            let panning = !cinema_active
                && !gizmo_in_progress
                && !gizmo_handled_primary
                && (response.dragged_by(egui::PointerButton::Middle)
                    || response.dragged_by(egui::PointerButton::Secondary)
                    || (shift_held && primary_dragging));
            if panning {
                st.camera.pan(response.drag_delta(), rect.height());
            } else if !cinema_active
                && primary_dragging
                && !gizmo_in_progress
                && !gizmo_handled_primary
            {
                let d = response.drag_delta();
                st.camera.yaw -= d.x * 0.01;
                st.camera.pitch = (st.camera.pitch - d.y * 0.01).clamp(-1.54, 1.54);
            }
            if !cinema_active && response.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    let factor = (1.0 - scroll * 0.0015).clamp(0.5, 1.5);
                    st.camera.zoom = (st.camera.zoom * factor).clamp(0.1, 10.0);
                }
            }

            if primary_released_raw && gizmo_in_progress {
                let edits = commit_gizmo_drag(&mut st);
                if std::env::var_os("MOGEN_GIZMO_TRACE").is_some() {
                    if edits.is_empty() {
                        eprintln!("[gizmo] commit SKIPPED (trivial delta)");
                    } else {
                        for edit in &edits {
                            match edit {
                                PendingEdit::SetAttrCanonical {
                                    node,
                                    attr,
                                    value,
                                    delete,
                                } => eprintln!(
                                    "[gizmo] commit SetAttrCanonical node={} attr={} value={} delete={:?}",
                                    node.0, attr, value, delete
                                ),
                                PendingEdit::DeleteNode { node } => eprintln!(
                                    "[gizmo] commit DeleteNode node={}",
                                    node.0
                                ),
                            }
                        }
                    }
                }
                for edit in edits {
                    st.pending_edits.push(edit);
                }
                // Clear the preview handle but DO NOT rebuild the mesh here.
                // The immediately-following `drain_viewport_edits` →
                // `compile_active` → `set_scene` path will rebuild against the
                // freshly-compiled scene. Rebuilding now would paint one frame
                // from the stale (pre-edit) scene without the preview —
                // exactly the snap-back the previous fix attempts chased.
                st.gizmo_drag = None;
                // A drag commit reshapes the scene; the recorded leaf NodeId
                // could land on a different node after the recompile, so the
                // next click should restart the drill at depth 0.
                st.pick_cycle = None;
                needs_repaint = true;
            }

            if !cinema_active && response.clicked() && !gizmo_in_progress {
                if let Some(cursor) = cursor_now {
                    // Resolve lights once per click, not per repaint, so the
                    // billboard halo test sees exactly the same world poses
                    // the renderer drew this frame. Cheap (≤ MAX_LIGHTS).
                    let lights = st.resolve_lights();
                    let hit = crate::pick::pick_node_or_light(
                        &st.camera,
                        rect,
                        cursor,
                        &st.mesh,
                        &lights,
                    );
                    let additive = shift_held || cmd_held;
                    match (additive, hit) {
                        // Plain click on a node → Figma-style drill-down.
                        // First click selects the editable wrapper / outer
                        // group (whatever `redirect_pick` returns). A
                        // second click at the same screen point on the
                        // same hit advances one ancestor closer to the
                        // leaf, until the leaf is reached or the cycle
                        // bumps into an imported subtree boundary.
                        (false, Some(id)) => {
                            replace_selection_cycling(&mut st, id, cursor);
                            needs_repaint = true;
                        }
                        (false, None) => {
                            replace_selection(&mut st, None);
                            st.pick_cycle = None;
                        }
                        // Shift/cmd-click on a node → toggle membership. Empty
                        // space with a modifier is intentionally a no-op:
                        // shift-drag is camera pan, and a shift-click that
                        // misses the model is just the start of a pan that
                        // didn't move — wiping the selection there would feel
                        // like a bug.
                        (true, Some(id)) => {
                            toggle_selection(&mut st, id);
                            st.pick_cycle = None;
                            needs_repaint = true;
                        }
                        (true, None) => {}
                    }
                }
            }

            // Esc deselects when the viewport is hovered. Gated on hover so
            // pressing Esc inside the editor / inspector / spotlight doesn't
            // wipe the selection out from under the user. `consume_key` so
            // the keypress doesn't also trigger any downstream listeners.
            if !cinema_active
                && !gizmo_in_progress
                && !st.selected.is_empty()
                && response.hovered()
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                replace_selection(&mut st, None);
                st.pick_cycle = None;
                needs_repaint = true;
            }

            // Backspace / Delete removes the selected node when the viewport
            // is hovered. Same hover gate as Esc so the keypress doesn't fire
            // through from the editor or inspector. The actual source mutation
            // and undo bookkeeping happen in `drain_viewport_edits`.
            if !cinema_active
                && !gizmo_in_progress
                && response.hovered()
                && !st.selected.is_empty()
            {
                let pressed = ui.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                        || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
                });
                if pressed {
                    // One PendingEdit per selected node — `drain_viewport_edits`
                    // resolves spans and applies them right-to-left so the
                    // multi-delete batch leaves the source valid even when
                    // the selection mixes a parent and one of its children.
                    let nodes: Vec<NodeId> = st.selected.clone();
                    for node in nodes {
                        st.pending_edits.push(PendingEdit::DeleteNode { node });
                    }
                    needs_repaint = true;
                }
            }

            if st.anim_playing && st.any_active() {
                let speed = st.playback_speed;
                let scaled_dt = dt * speed;
                let mut advanced = false;
                let n = st.clip_active.len();
                for i in 0..n {
                    if !st.clip_active[i] {
                        continue;
                    }
                    let duration = st
                        .scene
                        .as_ref()
                        .and_then(|s| s.clips.get(i))
                        .map(|c| c.duration)
                        .unwrap_or(0.0);
                    if duration > 0.0 && scaled_dt != 0.0 {
                        st.anim_times[i] = (st.anim_times[i] + scaled_dt).rem_euclid(duration);
                        advanced = true;
                    }
                }
                if advanced {
                    st.update_palettes();
                    needs_repaint = true;
                }
            }
            max_fps = st.max_fps;
        }
        if needs_repaint {
            // Continuous-repaint cases (cinema pan, animation, gizmo drag) go
            // through `request_repaint_after` when the user has set a cap so
            // the loop can't fire sooner than `1 / fps`. Without a cap egui's
            // immediate variant defers to vsync as before.
            match max_fps {
                Some(fps) if fps > 0 => {
                    let dt = std::time::Duration::from_secs_f32(1.0 / fps as f32);
                    ui.ctx().request_repaint_after(dt);
                }
                _ => ui.ctx().request_repaint(),
            }
        }

        let aspect = (rect.width() / rect.height()).max(0.01);
        let viewport_height = rect.height();
        let state_for_paint = self.state.clone();
        let renderer_for_paint = self.renderer.clone();

        let cb = egui_glow::CallbackFn::new(move |_info, painter| {
            let gl = painter.gl();
            let mut st = state_for_paint.lock().unwrap();
            let mut rr = renderer_for_paint.lock().unwrap();
            if st.mesh_dirty {
                rr.upload(gl, &st.mesh);
                st.mesh_dirty = false;
                // The VBO upload also refreshes the palette cache, so any
                // pending palette-only update is now redundant.
                st.palettes_dirty = false;
                rr.evict_unused_textures(gl);
            } else if st.palettes_dirty {
                rr.upload_palettes(&st.mesh.palettes);
                st.palettes_dirty = false;
            }
            // Service any queued offscreen capture before the on-screen draw.
            // Doing it first keeps the path independent: the on-screen pass
            // restores state egui_glow expects, and the capture path restores
            // the bound FBO + viewport itself, so neither leaks into the
            // other. Only one frame is processed per paint so the UI thread
            // gets to redraw between renders (otherwise a 180-frame video
            // freezes the window for the whole encode).
            if st.capture_request.is_some() {
                process_capture_step(&mut rr, gl, &mut st);
            }
            let viewproj = st.camera.view_proj(aspect);
            let eye = st.camera.eye();
            rr.set_preview(
                st.preview_shader.shader_mode(),
                st.preview_shader.wants_wireframe(),
            );
            // Hand the renderer the active environment-lighting preset's
            // resolved params each paint. Cheap (a struct copy) and lets the
            // user swap presets from the overlay without forcing a recompile.
            rr.set_environment(st.environment.params());
            // Sync shadow quality lazily: UI clicks only stash the desired
            // value on `state.shadows` because they have no `glow::Context`
            // in scope, so the actual depth-atlas reallocation happens here.
            // No-op when quality is unchanged across paints (the common
            // case).
            rr.set_shadow_quality(gl, st.shadows);
            // Forward the static-pose AABB so the shadow pre-pass can size
            // its directional ortho frustum and the spot/point far planes
            // without a borrow back into the viewer state.
            rr.set_scene_aabb(st.static_center, st.static_radius);
            // Resolve DSL `light` nodes against the live (animation- and
            // drag-modulated) world transforms so a light parented to a
            // moving rig follows it. With no scene loaded, hand back an empty
            // slice — the FS falls back to its built-in key/fill rig.
            let light_list = st.resolve_lights();
            rr.set_lights(&light_list);
            rr.draw(gl, viewproj, eye);
            // Cinema mode hides the grid + gizmo handles so the framing
            // reads as a clean presentation rather than an editor view.
            if !st.cinema.active {
                if st.show_grid {
                    rr.draw_grid(gl, viewproj, eye);
                }
                // Light overlays sit between the grid and the transform
                // gizmo: occluded by real geometry (depth-test on) but
                // drawn underneath the always-on-top transform handles so
                // selection markers don't fight for the same screen pixels.
                if st.show_light_gizmos {
                    rr.draw_lights_overlay(gl, viewproj, eye, viewport_height, &st.selected);
                }
                if st.show_colliders {
                    if let Some(scene) = st.scene.as_ref() {
                        let worlds = scene.world_transforms();
                        let instances = colliders_gl::collect(scene, &worlds, &st.selected);
                        rr.draw_colliders_overlay(gl, viewproj, &instances);
                    }
                }
                if let (true, Some(sel), Some(scene)) =
                    (st.show_transform_gizmo, st.primary_selected(), st.scene.as_ref())
                {
                    // Single source of truth shared with `begin_gizmo_drag`:
                    // skip drawing for non-editable / relative-placed nodes,
                    // and for attach-bound nodes whose current mode has no
                    // writeback path (rotate/scale always; translate when the
                    // socket has no source span). Drawing handles the input
                    // layer would refuse just lets the user grab a dead
                    // affordance and watch the camera orbit instead.
                    if gizmo_handles_supported(scene, sel, st.gizmo_mode) {
                        let worlds = scene.world_transforms();
                        let base_world = worlds
                            .get(sel.0 as usize)
                            .copied()
                            .unwrap_or(Mat4::IDENTITY);
                        let origin = base_world.w_axis.truncate();
                        let scale = crate::gizmo::handle_scale(origin, eye, viewport_height);
                        rr.draw_gizmo(gl, viewproj, origin, scale, st.gizmo_mode);
                    }
                }
            }
        });

        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(cb),
        });

        response
    }
}


/// Render one pending frame and hand the pixels to a background encoder.
/// Runs inside the paint callback because that's where we have access to
/// a `glow::Context`; the renderer's `render_to_pixels` already restores
/// the bound FBO + viewport before returning, so this never leaks into
/// egui's draw state. PNG encoding + disk I/O happen on the
/// [`state::EncodePool`] worker threads so the GL thread can render the
/// next frame as soon as `glReadPixels` returns instead of blocking on
/// deflate.
fn process_capture_step(
    rr: &mut renderer::Renderer,
    gl: &glow::Context,
    st: &mut state::ViewerState,
) {
    // Phase 1: drain whatever the encoder pool has finished since last
    // paint. Each completed encode either contributes a path to `written`
    // or sets the request's first-fatal-error slot.
    drain_encode_results(st);

    // Phase 2: decide whether to finalise. We can only finalise once
    // `frames` is drained AND there are no encodes still in flight —
    // otherwise the outcome's `frame_paths` would be missing PNGs that
    // workers are still writing.
    let in_flight = st.encode_pool.as_ref().map(|p| p.in_flight).unwrap_or(0);
    let (frames_done, errored) = match st.capture_request.as_ref() {
        Some(req) => (req.frames.is_empty(), req.error.is_some()),
        None => return,
    };
    if (frames_done || errored) && in_flight == 0 {
        // Drop the pool first: workers exit as soon as `job_tx` closes,
        // and dropping here means a fresh capture starts with a fresh
        // pool rather than reusing one that's already been signalled.
        st.encode_pool = None;
        let req = st.capture_request.take().expect("checked above");
        st.capture_outcome = Some(CaptureOutcome {
            kind: req.kind,
            frame_paths: req.written,
            error: req.error,
        });
        return;
    }
    if errored {
        // Don't queue any more renders once a fatal error is recorded;
        // just keep paint cycles ticking so phase 1 can drain whatever
        // encodes were already in flight when the error fired.
        return;
    }
    if frames_done {
        // Frames all submitted but encodes still pending — nothing to do
        // on the GL side this paint, just wait for the pool to catch up.
        return;
    }

    // Phase 3: render the next frame. The borrow scoping here mirrors the
    // pre-async version: pull the per-frame inputs out of `req` in a
    // narrow scope so we can call `st.update_palettes()` later without
    // holding two mutable borrows.
    let (size, bg, kind, frame) = {
        let req = st.capture_request.as_mut().expect("checked above");
        let f = req.frames.remove(0);
        (req.size, req.bg, req.kind, f)
    };

    let center = st.static_center;
    // Floor on the framing radius so a one-vertex / empty scene still picks
    // a sane orbit distance — without this, `radius * 2.8` collapses to 0
    // and the camera ends up inside the model.
    let radius = st.static_radius.max(0.001);
    let cam = camera::OrbitCamera {
        yaw: frame.yaw,
        pitch: frame.pitch,
        fit_distance: radius * 2.8,
        zoom: 1.0,
        target: center,
    };
    let viewproj = cam.view_proj(1.0);
    let eye = cam.eye();
    let frame_time = frame.time;
    // Video frames want the animation sampled at `frame.time` so the encoded
    // mp4 plays clips back across the rotation. Thumbnails ignore time and
    // capture whatever pose is currently visible.
    let anim_override = kind == CaptureKind::Video
        && st.any_active()
        && st
            .scene
            .as_ref()
            .map(|s| !s.clips.is_empty())
            .unwrap_or(false);
    let saved_anim_times = if anim_override {
        let snapshot = st.anim_times.clone();
        // Collect durations up front so we can index `st.anim_times` mutably
        // without holding a `&Scene` borrow across the loop.
        let durations: Vec<f32> = st
            .scene
            .as_ref()
            .map(|s| s.clips.iter().map(|c| c.duration).collect())
            .unwrap_or_default();
        for i in 0..st.clip_active.len() {
            if !st.clip_active[i] {
                continue;
            }
            let duration = durations.get(i).copied().unwrap_or(0.0);
            if duration > 0.0 {
                st.anim_times[i] = frame_time.rem_euclid(duration);
            }
        }
        st.update_palettes();
        rr.upload_palettes(&st.mesh.palettes);
        st.palettes_dirty = false;
        Some(snapshot)
    } else {
        None
    };
    let render_result = rr.render_to_pixels(gl, size, viewproj, eye, bg);
    // Restore palettes before touching anything else so an on-screen draw
    // that follows in this same paint callback matches the user's pose.
    if let Some(snapshot) = saved_anim_times {
        st.anim_times = snapshot;
        st.update_palettes();
        rr.upload_palettes(&st.mesh.palettes);
        st.palettes_dirty = false;
    }
    match render_result {
        Ok(pixels) => {
            // Lazy-init the pool so a never-captured studio session never
            // pays for the worker threads.
            let pool = st
                .encode_pool
                .get_or_insert_with(state::EncodePool::new);
            pool.submit(pixels, size, frame.path);
        }
        Err(e) => {
            if let Some(req) = st.capture_request.as_mut() {
                req.error = Some(format!("render: {e}"));
            }
        }
    }
}

/// Drain everything the encoder pool has produced since last paint into
/// the live `CaptureRequest`. Successful encodes append to `written`;
/// the first failure latches into `error` and short-circuits future
/// frames in the next `process_capture_step` call.
fn drain_encode_results(st: &mut state::ViewerState) {
    let Some(pool) = st.encode_pool.as_mut() else {
        return;
    };
    while let Ok((path, res)) = pool.result_rx.try_recv() {
        // Underflow guard: in-flight should always match the number of
        // outstanding sends, but a stray send from a dropped-and-recreated
        // pool would otherwise wrap to usize::MAX and freeze finalisation.
        pool.in_flight = pool.in_flight.saturating_sub(1);
        let Some(req) = st.capture_request.as_mut() else {
            continue;
        };
        match res {
            Ok(()) => req.written.push(path),
            Err(e) => {
                if req.error.is_none() {
                    req.error = Some(e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
