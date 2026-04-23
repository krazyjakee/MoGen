use std::path::PathBuf;

use eframe::egui;
use glam::{Mat4, Quat, Vec3};
use mogen_core::{NodeId, SceneGraph, Transform};

use super::anim::{apply_animation, world_transforms_from_locals};
use super::camera::OrbitCamera;
use super::flatten::{flatten, flatten_with_worlds, FlatMesh};
use crate::preview_shader::PreviewShader;

/// Shared state between the egui main thread and the render-time paint callback.
#[derive(Default)]
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
    /// Cursor ray at drag start. Used by each per-frame delta calculation
    /// together with the current ray.
    pub start_ray_origin: Vec3,
    pub start_ray_dir: Vec3,
    /// Running delta to apply. Translate: world-units along axis. Rotate:
    /// radians about axis. Scale: multiplicative factor along axis. Already
    /// snap-rounded if the user was holding Ctrl at the last update. The
    /// commit path checks this against an effective-zero threshold to skip
    /// pure-click taps where the cursor never moved off the handle.
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
    /// Inspector widget / simple attr set. Writes `attr=value` on the node,
    /// leaves any other attribute untouched.
    SetAttr {
        node: NodeId,
        attr: String,
        value: String,
    },
    /// Gizmo drag commit. Writes the canonical transform attr (`pos` / `rot` /
    /// `scale`) AND deletes any shadowing shortcut attrs listed in `delete` —
    /// per-axis (`x`/`y`/`z`, `rx`/`ry`/`rz`) or corner-form (`from`/`to`) —
    /// that would otherwise win on recompile and make the drag snap back.
    /// Using a separate variant keeps inspector edits (which must preserve
    /// the author's shorthand) on their own path.
    SetAttrCanonical {
        node: NodeId,
        attr: String,
        value: String,
        delete: Vec<String>,
    },
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
    let worlds = scene.world_transforms();
    let world = worlds.get(selected.0 as usize).copied().unwrap_or(Mat4::IDENTITY);
    let origin = world.w_axis.truncate();
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
    // Apply snap (if Ctrl held) against the start transform so the drag's
    // absolute landing value lands on a milestone regardless of where the
    // drag began. The same snapped delta feeds the preview and the final
    // DSL writeback, so the user's release value matches what they see.
    let new_delta = if snap {
        match drag_ref.mode {
            crate::gizmo::GizmoMode::Translate => {
                let start_axis = match drag_ref.axis {
                    crate::gizmo::Axis::X => drag_ref.start_transform.translation.x,
                    crate::gizmo::Axis::Y => drag_ref.start_transform.translation.y,
                    crate::gizmo::Axis::Z => drag_ref.start_transform.translation.z,
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
    use crate::gizmo::{Axis, GizmoMode};
    // Shortcut/corner-form attrs that the DSL lets override the canonical
    // transform field. The gizmo must strip these on commit or the author's
    // original shortcuts silently defeat the writeback on recompile.
    const POS_SHADOWS: &[&str] = &["x", "y", "z", "from", "to"];
    const ROT_SHADOWS: &[&str] = &["rx", "ry", "rz"];
    match drag.mode {
        GizmoMode::Translate => {
            // Write the full vec3 under `pos=` rather than a lone axis
            // shortcut (`x=`, `y=`, `z=`). Dragging the X handle, then Y,
            // then Z would otherwise smear three redundant attrs across
            // the header — this keeps the round-trip clean and matches
            // how most .mog sources are authored.
            let start = drag.start_transform.translation;
            let delta_vec = drag.axis.unit() * drag.delta;
            let end = start + delta_vec;
            let _ = Axis::X; // keep axis enum import in scope for other arms
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
            // Compose the incremental rotation with the start rotation and
            // decompose back to Euler XYZ in degrees so it round-trips
            // through the existing `rot=[x,y,z]` attr format.
            let q = Quat::from_axis_angle(drag.axis.unit(), drag.delta);
            let final_rot = (q * drag.start_transform.rotation).normalize();
            let (rx, ry, rz) = final_rot.to_euler(glam::EulerRot::XYZ);
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
            let s0 = drag.start_transform.scale;
            let factor = drag.delta.max(0.01);
            let new_scale = match drag.axis {
                Axis::X => Vec3::new(s0.x * factor, s0.y, s0.z),
                Axis::Y => Vec3::new(s0.x, s0.y * factor, s0.z),
                Axis::Z => Vec3::new(s0.x, s0.y, s0.z * factor),
            };
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
    use crate::gizmo::{Axis, GizmoMode};
    let mut t = drag.start_transform;
    match drag.mode {
        GizmoMode::Translate => {
            let delta = drag.axis.unit() * drag.delta;
            t.translation += delta;
        }
        GizmoMode::Rotate => {
            let q = Quat::from_axis_angle(drag.axis.unit(), drag.delta);
            t.rotation = (q * t.rotation).normalize();
        }
        GizmoMode::Scale => {
            let factor = drag.delta.max(0.01);
            let i = drag.axis.index();
            match drag.axis {
                Axis::X => t.scale.x = drag.start_transform.scale.x * factor,
                Axis::Y => t.scale.y = drag.start_transform.scale.y * factor,
                Axis::Z => t.scale.z = drag.start_transform.scale.z * factor,
            }
            let _ = i;
        }
    }
    t
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
