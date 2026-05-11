//! Viewport gizmo drag state, snap helpers, and the begin / update /
//! commit pipeline that turns a cursor drag into a [`PendingEdit`] the
//! app can splice back into the DSL source.

use eframe::egui;
use glam::{Mat4, Quat, Vec3};
use mogen_core::{NodeId, SceneGraph, Span, TrackProperty, Transform};

use super::super::anim::world_transforms_from_locals;
use super::selection::is_import_wrapper;
use super::ViewerState;

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
    /// When set, the drag is editing a constant track in the active source
    /// instead of the joint's rest pose. `start_transform` captures the
    /// animated pose at drag start so the preview composes off the
    /// currently-visible pose, and `commit_gizmo_drag` writes back to the
    /// track header at `binding.span` rather than synthesising a `pos=` /
    /// `rot=` on the bone.
    pub track_binding: Option<TrackBinding>,
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
pub(crate) fn snap_translate_delta(axis_start: f32, raw_delta: f32) -> f32 {
    let step = TRANSLATE_SNAP_STEP;
    let absolute = axis_start + raw_delta;
    let snapped = (absolute / step).round() * step;
    snapped - axis_start
}

/// Round an incremental rotation (in radians) to the nearest snap step.
pub(crate) fn snap_rotate_delta(raw_delta: f32) -> f32 {
    let step = ROTATE_SNAP_STEP_DEG.to_radians();
    (raw_delta / step).round() * step
}

/// Quantize a scale factor to clean 25% ticks and keep it well above zero
/// so the commit math never produces a degenerate scale.
pub(crate) fn snap_scale_factor(factor: f32) -> f32 {
    let step = SCALE_SNAP_STEP;
    ((factor / step).round() * step).max(SCALE_SNAP_STEP)
}

/// Convert an egui viewport rect into the aspect ratio the perspective
/// matrix expects, clamping near-zero heights so a degenerate layout pass
/// can't produce a NaN projection.
pub(crate) fn aspect_for(rect: egui::Rect) -> f32 {
    (rect.width() / rect.height()).max(0.01)
}

/// Edit intent the viewport has produced this frame. The app polls this via
/// [`super::super::Viewer::take_pending_edits`] and, if present, maps it
/// through the span-aware text mutator in `edit.rs` to rewrite the `.mog`
/// source.
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
    /// Set an attribute on whichever AST node carries `span`, regardless of
    /// SceneGraph identity. Used for non-`SceneNode` headers — currently only
    /// `track` headers, where the gizmo writes back `axis=`/`from=`/`to=`
    /// onto the originating clip's track. Drains identically to
    /// `SetAttrCanonical` (delete shadows, then `set_attr`) but bypasses the
    /// node-span lookup.
    SetAttrAtSpan {
        span: Span,
        attr: String,
        value: String,
        delete: Vec<String>,
    },
    /// Remove the node entirely from the source. Emitted by the viewport
    /// Backspace/Delete shortcut; the drain looks up the node's source span
    /// and splices it out via `edit::delete_node`.
    DeleteNode { node: NodeId },
}

/// Snapshot of a constant track that the gizmo is editing in place of the
/// joint's rest pose. Captured at drag start so the writeback can splice
/// back into the same `track` header even if a recompile renumbers tracks.
#[derive(Clone, Debug)]
pub struct TrackBinding {
    /// Source span of the `track "name" (...)` header in the active source.
    pub span: Span,
    /// Property the track drives — picks which DSL shorthand the gizmo
    /// updates (rotation: `axis=`/`from=`/`to=`, translation: same shape but
    /// `from`/`to` are meters).
    pub property: TrackProperty,
}

/// Map a [`crate::gizmo::GizmoMode`] to the [`TrackProperty`] it should drive
/// when the selected node is bound to a constant track. Scale tracks are
/// out of scope (see character.mog discussion) — the user opted for rotate
/// + translate only, so Scale returns `None` and the gizmo falls back to
/// the joint-rest writeback.
pub(crate) fn track_property_for_gizmo(mode: crate::gizmo::GizmoMode) -> Option<TrackProperty> {
    match mode {
        crate::gizmo::GizmoMode::Rotate => Some(TrackProperty::Rotation),
        crate::gizmo::GizmoMode::Translate => Some(TrackProperty::Translation),
        crate::gizmo::GizmoMode::Scale => None,
    }
}

