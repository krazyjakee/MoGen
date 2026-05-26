//! Tests for animation-track binding through the gizmo: lower stamps
//! source spans, constant-track detection, caret-in-track fallback,
//! and round-trip writeback into the source.

use super::super::state::{
    commit_gizmo_drag, find_active_constant_track, find_deepest_node_at_offset, GizmoDrag,
    PendingEdit, TrackBinding, ViewerState,
};
use glam::{Mat4, Quat, Vec3};
use mogen_core::{NodeId, SceneGraph, Span, TrackProperty, Transform};
use std::sync::Arc;

/// Compile helper: lower a `.mog` source through the full pipeline so
/// the resulting scene has clip tracks with `source_span` populated by
/// `anim_lower`.
fn compiled_scene(src: &str) -> (Arc<SceneGraph>, crate::pipeline::CompileResult) {
    let result = crate::pipeline::compile(src, None);
    let scene = result
        .scene
        .as_ref()
        .cloned()
        .unwrap_or_else(|| panic!("compile failed: {:?}", result.diagnostics));
    (scene, result)
}

#[test]
fn lower_stamps_source_span_on_track() {
    let src = r#"
scene { box "hip" (size=[0.1, 0.1, 0.1]) }
clip "pose" (seconds=1.0) {
  track "hip" (prop="rotation", axis=[1, 0, 0], from=30, to=30)
}
"#;
    let (scene, _) = compiled_scene(src);
    let track = &scene.clips[0].tracks[0];
    let span = track.source_span.expect("track must carry source_span");
    let snippet = &src[span.start..span.end];
    assert!(
        snippet.starts_with("track \"hip\""),
        "track span should cover the track header, got: {snippet}"
    );
}

#[test]
fn track_constant_detection_picks_only_active_authored_constants() {
    let src = r#"
scene { box "hip" (size=[0.1, 0.1, 0.1]) box "elbow" (size=[0.1, 0.1, 0.1]) }
clip "pose_const" (seconds=1.0) {
  track "hip" (prop="rotation", axis=[1, 0, 0], from=30, to=30)
}
clip "pose_anim" (seconds=1.0) {
  track "elbow" (prop="rotation", axis=[1, 0, 0], from=0, to=45)
}
"#;
    let (scene, _) = compiled_scene(src);
    let hip = scene.find_node("hip").unwrap();
    let elbow = scene.find_node("elbow").unwrap();
    let active = vec![true; scene.clips.len()];

    let binding = find_active_constant_track(&scene, &active, hip, TrackProperty::Rotation);
    assert!(binding.is_some(), "constant rotation track should bind");

    let none = find_active_constant_track(&scene, &active, elbow, TrackProperty::Rotation);
    assert!(none.is_none(), "animated track must fall through");

    let inactive = vec![false; scene.clips.len()];
    let none = find_active_constant_track(&scene, &inactive, hip, TrackProperty::Rotation);
    assert!(none.is_none(), "inactive clip must not bind");
}

#[test]
fn find_offset_falls_back_to_track_target_for_caret_in_track_header() {
    // The bone's source_span lives in the imported file (origin=Some), so
    // `find_deepest_node_at_offset` skips it. The track's span is in the
    // active source — caret-in-track-header should select the bone via
    // the new fallback path so the user can reach an imported bone.
    let src = r#"
scene { box "shoulder" (size=[0.1, 0.1, 0.1]) }
clip "pose" (seconds=1.0) {
  track "shoulder" (prop="rotation", axis=[1, 0, 0], from=15, to=15)
}
"#;
    let (scene, _) = compiled_scene(src);
    let track_span = scene.clips[0].tracks[0].source_span.unwrap();
    let offset = track_span.start + 10;
    let target = find_deepest_node_at_offset(&scene, offset)
        .expect("caret inside track header must select a node");
    let shoulder = scene.find_node("shoulder").unwrap();
    assert_eq!(target, shoulder, "should land on the track's target bone");
}

