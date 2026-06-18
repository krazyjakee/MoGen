//! Tests for snap helpers and `commit_gizmo_drag` writeback emission
//! (translate / rotate / parent-conjugation paths).

use super::super::state::{
    apply_gizmo_drag, commit_gizmo_drag, snap_rotate_delta, snap_scale_factor,
    snap_translate_delta, GizmoDrag, GizmoTarget, PendingEdit, ViewerState, SCALE_SNAP_STEP,
};
use glam::{Mat4, Quat, Vec3};
use mogen_core::{NodeId, Transform};

#[test]
fn snap_translate_rounds_to_quarter_grid_from_start() {
    let got = snap_translate_delta(0.45, 1.1);
    assert!((got - 1.05).abs() < 1e-5, "snap delta was {got}");
    let got = snap_translate_delta(0.0, 0.26);
    assert!((got - 0.25).abs() < 1e-5, "snap delta was {got}");
}

#[test]
fn snap_rotate_rounds_to_fifteen_degrees() {
    use std::f32::consts::PI;
    let deg = |r: f32| r.to_degrees();
    assert!(
        (deg(snap_rotate_delta(22.0_f32.to_radians())) - 15.0).abs() < 1e-3,
        "got {}",
        deg(snap_rotate_delta(22.0_f32.to_radians()))
    );
    assert!((deg(snap_rotate_delta(38.0_f32.to_radians())) - 45.0).abs() < 1e-3);
    assert!((deg(snap_rotate_delta(-6.0_f32.to_radians())) - 0.0).abs() < 1e-3);
    assert!((deg(snap_rotate_delta(-8.0_f32.to_radians())) + 15.0).abs() < 1e-3);
    assert!(
        (snap_rotate_delta(2.0 * PI) - 2.0 * PI).abs() < 1e-4,
        "360° should remain 360°"
    );
}

#[test]
fn snap_scale_factor_floors_at_step() {
    assert!((snap_scale_factor(1.1) - 1.0).abs() < 1e-5);
    assert!((snap_scale_factor(1.2) - 1.25).abs() < 1e-5);
    assert!((snap_scale_factor(0.0) - SCALE_SNAP_STEP).abs() < 1e-5);
    assert!((snap_scale_factor(-5.0) - SCALE_SNAP_STEP).abs() < 1e-5);
}

#[test]
fn commit_gizmo_drag_is_noop_with_zero_delta() {
    let mut st = ViewerState::default();
    st.gizmo_drag = Some(GizmoDrag {
        node: NodeId(0),
        axis: crate::gizmo::Axis::X,
        mode: crate::gizmo::GizmoMode::Translate,
        start_transform: Transform::IDENTITY,
        start_origin: Vec3::ZERO,
        parent_start_world: Mat4::IDENTITY,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 0.0,
        track_binding: None,
        others: Vec::new(),
    });
    assert!(commit_gizmo_drag(&mut st).is_empty());
}

