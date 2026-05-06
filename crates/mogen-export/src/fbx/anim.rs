//! Animation export — `Clip` → `AnimationStack` + `AnimationLayer` plus per-
//! track `AnimationCurveNode` / `AnimationCurve` triples.
//!
//! FBX times are stored as i64 ticks. The conversion factor is
//! `46_186_158_000` ticks per second, regardless of the target frame rate
//! (FBX uses ticks as a continuous time base; `TimeMode` only changes the
//! display unit). At 30 fps every frame falls on an exact tick boundary —
//! useful for both inspectors and round-trip tests.

use fbxcel::low::v7400::AttributeValue;

use mogen_core::{Interpolation, SceneGraph, TrackProperty};

use super::doc::{push_prop, write_properties70, ObjectEmitter};
use super::ids::IdAllocator;

/// FBX tick count for one second.
pub(super) const FBX_TICKS_PER_SECOND: i64 = 46_186_158_000;

/// Convert seconds → FBX KTime ticks.
pub(super) fn seconds_to_ktime(seconds: f32) -> i64 {
    (seconds as f64 * FBX_TICKS_PER_SECOND as f64).round() as i64
}

// `KeyAttrFlags` constants from the FBX SDK (`kfbxnodeshape.h`).
const KEY_ATTR_FLAGS_LINEAR: i32 = 0x00000002;
const KEY_ATTR_FLAGS_CONSTANT: i32 = 0x00000004;

