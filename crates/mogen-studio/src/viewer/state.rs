use std::path::PathBuf;

use eframe::egui;
use glam::{Mat4, Quat, Vec3};
use mogen_core::{NodeId, SceneGraph, Span, Transform};

use super::anim::{apply_animation, world_transforms_from_locals};
use super::camera::OrbitCamera;
use super::cinema::CinemaDirector;
use super::flatten::{flatten, flatten_with_worlds, FlatMesh};
use crate::preview_shader::PreviewShader;

/// Shared state between the egui main thread and the render-time paint callback.
pub struct ViewerState {
    pub camera: OrbitCamera,
    pub mesh: FlatMesh,
    pub mesh_dirty: bool,
    pub scene: Option<SceneGraph>,
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
    /// Currently-selected scene node, or `None` for no selection. Transient
    /// UI state — wiped when a compile fails, re-resolved by path across
    /// successful recompiles.
    pub selected: Option<NodeId>,
    /// Path (root → ... → selected) of node names — used to re-resolve the
    /// `NodeId` after a recompile renumbers scene nodes. Kept separate from
    /// `selected` so `set_scene` can refresh the id without the app code
    /// having to track both.
    pub selected_path: Option<Vec<String>>,
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
}

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
pub(super) const TRANSLATE_SNAP_STEP: f32 = 0.25;
/// Snap step for rotate: 15° feels coarse enough to be useful without
/// forcing the user to fight the handle for in-between angles.
pub(super) const ROTATE_SNAP_STEP_DEG: f32 = 15.0;
/// Snap step for scale: 25% increments land on nice factors (0.25, 0.5,
/// 0.75, 1.0, 1.25, …) when Ctrl is held.
pub(super) const SCALE_SNAP_STEP: f32 = 0.25;

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
    /// Set an attribute on the AST declaration at `span`. Used by the gizmo
    /// redirect path to rewrite a connector's `at=` when the user drags a
    /// child whose transform is overwritten by `attach` — there's no
    /// `NodeId` for a connector, so we carry the span directly. `node` is
    /// kept for undo coalescing only (the source node the user clicked).
    SetAttrAtSpan {
        node: NodeId,
        span: Span,
        attr: String,
        value: String,
        delete: Vec<String>,
    },
}

impl Default for ViewerState {
    fn default() -> Self {
        Self {
            camera: Default::default(),
            mesh: Default::default(),
            mesh_dirty: false,
            scene: None,
            base_dir: None,
            clip_active: Vec::new(),
            anim_times: Vec::new(),
            anim_playing: false,
            playback_speed: 1.0,
            static_center: Vec3::ZERO,
            static_radius: 0.0,
            selected: None,
            selected_path: None,
            gizmo_mode: Default::default(),
            gizmo_drag: None,
            pending_edits: Vec::new(),
            pending_caret: None,
            preview_shader: Default::default(),
            cinema: Default::default(),
        }
    }
}

impl ViewerState {
    pub(super) fn any_active(&self) -> bool {
        self.clip_active.iter().any(|&b| b)
    }