#[test]
fn commit_gizmo_drag_translate_emits_full_pos_vec3() {
    let mut st = ViewerState::default();
    st.gizmo_drag = Some(GizmoDrag {
        node: NodeId(3),
        axis: crate::gizmo::Axis::Y,
        mode: crate::gizmo::GizmoMode::Translate,
        start_transform: Transform::from_trs(
            Vec3::new(0.25, 0.5, 0.0),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
        start_origin: Vec3::ZERO,
        parent_start_world: Mat4::IDENTITY,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 0.75,
        track_binding: None,
        others: Vec::new(),
    });
    let mut edits = commit_gizmo_drag(&mut st).into_iter();
    let Some(PendingEdit::SetAttrCanonical {
        node,
        attr,
        value,
        delete,
    }) = edits.next()
    else {
        panic!("expected SetAttrCanonical");
    };
    assert!(edits.next().is_none(), "non-relative_placed should emit one edit");
    assert_eq!(node, NodeId(3));
    assert_eq!(attr, "pos");
    assert_eq!(value, "[0.25, 1.25, 0]");
    assert_eq!(delete, vec!["x", "y", "z", "from", "to"]);
}

#[test]
fn commit_gizmo_drag_translate_applies_to_every_selected_node() {
    // Shift-click multi-select: the gizmo is anchored to the primary
    // (`node`) but the drag must move every selected node by the same
    // world-space delta. `others` carries each secondary node's own start
    // state, so the +0.5-on-Y delta lands on both.
    let mut st = ViewerState::default();
    st.gizmo_drag = Some(GizmoDrag {
        node: NodeId(1),
        axis: crate::gizmo::Axis::Y,
        mode: crate::gizmo::GizmoMode::Translate,
        start_transform: Transform::from_trs(
            Vec3::new(0.0, 1.0, 0.0),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
        start_origin: Vec3::ZERO,
        parent_start_world: Mat4::IDENTITY,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 0.5,
        track_binding: None,
        others: vec![GizmoTarget {
            node: NodeId(2),
            start_transform: Transform::from_trs(
                Vec3::new(3.0, 0.0, 0.0),
                Quat::IDENTITY,
                Vec3::ONE,
            ),
            parent_start_world: Mat4::IDENTITY,
        }],
    });
    let edits = commit_gizmo_drag(&mut st);
    assert_eq!(edits.len(), 2, "one edit per selected node, got {edits:?}");
    let PendingEdit::SetAttrCanonical { node, attr, value, .. } = &edits[0] else {
        panic!("expected SetAttrCanonical for primary");
    };
    assert_eq!(*node, NodeId(1), "primary node committed first");
    assert_eq!(attr, "pos");
    assert_eq!(value, "[0, 1.5, 0]");
    let PendingEdit::SetAttrCanonical { node, attr, value, .. } = &edits[1] else {
        panic!("expected SetAttrCanonical for secondary");
    };
    assert_eq!(*node, NodeId(2), "secondary node moved in lockstep");
    assert_eq!(attr, "pos");
    assert_eq!(value, "[3, 0.5, 0]", "same +0.5 Y delta applied");
}

#[test]
fn commit_gizmo_drag_rotate_emits_euler_vec3() {
    let mut st = ViewerState::default();
    st.gizmo_drag = Some(GizmoDrag {
        node: NodeId(7),
        axis: crate::gizmo::Axis::Y,
        mode: crate::gizmo::GizmoMode::Rotate,
        start_transform: Transform::IDENTITY,
        start_origin: Vec3::ZERO,
        parent_start_world: Mat4::IDENTITY,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 45.0_f32.to_radians(),
        track_binding: None,
        others: Vec::new(),
    });
    let mut edits = commit_gizmo_drag(&mut st).into_iter();
    let Some(PendingEdit::SetAttrCanonical {
        node,
        attr,
        value,
        delete,
    }) = edits.next()
    else {
        panic!("expected SetAttrCanonical from non-trivial rotation");
    };
    assert!(edits.next().is_none(), "rotate should emit a single edit");
    assert_eq!(node, NodeId(7));
    assert_eq!(attr, "rot");
    assert_eq!(value, "[0, 45, 0]");
    assert_eq!(delete, vec!["rx", "ry", "rz", "dir"]);
}

#[test]
fn translate_drag_pulls_world_delta_through_rotated_parent() {
    // Parent rotated +90° about Y. World +X drag of 1 unit must land
    // as +Z in the child's local translation so the post-compile world
    // position moves along world +X (not the parent's tilted X).
    let parent_rot = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
    let parent_world = Mat4::from_quat(parent_rot);
    let mut st = ViewerState::default();
    st.gizmo_drag = Some(GizmoDrag {
        node: NodeId(1),
        axis: crate::gizmo::Axis::X,
        mode: crate::gizmo::GizmoMode::Translate,
        start_transform: Transform::IDENTITY,
        start_origin: Vec3::ZERO,
        parent_start_world: parent_world,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 1.0,
        track_binding: None,
        others: Vec::new(),
    });
    let edits = commit_gizmo_drag(&mut st);
    let Some(PendingEdit::SetAttrCanonical { value, .. }) = edits.into_iter().next() else {
        panic!("expected SetAttrCanonical");
    };
    assert_eq!(value, "[0, 0, 1]", "got {value}");
}

#[test]
fn translate_drag_compensates_for_parent_scale() {
    // Parent scales 2x along Y. A 1-unit world-Y drag must shrink to
    // 0.5 in local space so the post-compile world translation is
    // exactly +1 unit, not +2.
    let parent_world = Mat4::from_scale(Vec3::new(1.0, 2.0, 1.0));
    let mut st = ViewerState::default();
    st.gizmo_drag = Some(GizmoDrag {
        node: NodeId(1),
        axis: crate::gizmo::Axis::Y,
        mode: crate::gizmo::GizmoMode::Translate,
        start_transform: Transform::IDENTITY,
        start_origin: Vec3::ZERO,
        parent_start_world: parent_world,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 1.0,
        track_binding: None,
        others: Vec::new(),
    });
    let edits = commit_gizmo_drag(&mut st);
    let Some(PendingEdit::SetAttrCanonical { value, .. }) = edits.into_iter().next() else {
        panic!("expected SetAttrCanonical");
    };
    assert_eq!(value, "[0, 0.5, 0]", "got {value}");
}

#[test]
fn rotate_drag_conjugates_through_rotated_parent() {
    // Parent rotated +90° about Y, child starts identity.
    let parent_rot = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
    let parent_world = Mat4::from_quat(parent_rot);

    // Drag the world-Y rotation handle 30°. Since Y is the parent's
    // invariant axis, the conjugation is the identity and the local
    // rotation lands as a pure +30° about Y.
    let mut st = ViewerState::default();
    st.gizmo_drag = Some(GizmoDrag {
        node: NodeId(1),
        axis: crate::gizmo::Axis::Y,
        mode: crate::gizmo::GizmoMode::Rotate,
        start_transform: Transform::IDENTITY,
        start_origin: Vec3::ZERO,
        parent_start_world: parent_world,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 30.0_f32.to_radians(),
        track_binding: None,
        others: Vec::new(),
    });
    let edits = commit_gizmo_drag(&mut st);
    let Some(PendingEdit::SetAttrCanonical { value, .. }) = edits.into_iter().next() else {
        panic!("expected SetAttrCanonical");
    };
    assert_eq!(value, "[0, 30, 0]", "got {value}");

    // Now drag the world-X rotation handle 30° under the same parent.
    // The local-space writeback won't be a pure +X rotation, but
    // recomposing parent_rot * local should equal a world-space +30°
    // about world +X.
    let mut st = ViewerState::default();
    st.gizmo_drag = Some(GizmoDrag {
        node: NodeId(1),
        axis: crate::gizmo::Axis::X,
        mode: crate::gizmo::GizmoMode::Rotate,
        start_transform: Transform::IDENTITY,
        start_origin: Vec3::ZERO,
        parent_start_world: parent_world,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 30.0_f32.to_radians(),
        track_binding: None,
        others: Vec::new(),
    });
    let local = apply_gizmo_drag(st.gizmo_drag.as_ref().unwrap());
    let world_rot = parent_rot * local.rotation;
    // Recover the world-space rotation the drag added: the node's
    // world rotation before the drag was just parent_rot (start
    // transform was identity), so right-multiplying by its inverse
    // peels that back off and what's left should be the +30° about
    // world X the user grabbed.
    let added_world = world_rot * parent_rot.inverse();
    let expected = Quat::from_axis_angle(Vec3::X, 30.0_f32.to_radians());
    let dot = added_world.dot(expected).abs();
    assert!(
        dot > 0.9999,
        "world-space rotation added by the drag should equal +30° about world X; got dot={dot}"
    );
}

#[test]
fn commit_gizmo_drag_translate_relative_placed_emits_axis_shortcuts() {
    // `relative_placed` Translate commits write per-axis shortcuts so the
    // snap-axis value trips `pos_axis_explicit` even when it lands on 0
    // (a plain `pos=[…]` would lose the snap-axis component to the next
    // layout pass when the resolved value is 0). The first edit also
    // strips `pos`/`from`/`to` so they don't fight the new shortcuts.
    use mogen_core::SceneGraph;
    use std::sync::Arc;

    let mut scene = SceneGraph::new();
    let parent = scene.add_root("group", "group", Transform::IDENTITY);
    let child = scene.add_child(parent, "tier2", "box", Transform::IDENTITY);
    scene.nodes[child.0 as usize].relative_placed = true;
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    st.gizmo_drag = Some(GizmoDrag {
        node: child,
        axis: crate::gizmo::Axis::Y,
        mode: crate::gizmo::GizmoMode::Translate,
        start_transform: Transform::from_trs(
            Vec3::new(0.1, 0.5, 0.0),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
        start_origin: Vec3::ZERO,
        parent_start_world: Mat4::IDENTITY,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 0.75,
        track_binding: None,
        others: Vec::new(),
    });
    let edits = commit_gizmo_drag(&mut st);
    assert_eq!(edits.len(), 3, "expected three shortcut edits, got {edits:?}");
    let unwrap_set = |e: &PendingEdit| match e {
        PendingEdit::SetAttrCanonical { node, attr, value, delete } => {
            (*node, attr.clone(), value.clone(), delete.clone())
        }
        _ => panic!("expected SetAttrCanonical, got {e:?}"),
    };
    let (n0, a0, v0, d0) = unwrap_set(&edits[0]);
    let (n1, a1, v1, d1) = unwrap_set(&edits[1]);
    let (n2, a2, v2, d2) = unwrap_set(&edits[2]);
    assert_eq!(n0, child);
    assert_eq!(n1, child);
    assert_eq!(n2, child);
    assert_eq!(a0, "x");
    assert_eq!(a1, "y");
    assert_eq!(a2, "z");
    assert_eq!(v0, "0.1");
    assert_eq!(v1, "1.25");
    assert_eq!(v2, "0");
    assert_eq!(d0, vec!["pos", "from", "to"]);
    assert!(d1.is_empty());
    assert!(d2.is_empty());
}
