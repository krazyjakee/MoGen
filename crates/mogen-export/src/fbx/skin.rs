//! Skin export — `Skin` → `Deformer` (subclass `Skin`) + per-joint
//! `Deformer` (subclass `Cluster`).
//!
//! FBX 7.4 skinning model:
//!
//! ```text
//! Geometry --OO-- Skin Deformer
//!                 │
//!                 ├─ Cluster (joint 0) ──OO─→ joint Model
//!                 ├─ Cluster (joint 1) ──OO─→ joint Model
//!                 └─ …
//! ```
//!
//! Each Cluster carries:
//! - `Indexes` — i32 array of vertex indices this joint deforms.
//! - `Weights` — f64 array, same length as `Indexes`.
//! - `Transform` — 16-element column-major matrix: the inverse-bind
//!   matrix supplied by `mogen_core::Skin.inverse_bind_matrices[i]`.
//! - `TransformLink` — 16-element column-major matrix: the joint's world
//!   bind transform (i.e. `inverse(Transform)`).
//!
//! We compute `TransformLink` by inverting `Transform`. The GLB
//! exporter passes the inverse-bind matrices straight through to glTF;
//! both formats expect the same column-major glam memory layout.

use std::collections::HashMap;

use fbxcel::low::v7400::AttributeValue;
use glam::Mat4;

use mogen_core::SceneGraph;

use super::doc::ObjectEmitter;
use super::ids::IdAllocator;
use super::mesh::MeshTable;

pub(super) fn emit_skins(
    scene: &SceneGraph,
    model_ids: &[i64],
    mesh_table: &MeshTable,
    ids: &mut IdAllocator,
    emit: &mut ObjectEmitter,
) {
    if scene.skins.is_empty() {
        return;
    }

    // Map: skin index → list of geometry ids that bind to it. A skin
    // with no bound mesh is silently dropped — there is no FBX object
    // to attach the deformer to.
    let mut geometries_per_skin: HashMap<u32, Vec<i64>> = HashMap::new();
    for (i, n) in scene.nodes.iter().enumerate() {
        if let (Some(skin_id), Some(geom_id)) = (n.skin, mesh_table.geometry_id_for_node[i]) {
            geometries_per_skin
                .entry(skin_id.0)
                .or_default()
                .push(geom_id);
        }
    }

    for (skin_idx, skin) in scene.skins.iter().enumerate() {
        let geometry_ids = match geometries_per_skin.get(&(skin_idx as u32)) {
            Some(g) if !g.is_empty() => g.clone(),
            _ => continue,
        };

        let skin_id = ids.alloc();
        let skin_name = skin.name.clone();

        emit.push_object(
            "Deformer",
            Box::new(move |tree, parent| {
                let d = tree.append_new(parent, "Deformer");
                tree.append_attribute(d, skin_id);
                tree.append_attribute(d, format!("{skin_name}\u{0}\u{1}Deformer"));
                tree.append_attribute(d, "Skin");

                let v = tree.append_new(d, "Version");
                tree.append_attribute(v, 101i32);
                let lr = tree.append_new(d, "Link_DeformAcuracy");
                tree.append_attribute(lr, 50.0_f64);
            }),
        );
        for geom_id in &geometry_ids {
            emit.connect_oo(skin_id, *geom_id);
        }

        // Per-joint clusters. Per-vertex influences come from the mesh
        // bound on every node in `geometries_per_skin` — but we expect
        // every bound mesh to share the same per-vertex layout (joint
        // ids index into `skin.joints`, not into something mesh-local).
        // For multi-mesh skins (rare) we collapse to "any mesh that
        // mentions joint J in row R" which mirrors the GLB exporter's
        // assumption that `Mesh.joints` rows are one-to-one with
        // `Mesh.positions` for the bound mesh.
        for (joint_idx, joint_node) in skin.joints.iter().enumerate() {
            let cluster_id = ids.alloc();
            let joint_model_id = match model_ids.get(joint_node.0 as usize) {
                Some(&m) => m,
                None => continue,
            };

            // Collect (vertex_index, weight) entries for this joint
            // across every bound mesh. Skip zero-weight rows so the
            // arrays stay tight and importers don't ignore real data.
            let mut indices: Vec<i32> = Vec::new();
            let mut weights: Vec<f64> = Vec::new();
            for (i, n) in scene.nodes.iter().enumerate() {
                let (Some(skin_ref), Some(_)) = (n.skin, mesh_table.geometry_id_for_node[i]) else {
                    continue;
                };
                if skin_ref.0 != skin_idx as u32 {
                    continue;
                }
                let Some(mesh) = &n.mesh else { continue };
                if !mesh.is_skinned() {
                    continue;
                }
                for (vi, (joints_row, weights_row)) in
                    mesh.joints.iter().zip(mesh.weights.iter()).enumerate()
                {
                    for slot in 0..4 {
                        if joints_row[slot] as usize == joint_idx && weights_row[slot] > 0.0 {
                            indices.push(vi as i32);
                            weights.push(weights_row[slot] as f64);
                        }
                    }
                }
            }

            // Flatten the inverse-bind matrix (column-major glam) into
            // a 16-element f64 array. `TransformLink` is the world-bind
            // (inverse of inverse-bind).
            let ibm: Mat4 = {
                let m = skin.inverse_bind_matrices[joint_idx];
                Mat4::from_cols_array_2d(&m)
            };
            let inv: Mat4 = ibm.inverse();

            let transform_arr: Vec<f64> = ibm
                .to_cols_array()
                .iter()
                .map(|f| *f as f64)
                .collect();
            let transform_link_arr: Vec<f64> = inv
                .to_cols_array()
                .iter()
                .map(|f| *f as f64)
                .collect();

            let cluster_name = format!("{}_{}", skin.name, joint_idx);

            emit.push_object(
                "Deformer",
                Box::new(move |tree, parent| {
                    let c = tree.append_new(parent, "Deformer");
                    tree.append_attribute(c, cluster_id);
                    tree.append_attribute(c, format!("{cluster_name}\u{0}\u{1}SubDeformer"));
                    tree.append_attribute(c, "Cluster");

                    let v = tree.append_new(c, "Version");
                    tree.append_attribute(v, 100i32);
                    let um = tree.append_new(c, "UserData");
                    tree.append_attribute(um, "");
                    tree.append_attribute(um, "");

                    let ix = tree.append_new(c, "Indexes");
                    tree.append_attribute(ix, AttributeValue::ArrI32(indices));
                    let w = tree.append_new(c, "Weights");
                    tree.append_attribute(w, AttributeValue::ArrF64(weights));

                    let tr = tree.append_new(c, "Transform");
                    tree.append_attribute(tr, AttributeValue::ArrF64(transform_arr));
                    let tl = tree.append_new(c, "TransformLink");
                    tree.append_attribute(tl, AttributeValue::ArrF64(transform_link_arr));
                }),
            );

            emit.connect_oo(cluster_id, skin_id);
            emit.connect_oo(joint_model_id, cluster_id);
        }
    }
}