#[test]
fn commit_gizmo_drag_with_track_binding_emits_axis_from_to() {
    // Three SetAttrAtSpan edits (axis=, from=, to=) all targeting the
    // same track-header span — the writeback splices a self-consistent
    // constant track in one go.
    let span = Span::new(100, 200);
    let mut st = ViewerState::default();
    st.gizmo_drag = Some(GizmoDrag {
        node: NodeId(0),
        axis: crate::gizmo::Axis::X,
        mode: crate::gizmo::GizmoMode::Rotate,
        start_transform: Transform::IDENTITY,
        start_origin: Vec3::ZERO,
        parent_start_world: Mat4::IDENTITY,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 45.0_f32.to_radians(),
        track_binding: Some(TrackBinding {
            span,
            property: TrackProperty::Rotation,
        }),
    });
    let edits = commit_gizmo_drag(&mut st);
    assert_eq!(edits.len(), 3, "expected axis/from/to triple, got {edits:?}");
    let attrs: Vec<&str> = edits
        .iter()
        .map(|e| match e {
            PendingEdit::SetAttrAtSpan { attr, span: s, .. } => {
                assert_eq!(*s, span, "all edits should target the track header");
                attr.as_str()
            }
            _ => panic!("expected SetAttrAtSpan, got {e:?}"),
        })
        .collect();
    assert_eq!(attrs, vec!["axis", "from", "to"]);
    if let PendingEdit::SetAttrAtSpan { value, .. } = &edits[1] {
        let parsed: f32 = value.parse().unwrap_or(0.0);
        assert!(
            (parsed - 45.0).abs() < 0.01,
            "from value should be ~45°, got {value}",
        );
    }
    if let PendingEdit::SetAttrAtSpan { value, .. } = &edits[0] {
        assert!(
            value.starts_with("[1") || value.starts_with("[0.99"),
            "axis should be ~[1, 0, 0], got {value}",
        );
    }
}

#[test]
fn track_gizmo_round_trips_through_compile() {
    // Compile a constant-track scene, build a gizmo drag with the track
    // binding, splice the emitted edits back into the source, recompile,
    // and verify the new track lands the joint at the dragged pose.
    use crate::edit;
    let src = r#"scene { box "hip" (size=[0.1, 0.1, 0.1]) }
clip "pose" (seconds=1.0) {
  track "hip" (prop="rotation", axis=[1, 0, 0], from=0, to=0)
}
"#;
    let (scene, _result) = compiled_scene(src);
    let hip = scene.find_node("hip").unwrap();
    let span = scene.clips[0].tracks[0].source_span.unwrap();

    let mut st = ViewerState::default();
    st.gizmo_drag = Some(GizmoDrag {
        node: hip,
        axis: crate::gizmo::Axis::Y,
        mode: crate::gizmo::GizmoMode::Rotate,
        start_transform: Transform::IDENTITY,
        start_origin: Vec3::ZERO,
        parent_start_world: Mat4::IDENTITY,
        start_ray_origin: Vec3::ZERO,
        start_ray_dir: Vec3::Z,
        delta: 60.0_f32.to_radians(),
        track_binding: Some(TrackBinding {
            span,
            property: TrackProperty::Rotation,
        }),
    });
    let edits = commit_gizmo_drag(&mut st);
    assert_eq!(edits.len(), 3);

    let mut out = src.to_string();
    for edit in edits {
        if let PendingEdit::SetAttrAtSpan { span, attr, value, .. } = edit {
            out = edit::set_attr(&out, span, &attr, &value);
        }
    }
    assert!(out.contains("from=60"), "expected from=60 in source: {out}");
    assert!(out.contains("to=60"), "expected to=60 in source: {out}");

    let recompiled = crate::pipeline::compile(&out, None);
    let scene2 = recompiled
        .scene
        .as_ref()
        .unwrap_or_else(|| panic!("recompile failed: {:?}", recompiled.diagnostics));
    let track = &scene2.clips[0].tracks[0];
    let v = track.values[0];
    let got = Quat::from_xyzw(v[0], v[1], v[2], v[3]);
    let expected = Quat::from_axis_angle(Vec3::Y, 60.0_f32.to_radians());
    assert!(
        got.dot(expected).abs() > 1.0 - 1e-3,
        "track value should encode 60° Y rotation; got {got:?} expected {expected:?}",
    );
}