    pub(super) fn rebuild_mesh(&mut self) {
        let Some(scene) = &self.scene else {
            return;
        };
        let base_dir = self.base_dir.as_deref();
        let mut locals: Vec<Transform> = scene.nodes.iter().map(|n| n.transform).collect();
        let animating = self.any_active();
        if animating {
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
        let mesh = if animating || self.gizmo_drag.is_some() {
            let worlds = world_transforms_from_locals(scene, &locals);
            flatten_with_worlds(scene, &worlds, base_dir)
        } else {
            flatten(scene, base_dir)
        };
        self.mesh = mesh;
        self.mesh_dirty = true;
    }
}

pub(super) fn aspect_for(rect: egui::Rect) -> f32 {
    (rect.width() / rect.height()).max(0.01)
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
    let node = scene.nodes.get(selected.0 as usize)?;
    if !node.editable {
        return None;
    }
    // Relative placement re-shifts the node's translation every compile. A
    // `pos=` writeback would stack on top of that shift, producing a visible
    // jump-and-snap instead of following the cursor. Refuse the drag here so
    // the render path (which also skips the gizmo handles for these nodes)
    // stays consistent with the input path.
    if node.relative_placed {
        return None;
    }
    // Attach-bound nodes have their transform overwritten on every compile.
    // Translate is redirected to the bound socket's `at=` (handled in
    // commit), but only when the socket has a source span — synthesized
    // default sockets (primitive faces) have no DSL slice to rewrite, so
    // any drag on them would just snap back. Rotate/scale have no clean
    // redirect either. Refuse the drag in those cases so the user sees a
    // no-op gizmo rather than a confusing snap-back.
    if let Some(binding) = node.attach_binding.as_ref() {
        let mode_supported = matches!(st.gizmo_mode, crate::gizmo::GizmoMode::Translate);
        let has_span = scene
            .nodes
            .get(binding.parent.0 as usize)
            .and_then(|p| p.connectors.iter().find(|c| c.name == binding.socket))
            .and_then(|c| c.source_span)
            .is_some();
        if !mode_supported || !has_span {
            return None;
        }
    }
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
    st.rebuild_mesh();
}

/// Resolved redirect target for an attach-bound translate. Only present
/// when the dragged node has an `attach_binding` AND the bound socket has
/// a source span (i.e. it was user-declared, not synthesized from a
/// primitive's faces).
struct AttachRedirect {
    connector_span: Span,
    /// Connector's current `pos` in the parent's local frame — the value
    /// the rewritten `at=` is offset from.
    socket_at: Vec3,
}

fn drag_attach_redirect(st: &ViewerState, drag: &GizmoDrag) -> Option<AttachRedirect> {
    let scene = st.scene.as_ref()?;
    let node = scene.nodes.get(drag.node.0 as usize)?;
    let binding = node.attach_binding.as_ref()?;
    let parent = scene.nodes.get(binding.parent.0 as usize)?;
    let socket = parent.connectors.iter().find(|c| c.name == binding.socket)?;
    Some(AttachRedirect {
        connector_span: socket.source_span?,
        socket_at: socket.pos,
    })
}

pub(super) fn commit_gizmo_drag(st: &mut ViewerState) -> Option<PendingEdit> {
    let drag = st.gizmo_drag.as_ref()?;
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
        return None;
    }
    use crate::gizmo::GizmoMode;
    // Shortcut/corner-form attrs that the DSL lets override the canonical
    // transform field. The gizmo must strip these on commit or the author's
    // original shortcuts silently defeat the writeback on recompile.
    const POS_SHADOWS: &[&str] = &["x", "y", "z", "from", "to"];
    const ROT_SHADOWS: &[&str] = &["rx", "ry", "rz"];
    let final_local = apply_gizmo_drag(drag);
    // Attach-bound nodes have their transform rewritten on every compile.
    // Writing `pos=` on the child would be silently clobbered, so redirect
    // a translate into the bound socket connector's `at=` instead. Rotate
    // and scale have no clean redirect target (the connector carries a
    // dir but not a scale, and rewriting `dir=` mid-rig is a footgun) —
    // refuse them so the user gets a no-op rather than a snap-back.
    let attach_redirect = drag_attach_redirect(st, drag);
    if matches!(drag.mode, GizmoMode::Rotate | GizmoMode::Scale) && attach_redirect.is_some() {
        return None;
    }
    match drag.mode {
        GizmoMode::Translate => {
            if let Some(redirect) = attach_redirect {
                // delta in child-local translation == delta in socket `at`
                // (both live in the parent's local frame and the connector
                // is what fixes the child there). Just shift the connector
                // by the same vector and the child follows on recompile.
                let delta_local = final_local.translation - drag.start_transform.translation;
                let new_at = redirect.socket_at + delta_local;
                return Some(PendingEdit::SetAttrAtSpan {
                    node: drag.node,
                    span: redirect.connector_span,
                    attr: "at".to_string(),
                    value: format!(
                        "[{}, {}, {}]",
                        format_scalar(new_at.x),
                        format_scalar(new_at.y),
                        format_scalar(new_at.z)
                    ),
                    delete: Vec::new(),
                });
            }
            // Write the full vec3 under `pos=` rather than a lone axis
            // shortcut (`x=`, `y=`, `z=`). Dragging the X handle, then Y,
            // then Z would otherwise smear three redundant attrs across
            // the header — this keeps the round-trip clean and matches
            // how most .mog sources are authored. The new value is the
            // node's *local* translation after the world-space drag has
            // been pulled back through the parent inverse, so children of
            // a rotated/scaled parent move along the world axis the user
            // grabbed instead of the parent's tilted axis.
            let end = final_local.translation;
            Some(PendingEdit::SetAttrCanonical {
                node: drag.node,
                attr: "pos".to_string(),
                value: format!(
                    "[{}, {}, {}]",
                    format_scalar(end.x),
                    format_scalar(end.y),
                    format_scalar(end.z)
                ),
                delete: POS_SHADOWS.iter().map(|s| s.to_string()).collect(),
            })
        }
        GizmoMode::Rotate => {
            // `apply_gizmo_drag` already conjugated the world-axis rotation
            // through the parent's world rotation, so `final_local.rotation`
            // is the local-space quaternion to write. Decompose to Euler
            // XYZ degrees for the existing `rot=[x,y,z]` DSL surface.
            let (rx, ry, rz) = final_local.rotation.to_euler(glam::EulerRot::XYZ);
            let value = format!(
                "[{}, {}, {}]",
                format_scalar(rx.to_degrees()),
                format_scalar(ry.to_degrees()),
                format_scalar(rz.to_degrees())
            );
            Some(PendingEdit::SetAttrCanonical {
                node: drag.node,
                attr: "rot".to_string(),
                value,
                delete: ROT_SHADOWS.iter().map(|s| s.to_string()).collect(),
            })
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
            Some(PendingEdit::SetAttrCanonical {
                node: drag.node,
                attr: "scale".to_string(),
                value,
                delete: Vec::new(),
            })
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

pub(super) fn select_by_id(st: &mut ViewerState, id: Option<NodeId>) {
    st.selected = id;
    st.selected_path = match id {
        Some(n) => st.scene.as_ref().and_then(|s| node_path(s, n)),
        None => None,
    };
    // Surface the selected node's source-span start so the editor caret
    // jumps to its declaration.
    st.pending_caret = id
        .and_then(|n| {
            st.scene
                .as_ref()
                .and_then(|s| s.nodes.get(n.0 as usize))
                .and_then(|node| node.source_span)
        })
        .map(|span| span.start);
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

/// Walk from `id` up to a root collecting names in root → ... → node order.
pub(super) fn node_path(scene: &SceneGraph, id: NodeId) -> Option<Vec<String>> {
    if id.0 as usize >= scene.nodes.len() {
        return None;
    }
    let mut out = Vec::new();
    let mut cur = Some(id);
    while let Some(n) = cur {
        let node = &scene.nodes[n.0 as usize];
        out.push(node.name.clone());
        cur = node.parent;
    }
    out.reverse();
    Some(out)
}

/// Re-resolve a saved node path against a freshly-lowered scene. Returns the
/// first node whose parent chain matches — collisions (two siblings with the
/// same name) are a DSL authoring smell, so we don't try to disambiguate.
pub(super) fn resolve_node_path(scene: &SceneGraph, path: &[String]) -> Option<NodeId> {
    if path.is_empty() {
        return None;
    }
    for i in 0..scene.nodes.len() {
        let id = NodeId(i as u32);
        let Some(cur_path) = node_path(scene, id) else {
            continue;
        };
        if cur_path == path {
            return Some(id);
        }
    }
    None
}