/// If an active clip authored in the active source contains a constant track
/// driving `node` for `property`, return its [`TrackBinding`] (source span +
/// property). The gizmo redirects writeback onto this span instead of the
/// joint's rest-pose `pos=`/`rot=` so a recompile reproduces the dragged
/// pose.
///
/// Excluded:
/// - inactive clips (`clip_active[i] == false`) — wouldn't affect the visible
///   pose, so editing them silently from the viewport would be surprising.
/// - clips lifted from imported modules (`clip.origin.is_some()`) — their
///   track headers live in another file; we'd need cross-file editing to
///   round-trip safely.
/// - tracks without a `source_span` (procedural-template-emitted tracks)
///   and non-constant tracks (`from != to`, multi-keyframe).
pub(crate) fn find_active_constant_track(
    scene: &SceneGraph,
    clip_active: &[bool],
    node: NodeId,
    property: TrackProperty,
) -> Option<TrackBinding> {
    const EPS: f32 = 1e-5;
    for (i, clip) in scene.clips.iter().enumerate() {
        if !clip_active.get(i).copied().unwrap_or(false) {
            continue;
        }
        if clip.origin.is_some() {
            continue;
        }
        for track in &clip.tracks {
            if track.node != node || track.property != property {
                continue;
            }
            let Some(span) = track.source_span else {
                continue;
            };
            if !track.is_constant_value(EPS) {
                continue;
            }
            return Some(TrackBinding { span, property });
        }
    }
    None
}

/// Whether the gizmo for `mode` should be drawn AND respond to drags on
/// `node_id`. The render path and `begin_gizmo_drag` both consult this so
/// the visual affordance never lies — if the input layer would refuse the
/// drag, no handles are drawn and clicks fall through to camera orbit
/// without first looking active. Keep this in sync with the commit path
/// in [`commit_gizmo_drag`].
pub(crate) fn gizmo_handles_supported(
    scene: &SceneGraph,
    clip_active: &[bool],
    node_id: NodeId,
    mode: crate::gizmo::GizmoMode,
) -> bool {
    let Some(node) = scene.nodes.get(node_id.0 as usize) else {
        return false;
    };
    if !node.editable {
        return false;
    }
    // Imported subtree (`use_id != None` AND `origin = Some(path)`): the
    // node's source span points at the imported file, not the active
    // source. `replace_selection` redirects picks to the nearest
    // user-authored wrapper, but a stale selection (set before the
    // redirect existed, or restored from a path that now resolves into an
    // imported subtree) can still land here. Refusing the gizmo handles
    // is the same affordance as for replicators: no draggable handle, so
    // the user can't initiate an edit that would be silently dropped or
    // corrupt the wrong file.
    //
    // Exceptions:
    //   - The synthesised wrapper group of `use "X" (pos=...)` for an
    //     imported file has `use_id = Some(...)` but its source span is
    //     the `use` line in the active source — set_attr writes the
    //     `pos=`/`rot=`/`scale=` back through it cleanly.
    //   - A `use "local_module" ()` call expands to nodes with
    //     `use_id = Some(...)` and `origin = None`; their `source_span`
    //     IS in the active source (the module body lives in the same
    //     `.mog`), so the gizmo writeback is safe. Stdlib expansions are
    //     distinguished by `origin = Some("<stdlib>/…")` and continue to
    //     bail out.
    //   - An active constant track in the active source drives this node
    //     for the matching property. The writeback lands on the track's
    //     header (which lives in the active source) instead of the
    //     joint's rest pose, so the imported origin doesn't matter.
    if node.use_id.is_some() && node.origin.is_some() && !is_import_wrapper(scene, node_id) {
        let track_editable = track_property_for_gizmo(mode)
            .and_then(|p| find_active_constant_track(scene, clip_active, node_id, p))
            .is_some();
        if !track_editable {
            return false;
        }
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
pub(crate) fn begin_gizmo_drag(
    st: &ViewerState,
    selected: NodeId,
    rect: egui::Rect,
    cursor: egui::Pos2,
    aspect: f32,
) -> Option<GizmoDrag> {
    let scene = st.scene.as_ref()?;
    if !gizmo_handles_supported(scene, &st.clip_active, selected, st.gizmo_mode) {
        return None;
    }
    let node = scene.nodes.get(selected.0 as usize)?;
    // Probe the active source for a constant track driving this node in the
    // gizmo's mode. A binding shifts both the start pose (sample animated
    // locals/worlds so the gizmo origin matches what the user sees) and the
    // commit path (writeback splices the track header instead of synthesising
    // a `pos=`/`rot=` on the joint's rest pose).
    let track_binding = track_property_for_gizmo(st.gizmo_mode)
        .and_then(|p| find_active_constant_track(scene, &st.clip_active, selected, p));
    let (start_transform, worlds) = if track_binding.is_some() {
        let animated = st.animated_locals();
        let worlds = world_transforms_from_locals(scene, &animated);
        let local = animated
            .get(selected.0 as usize)
            .copied()
            .unwrap_or(node.transform);
        (local, worlds)
    } else {
        (node.transform, scene.world_transforms())
    };
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
        start_transform,
        start_origin: origin,
        parent_start_world,
        start_ray_origin: ro,
        start_ray_dir: rd,
        delta: match st.gizmo_mode {
            crate::gizmo::GizmoMode::Scale => 1.0,
            _ => 0.0,
        },
        track_binding,
    })
}