pub(super) fn emit_animations(
    scene: &SceneGraph,
    model_ids: &[i64],
    ids: &mut IdAllocator,
    emit: &mut ObjectEmitter,
) {
    if scene.clips.is_empty() {
        return;
    }

    for clip in &scene.clips {
        let stack_id = ids.alloc();
        let layer_id = ids.alloc();
        let stack_name = clip.name.clone();
        let layer_name = format!("{}_layer", clip.name);
        let stop_ticks = seconds_to_ktime(clip.duration);

        emit.push_object(
            "AnimationStack",
            Box::new(move |tree, parent| {
                let n = tree.append_new(parent, "AnimationStack");
                tree.append_attribute(n, stack_id);
                tree.append_attribute(n, format!("{stack_name}\u{0}\u{1}AnimStack"));
                tree.append_attribute(n, "");
                write_properties70(tree, n, |t, props| {
                    push_prop(t, props, "LocalStart", "KTime", "Time", "", AttributeValue::I64(0));
                    push_prop(t, props, "LocalStop", "KTime", "Time", "", AttributeValue::I64(stop_ticks));
                    push_prop(t, props, "ReferenceStart", "KTime", "Time", "", AttributeValue::I64(0));
                    push_prop(t, props, "ReferenceStop", "KTime", "Time", "", AttributeValue::I64(stop_ticks));
                });
            }),
        );

        emit.push_object(
            "AnimationLayer",
            Box::new(move |tree, parent| {
                let n = tree.append_new(parent, "AnimationLayer");
                tree.append_attribute(n, layer_id);
                tree.append_attribute(n, format!("{layer_name}\u{0}\u{1}AnimLayer"));
                tree.append_attribute(n, "");
            }),
        );
        emit.connect_oo(layer_id, stack_id);

        for track in &clip.tracks {
            let model_id = match model_ids.get(track.node.0 as usize) {
                Some(&m) => m,
                None => continue,
            };

            let prop_name: &'static str = match track.property {
                TrackProperty::Translation => "Lcl Translation",
                TrackProperty::Rotation => "Lcl Rotation",
                TrackProperty::Scale => "Lcl Scaling",
            };

            // Per-axis component values. Rotation tracks come in as quats;
            // FBX wants Euler XYZ degrees on `Lcl Rotation`. Linear-
            // interpolated quaternion data going through Euler isn't
            // bit-identical (it can change which axis catches a rollover)
            // but matches the GLB exporter's treatment of the same data,
            // and the values still describe the same orientations.
            let component_values: Vec<[f32; 3]> = match track.property {
                TrackProperty::Rotation => track
                    .values
                    .iter()
                    .map(|v| {
                        let q = glam::Quat::from_xyzw(v[0], v[1], v[2], v[3]);
                        let (x, y, z) = q.to_euler(glam::EulerRot::XYZ);
                        [x.to_degrees(), y.to_degrees(), z.to_degrees()]
                    })
                    .collect(),
                _ => track.values.iter().map(|v| [v[0], v[1], v[2]]).collect(),
            };

            // CurveNode wraps three component curves (X, Y, Z). All Lcl
            // properties go through this same pattern in FBX 7.4.
            let curve_node_id = ids.alloc();
            emit.push_object(
                "AnimationCurveNode",
                Box::new(move |tree, parent| {
                    let n = tree.append_new(parent, "AnimationCurveNode");
                    tree.append_attribute(n, curve_node_id);
                    tree.append_attribute(n, format!("{prop_name}\u{0}\u{1}AnimCurveNode"));
                    tree.append_attribute(n, "");
                    write_properties70(tree, n, |t, props| {
                        push_prop(t, props, "d|X", "Number", "", "A", AttributeValue::F64(0.0));
                        push_prop(t, props, "d|Y", "Number", "", "A", AttributeValue::F64(0.0));
                        push_prop(t, props, "d|Z", "Number", "", "A", AttributeValue::F64(0.0));
                    });
                }),
            );
            emit.connect_oo(curve_node_id, layer_id);
            emit.connect_op(curve_node_id, model_id, prop_name);

            for axis in 0..3usize {
                let curve_id = ids.alloc();
                let times: Vec<i64> =
                    track.times.iter().map(|t| seconds_to_ktime(*t)).collect();
                let values: Vec<f32> =
                    component_values.iter().map(|v| v[axis]).collect();
                let flags = match track.interpolation {
                    Interpolation::Linear => KEY_ATTR_FLAGS_LINEAR,
                    Interpolation::Step => KEY_ATTR_FLAGS_CONSTANT,
                };
                let key_count = times.len() as i32;
                let axis_letter = ["X", "Y", "Z"][axis];

                emit.push_object(
                    "AnimationCurve",
                    Box::new(move |tree, parent| {
                        let n = tree.append_new(parent, "AnimationCurve");
                        tree.append_attribute(n, curve_id);
                        tree.append_attribute(n, "\u{0}\u{1}AnimCurve");
                        tree.append_attribute(n, "");

                        let default = tree.append_new(n, "Default");
                        tree.append_attribute(default, 0.0_f64);

                        let kver = tree.append_new(n, "KeyVer");
                        tree.append_attribute(kver, 4008i32);

                        let kt = tree.append_new(n, "KeyTime");
                        tree.append_attribute(kt, AttributeValue::ArrI64(times));

                        let kv = tree.append_new(n, "KeyValueFloat");
                        tree.append_attribute(kv, AttributeValue::ArrF32(values));

                        // Per-FBX convention, when every key in the curve
                        // shares the same flags / data / refcount, the
                        // arrays carry exactly one entry. Importers
                        // broadcast that single entry across every key.
                        let kaf = tree.append_new(n, "KeyAttrFlags");
                        tree.append_attribute(kaf, AttributeValue::ArrI32(vec![flags]));

                        let kad = tree.append_new(n, "KeyAttrDataFloat");
                        tree.append_attribute(kad, AttributeValue::ArrF32(vec![0.0; 4]));

                        let krc = tree.append_new(n, "KeyAttrRefCount");
                        tree.append_attribute(krc, AttributeValue::ArrI32(vec![key_count]));
                    }),
                );
                emit.connect_op(curve_id, curve_node_id, format!("d|{axis_letter}"));
            }
        }
    }
}
