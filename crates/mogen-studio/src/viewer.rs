mod anim;
mod api;
mod camera;
mod cinema;
mod colliders_gl;
pub mod environment;
pub(crate) mod flatten;
mod gizmo_gl;
mod gl_util;
mod grid_gl;
mod imposter_gl;
mod lights;
mod lights_gl;
mod paint_jobs;
mod renderer;
mod shaders;
pub mod shadows;
pub(crate) mod state;

use std::sync::{Arc, Mutex};

use eframe::egui;
use glam::Mat4;
use mogen_core::NodeId;

pub use camera::{CameraSnapshot, OrbitCamera};
pub use environment::Environment;
pub use flatten::{ClipSummary, FlatMesh, FLOATS_PER_VERTEX};
pub use lights::ResolvedLight;
#[allow(unused_imports)]
pub use shadows::ShadowQuality;
pub use state::{
    is_import_wrapper, CaptureFrame, CaptureKind, CaptureOutcome, CaptureRequest, ImposterOutcome,
    ImposterRequest, PendingEdit, SelectionPath,
};

use renderer::Renderer;
use state::{
    aspect_for, begin_gizmo_drag, commit_gizmo_drag, gizmo_handles_supported,
    replace_selection, replace_selection_cycling, toggle_selection, update_gizmo_drag,
    ViewerState,
};

pub struct Viewer {
    pub state: Arc<Mutex<ViewerState>>,
    pub renderer: Arc<Mutex<Renderer>>,
    /// Monotonic clock anchored at viewer construction. Forwarded to the
    /// renderer each paint as `u_time` so per-material shaders (water) get a
    /// stable, wraparound-free seconds counter without depending on the
    /// system wall clock.
    start: std::time::Instant,
}

impl Viewer {
    pub fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        Ok(Self {
            state: Arc::new(Mutex::new(ViewerState::default())),
            renderer: Arc::new(Mutex::new(Renderer::new(gl)?)),
            start: std::time::Instant::now(),
        })
    }

    pub fn destroy(&self, gl: &glow::Context) {
        if let Ok(mut r) = self.renderer.lock() {
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
                                PendingEdit::SetAttrAtSpan {
                                    span,
                                    attr,
                                    value,
                                    delete,
                                } => eprintln!(
                                    "[gizmo] commit SetAttrAtSpan span={:?} attr={} value={} delete={:?}",
                                    span, attr, value, delete
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
            // is hovered AND no widget holds keyboard focus — otherwise
            // `consume_key` snatches the Backspace out of a focused TextEdit
            // (inspector field, spotlight, etc.) just because the cursor
            // happens to be over the 3D view.
            if !cinema_active
                && !gizmo_in_progress
                && response.hovered()
                && !st.selected.is_empty()
                && ui.memory(|m| m.focused().is_none())
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
            // Per-material shaders that animate (water) need the viewport to
            // keep repainting even when no clips are active, otherwise the
            // waves freeze.
            if st.mesh.has_animated_shader() {
                needs_repaint = true;
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
        // Snapshot the monotonic clock outside the move closure so the GL
        // thread doesn't need access to `self`. `as_secs_f32` rolls over
        // around 2^24 seconds (~6 months) — well past any sane viewport
        // session, so it stays a clean monotonic float for water shaders.
        let frame_time = self.start.elapsed().as_secs_f32();

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
            // Push the frame clock before the capture branch so a still
            // capture of a water material samples the same `u_time` the
            // viewport just used (otherwise the capture would render with
            // the previous frame's clock).
            rr.set_frame_time(frame_time);
            if st.capture_request.is_some() {
                paint_jobs::process_capture_step(&mut rr, gl, &mut st);
            }
            if st.imposter_request.is_some() {
                paint_jobs::process_imposter_step(gl, &mut st);
            }
            // Free any cached billboard atlas when the user leaves
            // imposter view, and re-bake when entering / when the
            // source scene has changed since the last bake.
            if !st.imposter_view_active {
                if let Some(overlay) = st.imposter_view_overlay.take() {
                    renderer::Renderer::destroy_imposter_texture(gl, overlay.texture);
                }
                st.imposter_view_dirty = false;
            } else if st.imposter_view_dirty {
                paint_jobs::process_imposter_view_bake(gl, &mut st);
                st.imposter_view_dirty = false;
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

            // Imposter preview mode swaps the main scene draw for a single
            // billboard quad sampled from the baked atlas. The grid still
            // draws so the user has a ground reference while orbiting; all
            // editor overlays (gizmos / lights / colliders) are skipped
            // because they don't apply to a billboard preview.
            if st.imposter_view_active {
                unsafe {
                    use glow::HasContext as _;
                    gl.disable(glow::SCISSOR_TEST);
                    gl.clear_depth_f32(1.0);
                    gl.clear(glow::DEPTH_BUFFER_BIT);
                }
                if !st.cinema.active && st.show_grid {
                    rr.draw_grid(gl, viewproj, eye);
                }
                if let Some(overlay) = st.imposter_view_overlay.as_ref() {
                    let center = glam::Vec3::new(
                        overlay.center[0],
                        overlay.center[1],
                        overlay.center[2],
                    );
                    rr.draw_imposter(
                        gl,
                        viewproj,
                        eye,
                        center,
                        overlay.half_width,
                        overlay.half_height,
                        overlay.view_count,
                        overlay.uv_y_top,
                        overlay.uv_y_bottom,
                        overlay.texture,
                    );
                }
                return;
            }

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
                        rr.draw_colliders_overlay(gl, viewproj, &instances, &st.selected);
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
                    if gizmo_handles_supported(scene, &st.clip_active, sel, st.gizmo_mode) {
                        // Live-pose worlds (clips + drag overlay) so the
                        // handle origin tracks what the user actually sees.
                        // For static rigs this collapses to rest-pose
                        // worlds, so the legacy non-animated path is
                        // unaffected.
                        let worlds = st.live_worlds();
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

#[cfg(test)]
mod tests;