pub(crate) fn update_gizmo_drag(
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

pub(crate) fn commit_gizmo_drag(st: &mut ViewerState) -> Vec<PendingEdit> {
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

    // Track-bound drag: bypass the joint's rest-pose writeback entirely and
    // splice the new axis-angle into the originating `track` header. Animation
    // channels REPLACE the node's TRS at runtime, so the new track value
    // **is** the desired local transform — no rest-pose composition needed.
    if let Some(binding) = drag.track_binding.as_ref() {
        return commit_track_drag(drag, binding, final_local);
    }
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

/// Splice a track-bound gizmo drag back onto its originating `track "..."
/// (axis=…, from=…, to=…)` header. Decomposes `final_local` into the same
/// axis-angle (or axis-distance) form `anim_lower::sample_2kf` will rebuild
/// it from on the next compile, so the round-trip lands the bone exactly
/// where the user dragged it.
///
/// Constant tracks only: the writeback collapses `from` and `to` to one
/// scalar. Multi-keyframe / animated tracks fall through to the joint
/// writeback path (see `find_active_constant_track`) and never reach here.
///
/// Shortcut shadows (`prop` aliases, `keys=`) are left alone: the user
/// asked for a constant track, and stomping `keys=` would silently discard
/// any leftover authoring intent.
fn commit_track_drag(
    _drag: &GizmoDrag,
    binding: &TrackBinding,
    final_local: Transform,
) -> Vec<PendingEdit> {
    let (axis, scalar) = match binding.property {
        TrackProperty::Rotation => {
            let q = final_local.rotation.normalize();
            // Quat::to_axis_angle returns (axis, angle) with angle in
            // [0, 2π]. Tiny rotations have an indeterminate axis — fall
            // back to the start-pose axis so dragging back to identity
            // doesn't smear the axis to a meaningless value.
            let (axis, angle) = q.to_axis_angle();
            let degrees = angle.to_degrees();
            // Wrap > 180° to the shortest-arc equivalent so the writeback
            // matches what an author would naturally type.
            let (axis, degrees) = if degrees > 180.0 {
                (-axis, 360.0 - degrees)
            } else {
                (axis, degrees)
            };
            (axis, degrees)
        }
        TrackProperty::Translation => {
            let p = final_local.translation;
            let len = p.length();
            let axis = if len > 1e-5 {
                p / len
            } else {
                Vec3::Y
            };
            (axis, len)
        }
        TrackProperty::Scale => {
            // Scale tracks are out of scope for the track-binding gizmo;
            // begin_gizmo_drag never produces a binding for scale mode.
            return Vec::new();
        }
    };
    let scalar_s = format_scalar(scalar);
    vec![
        PendingEdit::SetAttrAtSpan {
            span: binding.span,
            attr: "axis".to_string(),
            value: format!(
                "[{}, {}, {}]",
                format_scalar(axis.x),
                format_scalar(axis.y),
                format_scalar(axis.z)
            ),
            delete: Vec::new(),
        },
        PendingEdit::SetAttrAtSpan {
            span: binding.span,
            attr: "from".to_string(),
            value: scalar_s.clone(),
            delete: Vec::new(),
        },
        PendingEdit::SetAttrAtSpan {
            span: binding.span,
            attr: "to".to_string(),
            value: scalar_s,
            delete: Vec::new(),
        },
    ]
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

pub(crate) fn apply_gizmo_drag(drag: &GizmoDrag) -> Transform {
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
