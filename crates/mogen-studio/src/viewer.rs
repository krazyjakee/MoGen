mod anim;
mod camera;
mod cinema;
pub(crate) mod flatten;
mod gizmo_gl;
mod gl_util;
mod grid_gl;
mod renderer;
mod shaders;
mod state;

use std::path::Path;
use std::sync::{Arc, Mutex};

use eframe::egui;
use glam::Mat4;
use mogen_core::{NodeId, SceneGraph};

pub use camera::{CameraSnapshot, OrbitCamera};
pub use flatten::{ClipSummary, FlatMesh, FLOATS_PER_VERTEX};
pub use state::PendingEdit;

use crate::preview_shader::PreviewShader;

use renderer::Renderer;
use state::{
    aspect_for, begin_gizmo_drag, commit_gizmo_drag, node_path, resolve_node_path, select_by_id,
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

    pub fn set_scene(&self, scene: &SceneGraph, base_dir: Option<&Path>, fit_camera: bool) {
        // Fit the camera using the static (unanimated) pose so the framing
        // stays stable across animation frames — using an animated mesh would
        // make the camera jump as the bounding box swings.
        let base_mesh = flatten::flatten(scene, base_dir);
        let mut st = self.state.lock().unwrap();
        let viewer_was_empty = st.scene.is_none();
        // Camera fit is caller-driven: the App knows whether this is the first
        // time a given file's scene is being shown. Subsequent compiles
        // (gizmo release, keystroke debounce) pass `false` so `camera.target` /
        // `camera.fit_distance` stay put — recentering on every compile would
        // make it look like the user's edit was rejected. The Frame button
        // (`frame_view`) re-fits on demand using `static_center` /
        // `static_radius` below.
        if fit_camera {
            st.camera.fit(&base_mesh);
            st.camera.zoom = 1.0;
        }
        st.static_center = base_mesh.center;
        st.static_radius = base_mesh.radius;
        st.base_dir = base_dir.map(|p| p.to_path_buf());
        st.selected = match &st.selected_path {
            Some(path) => resolve_node_path(scene, path),
            None => None,
        };
        st.gizmo_drag = None;

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

        let mut clip_active: Vec<bool> = vec![false; scene.clips.len()];
        if viewer_was_empty {
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
        st.scene = Some(scene.clone());
        st.rebuild_mesh();
    }

    pub fn clear(&self) {
        let mut st = self.state.lock().unwrap();
        st.mesh = FlatMesh::default();
        st.mesh_dirty = true;
        st.scene = None;
        st.base_dir = None;
        st.clip_active.clear();
        st.anim_times.clear();
        st.selected = None;
        st.selected_path = None;
        st.gizmo_drag = None;
    }

    pub fn set_selection(&self, id: Option<NodeId>) {
        let mut st = self.state.lock().unwrap();
        st.selected = id;
        st.selected_path = id.and_then(|n| {
            st.scene.as_ref().and_then(|s| node_path(s, n))
        });
        st.gizmo_drag = None;
    }

    pub fn selection(&self) -> Option<NodeId> {
        self.state.lock().unwrap().selected
    }

    /// Stable name-path of the current selection (`["root", "torso", "arm_l"]`),
    /// used by the undo stack to capture / restore selection across recompiles
    /// when raw `NodeId` indices may have shifted.
    pub fn selected_path(&self) -> Option<Vec<String>> {
        self.state.lock().unwrap().selected_path.clone()
    }

    /// Set the desired selection by stable path. The live `selected` NodeId
    /// is cleared so the inspector doesn't render against a stale index;
    /// the next `set_scene` call resolves the path back to a NodeId once the
    /// recompile lands.
    pub fn set_selected_path(&self, path: Option<Vec<String>>) {
        let mut st = self.state.lock().unwrap();
        st.selected_path = path;
        st.selected = None;
        st.gizmo_drag = None;
    }

    pub fn gizmo_mode(&self) -> crate::gizmo::GizmoMode {
        self.state.lock().unwrap().gizmo_mode
    }

    pub fn set_preview_shader(&self, shader: PreviewShader) {
        self.state.lock().unwrap().preview_shader = shader;
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
        st.rebuild_mesh();
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
            st.rebuild_mesh();
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
        st.rebuild_mesh();
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
            st.rebuild_mesh();
        }
    }

    pub fn frame_view(&self) {
        let mut st = self.state.lock().unwrap();
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
        if let Ok(r) = self.renderer.lock() {
            r.destroy(gl);
        }
    }

    pub fn show(&self, ui: &mut egui::Ui) -> egui::Response {
        let available = ui.available_size();
        let desired = egui::vec2(available.x.max(64.0), available.y.max(64.0));
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());

        let dt = ui.input(|i| i.stable_dt);
        let shift_held = ui.input(|i| i.modifiers.shift);
        let ctrl_held = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
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
            if !cinema_active && primary_pressed_on_widget && !shift_held {
                if let (Some(cursor), Some(sel)) = (press_pos_raw, st.selected) {
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
                let maybe_edit = commit_gizmo_drag(&mut st);
                if std::env::var_os("MOGEN_GIZMO_TRACE").is_some() {
                    match &maybe_edit {
                        Some(PendingEdit::SetAttrCanonical {
                            node,
                            attr,
                            value,
                            delete,
                        }) => eprintln!(
                            "[gizmo] commit SetAttrCanonical node={} attr={} value={} delete={:?}",
                            node.0, attr, value, delete
                        ),
                        Some(PendingEdit::SetAttrAtSpan {
                            node,
                            span,
                            attr,
                            value,
                            delete,
                        }) => eprintln!(
                            "[gizmo] commit SetAttrAtSpan node={} span={:?} attr={} value={} delete={:?}",
                            node.0, span, attr, value, delete
                        ),
                        None => eprintln!("[gizmo] commit SKIPPED (trivial delta)"),
                    }
                }
                if let Some(edit) = maybe_edit {
                    st.pending_edits.push(edit);
                }
                // Clear the preview handle but DO NOT rebuild the mesh here.
                // The immediately-following `drain_viewport_edits` →
                // `compile_active` → `set_scene` path will rebuild against the
                // freshly-compiled scene. Rebuilding now would paint one frame
                // from the stale (pre-edit) scene without the preview —
                // exactly the snap-back the previous fix attempts chased.
                st.gizmo_drag = None;
                needs_repaint = true;
            }

            if !cinema_active && response.clicked() && !gizmo_in_progress {
                if let Some(cursor) = cursor_now {
                    if let Some(id) = crate::pick::pick_node(
                        &st.camera,
                        rect,
                        cursor,
                        &st.mesh,
                    ) {
                        select_by_id(&mut st, Some(id));
                        needs_repaint = true;
                    } else {
                        select_by_id(&mut st, None);
                    }
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
                    st.rebuild_mesh();
                    needs_repaint = true;
                }
            }
        }
        if needs_repaint {
            ui.ctx().request_repaint();
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
                rr.evict_unused_textures(gl);
            }
            let viewproj = st.camera.view_proj(aspect);
            let eye = st.camera.eye();
            rr.set_preview(
                st.preview_shader.shader_mode(),
                st.preview_shader.wants_wireframe(),
            );
            rr.draw(gl, viewproj, eye);
            // Cinema mode hides the grid + gizmo handles so the framing
            // reads as a clean presentation rather than an editor view.
            if !st.cinema.active {
                rr.draw_grid(gl, viewproj, eye);
                if let (Some(sel), Some(scene)) = (st.selected, st.scene.as_ref()) {
                    if let Some(node) = scene.nodes.get(sel.0 as usize) {
                        // Skip gizmo handles for non-editable (replicator/CSG)
                        // nodes AND for relative-placed nodes: both have derived
                        // transforms that a direct writeback can't change
                        // coherently. `begin_gizmo_drag` mirrors both gates.
                        if node.editable && !node.relative_placed {
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
            }
        });

        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(cb),
        });

        response
    }
}


#[cfg(test)]
mod tests;
