//! Skin export — `Skin` → `Deformer` (subclass `Skin`) + per-joint
//! `Deformer` (subclass `Cluster`).
//!
//! FBX 7.4 skinning model:
//!
//! ```text
//! Geometry <--OO-- Skin Deformer
//!                  ↑
//!                  │ OO (cluster is the source/child of the skin)
//!                  │
//!                  ├─ Cluster (joint 0) --OO--> joint Model
//!                  ├─ Cluster (joint 1) --OO--> joint Model
//!                  └─ …
//! ```
//!
//! Connection arrows in this diagram point from source (child) to
//! destination (parent). FBX `OO` rows are written as
//! `(source, destination)`, so a `Cluster --OO--> joint Model` connection
//! is `connect_oo(cluster_id, joint_model_id)`.
//!
//! **Per-geometry scoping.** A Cluster's `Indexes` array is interpreted
//! relative to the geometry the Skin Deformer is connected to. We
//! therefore emit one Skin Deformer (and its full set of joint Clusters)
//! per (skin, bound geometry) pair — sharing a Skin across multiple
//! geometries in FBX would conflate vertex-id namespaces.
//!
//! Each Cluster carries:
//! - `Indexes` — i32 array of vertex indices in the bound geometry.
//! - `Weights` — f64 array, same length as `Indexes`.
//! - `Transform` — 16-element column-major matrix: the inverse-bind
//!   matrix supplied by `mogen_core::Skin.inverse_bind_matrices[i]`.
//! - `TransformLink` — 16-element column-major matrix: the joint's world
//!   bind transform, derived as the inverse of `Transform`.

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

    // Walk every (mesh-bearing node, skin reference) pair. Each becomes its
    // own Skin Deformer + per-joint Cluster set so the cluster Indexes
    // never cross geometry boundaries.
    for (i, n) in scene.nodes.iter().enumerate() {
        let skin_ref = match n.skin {
            Some(s) => s,
            None => continue,
        };
        let geom_id = match mesh_table.geometry_id_for_node[i] {
            Some(g) => g,
            None => continue,
        };
        let skin = match scene.skins.get(skin_ref.0 as usize) {
            Some(s) => s,
            None => continue,
        };
        let mesh = match &n.mesh {
            Some(m) => m,
            None => continue,
        };
        if !mesh.is_skinned() {
            // Carrying a skin reference but no per-vertex influences is a
            // semantic mismatch upstream of us; the GLB pipeline silently
            // drops such nodes from skinning, so do the same here.
            continue;
        }

        let skin_id = ids.alloc();
        let skin_name = format!("{}_{}", skin.name, n.name);

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
        // Skin Deformer is connected as the child of the Geometry. The
        // Geometry node is the destination; the Skin's own children
        // (Clusters) connect with the Skin as their destination.
        emit.connect_oo(skin_id, geom_id);

        for (joint_idx, joint_node) in skin.joints.iter().enumerate() {
            let cluster_id = ids.alloc();
            let joint_model_id = match model_ids.get(joint_node.0 as usize) {
                Some(&m) => m,
                None => continue,
            };

            // Collect (vertex_index, weight) entries for this joint from
            // *this* mesh only. Zero-weight slots are skipped so importers
            // don't waste cycles on no-op influences.
            let mut indices: Vec<i32> = Vec::new();
            let mut weights: Vec<f64> = Vec::new();
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

            // Flatten the inverse-bind matrix (column-major glam) into a
            // 16-element f64 array — that's what FBX `Transform` expects
            // (joint's geometry-space → world-space matrix at bind, i.e.
            // the inverse of the world-space joint transform).
            // `TransformLink` is the joint's world bind transform, which
            // is the inverse of `Transform`.
            let ibm: Mat4 = {
                let m = skin.inverse_bind_matrices[joint_idx];
                Mat4::from_cols_array_2d(&m)
            };
            let inv: Mat4 = ibm.inverse();

            let transform_arr: Vec<f64> =
                ibm.to_cols_array().iter().map(|f| *f as f64).collect();
            let transform_link_arr: Vec<f64> =
                inv.to_cols_array().iter().map(|f| *f as f64).collect();

            let cluster_name = format!("{}_{}_{}", skin.name, n.name, joint_idx);

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

            // Cluster is the child of both the Skin and the joint Model
            // (FBX models a Cluster as the bridge between them). Both
            // edges therefore have `cluster_id` as the source.
            emit.connect_oo(cluster_id, skin_id);
            emit.connect_oo(cluster_id, joint_model_id);
        }
    }
}
