//! Per-`SceneNode` `Model` Objects + their parent connections.
//!
//! Every node in the input graph becomes one `Model` object in FBX, with the
//! subclass picked from whether it carries a mesh / light / nothing. The
//! Model holds the local TRS via `Lcl Translation/Rotation/Scaling`
//! properties; quaternions are converted to Euler XYZ degrees because FBX's
//! `RotationOrder=0` (XYZ) is the only rotation order the rest of the
//! property block agrees with.

use fbxcel::low::v7400::AttributeValue;
use glam::EulerRot;

use mogen_core::SceneGraph;

use super::doc::{push_prop, push_prop_vec3, write_properties70, ObjectEmitter};
use super::ids::IdAllocator;

/// Allocate and emit one `Model` Object per `SceneNode`. Returns the
/// per-node id table so downstream emitters (skin, animation) can connect
/// into the right Model. Index `i` corresponds to `scene.nodes[i]`.
pub(super) fn emit_models(
    scene: &SceneGraph,
    ids: &mut IdAllocator,
    emit: &mut ObjectEmitter,
) -> Vec<i64> {
    let model_ids: Vec<i64> = scene.nodes.iter().map(|_| ids.alloc()).collect();

    for (i, n) in scene.nodes.iter().enumerate() {
        let model_id = model_ids[i];
        let name = n.name.clone();

        // Pick the subclass. Light and Mesh nodes get their own NodeAttribute
        // that we attach via OO-connections in the relevant emitter; the
        // Model subclass label here is what Blender uses to decide which
        // attribute it expects.
        let subclass: &'static str = if n.light.is_some() {
            "Light"
        } else if n.mesh.is_some() {
            "Mesh"
        } else {
            "Null"
        };

        let translation: [f64; 3] = [
            n.transform.translation.x as f64,
            n.transform.translation.y as f64,
            n.transform.translation.z as f64,
        ];
        let (rx, ry, rz) = n.transform.rotation.to_euler(EulerRot::XYZ);
        let rotation_deg: [f64; 3] = [
            (rx as f64).to_degrees(),
            (ry as f64).to_degrees(),
            (rz as f64).to_degrees(),
        ];
        let scale: [f64; 3] = [
            n.transform.scale.x as f64,
            n.transform.scale.y as f64,
            n.transform.scale.z as f64,
        ];

        // Stash DSL-only metadata that has no first-class FBX equivalent
        // as Properties70 custom props. Anything tooling-internal (use_id,
        // origin, source_span, editable, relative_placed) stays out.
        let kind = n.kind.clone();
        let role = n.role.clone();
        let tags = n.tags.clone();
        let cast_shadow = n.cast_shadow;

        emit.push_object(
            "Model",
            Box::new(move |tree, parent| {
                let m = tree.append_new(parent, "Model");
                tree.append_attribute(m, model_id);
                tree.append_attribute(m, format!("{name}\u{0}\u{1}Model"));
                tree.append_attribute(m, subclass);

                let v = tree.append_new(m, "Version");
                tree.append_attribute(v, 232i32);

                write_properties70(tree, m, |t, props| {
                    push_prop(t, props, "RotationActive", "bool", "", "", AttributeValue::I32(1));
                    push_prop(t, props, "InheritType", "enum", "", "", AttributeValue::I32(0));
                    push_prop(t, props, "ScalingMax", "Vector3D", "Vector", "", AttributeValue::F64(0.0));
                    push_prop(t, props, "DefaultAttributeIndex", "int", "Integer", "", AttributeValue::I32(0));
                    push_prop_vec3(
                        t,
                        props,
                        "Lcl Translation",
                        "Lcl Translation",
                        "",
                        "A",
                        translation,
                    );
                    push_prop_vec3(
                        t,
                        props,
                        "Lcl Rotation",
                        "Lcl Rotation",
                        "",
                        "A",
                        rotation_deg,
                    );
                    push_prop_vec3(t, props, "Lcl Scaling", "Lcl Scaling", "", "A", scale);
                    push_prop(t, props, "RotationOrder", "enum", "", "", AttributeValue::I32(0));

                    if !kind.is_empty() && kind != name {
                        push_prop(
                            t,
                            props,
                            "mogen_kind",
                            "KString",
                            "",
                            "U",
                            AttributeValue::String(kind.clone()),
                        );
                    }
                    if let Some(role) = &role {
                        push_prop(
                            t,
                            props,
                            "mogen_role",
                            "KString",
                            "",
                            "U",
                            AttributeValue::String(role.clone()),
                        );
                    }
                    if !tags.is_empty() {
                        push_prop(
                            t,
                            props,
                            "mogen_tags",
                            "KString",
                            "",
                            "U",
                            AttributeValue::String(tags.join(",")),
                        );
                    }
                    if !cast_shadow {
                        push_prop(
                            t,
                            props,
                            "CastShadow",
                            "bool",
                            "",
                            "",
                            AttributeValue::I32(0),
                        );
                    }
                });

                // Rendering-relevant defaults that sit outside Properties70
                // in FBX. Blender ignores most, but emitting them keeps the
                // file structurally identical to what the SDK produces.
                let mt = tree.append_new(m, "MultiLayer");
                tree.append_attribute(mt, 0i32);
                let mta = tree.append_new(m, "MultiTake");
                tree.append_attribute(mta, 0i32);
                let shading = tree.append_new(m, "Shading");
                tree.append_attribute(shading, true);
                let culling = tree.append_new(m, "Culling");
                tree.append_attribute(culling, "CullingOff");
            }),
        );

        // Parent connection. Roots connect to the implicit RootNode (id=0).
        let parent_id = match n.parent {
            Some(pid) => model_ids[pid.0 as usize],
            None => 0,
        };
        emit.connect_oo(model_id, parent_id);
    }

    model_ids
}
