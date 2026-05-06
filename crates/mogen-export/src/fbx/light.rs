//! `Light` → `NodeAttribute` (subclass `Light`) emission.
//!
//! FBX represents lights as a `Model` (the transform carrier) plus a
//! `NodeAttribute` of subclass `Light` connected via OO. The Model is
//! already emitted by `nodes.rs`; this module owns the NodeAttribute and
//! the OO connection that links it.
//!
//! Lossy mappings flagged in the plan: glTF `KHR_lights_punctual` uses
//! candela (point/spot) vs lux (directional) for `intensity`. FBX has no
//! such distinction — the value passes through unchanged into the FBX
//! `Intensity` property and is the renderer's problem to interpret.

use fbxcel::low::v7400::AttributeValue;

use mogen_core::{LightKind, SceneGraph};

use super::doc::{push_prop, push_prop_vec3, write_properties70, ObjectEmitter};
use super::ids::IdAllocator;

pub(super) fn emit_lights(
    scene: &SceneGraph,
    model_ids: &[i64],
    ids: &mut IdAllocator,
    emit: &mut ObjectEmitter,
) {
    for (i, n) in scene.nodes.iter().enumerate() {
        let light = match &n.light {
            Some(l) => l,
            None => continue,
        };
        let model_id = model_ids[i];
        let attr_id = ids.alloc();

        // Snapshot owned data into the closure.
        let name = n.name.clone();
        let light_type: i32 = match light.kind {
            LightKind::Point => 0,
            LightKind::Directional => 1,
            LightKind::Spot => 2,
        };
        let color: [f64; 3] = [
            light.color[0] as f64,
            light.color[1] as f64,
            light.color[2] as f64,
        ];
        let intensity = light.intensity as f64;
        let range = light.range.map(|r| r as f64);
        let inner_deg = light.inner_cone_rad.to_degrees() as f64;
        let outer_deg = light.outer_cone_rad.to_degrees() as f64;
        let kind = light.kind;

        emit.push_object(
            "NodeAttribute",
            Box::new(move |tree, parent| {
                let na = tree.append_new(parent, "NodeAttribute");
                tree.append_attribute(na, attr_id);
                tree.append_attribute(na, format!("{name}\u{0}\u{1}NodeAttribute"));
                tree.append_attribute(na, "Light");

                // The TypeFlags string distinguishes Light/Camera/etc when
                // a NodeAttribute could be ambiguous from its subclass.
                let tf = tree.append_new(na, "TypeFlags");
                tree.append_attribute(tf, "Light");

                write_properties70(tree, na, |t, props| {
                    push_prop(t, props, "LightType", "enum", "", "", AttributeValue::I32(light_type));
                    push_prop(t, props, "CastLight", "bool", "", "", AttributeValue::I32(1));
                    push_prop_vec3(t, props, "Color", "Color", "", "A", color);
                    push_prop(t, props, "Intensity", "Number", "", "A", AttributeValue::F64(intensity));
                    if matches!(kind, LightKind::Point | LightKind::Spot) {
                        if let Some(r) = range {
                            push_prop(
                                t,
                                props,
                                "FarAttenuationStart",
                                "Number",
                                "",
                                "A",
                                AttributeValue::F64(r * 0.5),
                            );
                            push_prop(
                                t,
                                props,
                                "FarAttenuationEnd",
                                "Number",
                                "",
                                "A",
                                AttributeValue::F64(r),
                            );
                            push_prop(
                                t,
                                props,
                                "EnableFarAttenuation",
                                "bool",
                                "",
                                "",
                                AttributeValue::I32(1),
                            );
                        }
                    }
                    if matches!(kind, LightKind::Spot) {
                        push_prop(
                            t,
                            props,
                            "InnerAngle",
                            "Number",
                            "",
                            "A",
                            AttributeValue::F64(inner_deg),
                        );
                        push_prop(
                            t,
                            props,
                            "OuterAngle",
                            "Number",
                            "",
                            "A",
                            AttributeValue::F64(outer_deg),
                        );
                    }
                });
            }),
        );

        emit.connect_oo(attr_id, model_id);
    }
}
