//! `Mesh` → `Geometry` Object emission.
//!
//! Each `SceneNode` that carries a mesh produces one `Geometry` object in
//! the FBX `Objects` block, connected via OO to its owning `Model`. We do
//! not deduplicate on a (positions, indices, material) key here the way
//! the GLB exporter does — FBX skinning attaches `Deformer` Clusters by
//! Geometry id, so sharing a Geometry between two skinned-mesh nodes
//! would silently cross-wire their deformations. Sharing for non-skinned
//! meshes is a future optimisation; for now the tree carries one Geometry
//! per mesh-bearing node, matching what the official Autodesk exporter
//! produces.
//!
//! Triangle list encoding: FBX `PolygonVertexIndex` is a single i32 array
//! that runs every polygon's vertex ids back-to-back. Polygons end at the
//! last entry, which the spec marks by negate-and-decrement (`!idx`,
//! equivalent to `-(idx + 1)` in two's complement). For triangle lists
//! that means every third index has the high bit set.


use fbxcel::low::v7400::AttributeValue;

use mogen_core::SceneGraph;

use super::doc::ObjectEmitter;
use super::ids::IdAllocator;

/// Per-`SceneNode` Geometry object id, or `None` when the node has no mesh.
/// Index-aligned with `scene.nodes`. Skin and material connections look
/// up Geometry ids through this table.
pub(super) struct MeshTable {
    pub geometry_id_for_node: Vec<Option<i64>>,
}

pub(super) fn emit_geometries(
    scene: &SceneGraph,
    model_ids: &[i64],
    ids: &mut IdAllocator,
    emit: &mut ObjectEmitter,
) -> MeshTable {
    let mut geometry_id_for_node: Vec<Option<i64>> = vec![None; scene.nodes.len()];

    for (i, n) in scene.nodes.iter().enumerate() {
        let mesh = match &n.mesh {
            Some(m) => m,
            None => continue,
        };
        let model_id = model_ids[i];
        let geom_id = ids.alloc();
        geometry_id_for_node[i] = Some(geom_id);


        // Convert positions / normals / indices / uvs into FBX-friendly
        // owned buffers up front, then move them into the closure.
        let vertices: Vec<f64> = mesh
            .positions
            .iter()
            .flat_map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
            .collect();

        // `PolygonVertexIndex` runs per-polygon, with the last index of
        // each polygon negate-and-decremented. For our triangle lists
        // that means every third entry.
        let mut polygon_vertex_index: Vec<i32> = Vec::with_capacity(mesh.indices.len());
        for tri in mesh.indices.chunks_exact(3) {
            let a = tri[0] as i32;
            let b = tri[1] as i32;
            let c = tri[2] as i32;
            polygon_vertex_index.push(a);
            polygon_vertex_index.push(b);
            // `!c` in two's complement equals `-(c + 1)`.
            polygon_vertex_index.push(!c);
        }

        let normals: Vec<f64> = mesh
            .normals
            .iter()
            .flat_map(|n| [n[0] as f64, n[1] as f64, n[2] as f64])
            .collect();

        // UV layer is optional — empty `uvs` skips the LayerElementUV
        // emission entirely. Note this passes the raw UVs through; the
        // `uv_scale` material multiplier is *not* applied here, matching
        // the FBX convention that material UV transforms live on the
        // material, not baked into vertex data. The GLB exporter bakes it
        // because glTF's KHR_texture_transform support is uneven.
        let uvs: Vec<f64> = mesh
            .uvs
            .iter()
            .flat_map(|uv| [uv[0] as f64, uv[1] as f64])
            .collect();
        let has_uvs = !uvs.is_empty();

        let geom_name = n.name.clone();

        emit.push_object(
            "Geometry",
            Box::new(move |tree, parent| {
                let g = tree.append_new(parent, "Geometry");
                tree.append_attribute(g, geom_id);
                tree.append_attribute(g, format!("{geom_name}\u{0}\u{1}Geometry"));
                tree.append_attribute(g, "Mesh");

                let gver = tree.append_new(g, "GeometryVersion");
                tree.append_attribute(gver, 124i32);

                let v = tree.append_new(g, "Vertices");
                tree.append_attribute(v, AttributeValue::ArrF64(vertices));

                let pvi = tree.append_new(g, "PolygonVertexIndex");
                tree.append_attribute(pvi, AttributeValue::ArrI32(polygon_vertex_index));

                // LayerElementNormal — direct mapping, one normal per
                // vertex. Matches our Mesh storage exactly.
                let lne = tree.append_new(g, "LayerElementNormal");
                tree.append_attribute(lne, 0i32);
                let v = tree.append_new(lne, "Version");
                tree.append_attribute(v, 102i32);
                let n = tree.append_new(lne, "Name");
                tree.append_attribute(n, "");
                let mit = tree.append_new(lne, "MappingInformationType");
                tree.append_attribute(mit, "ByVertice");
                let rit = tree.append_new(lne, "ReferenceInformationType");
                tree.append_attribute(rit, "Direct");
                let nrm = tree.append_new(lne, "Normals");
                tree.append_attribute(nrm, AttributeValue::ArrF64(normals));

                if has_uvs {
                    let leu = tree.append_new(g, "LayerElementUV");
                    tree.append_attribute(leu, 0i32);
                    let v = tree.append_new(leu, "Version");
                    tree.append_attribute(v, 101i32);
                    let n = tree.append_new(leu, "Name");
                    tree.append_attribute(n, "UVMap");
                    let mit = tree.append_new(leu, "MappingInformationType");
                    tree.append_attribute(mit, "ByVertice");
                    let rit = tree.append_new(leu, "ReferenceInformationType");
                    tree.append_attribute(rit, "Direct");
                    let uv = tree.append_new(leu, "UV");
                    tree.append_attribute(uv, AttributeValue::ArrF64(uvs));
                }

                // Layer descriptor that wires the Normal/UV layers we just
                // emitted into the renderer's expected slot indices.
                let layer = tree.append_new(g, "Layer");
                tree.append_attribute(layer, 0i32);
                let lver = tree.append_new(layer, "Version");
                tree.append_attribute(lver, 100i32);
                let le_n = tree.append_new(layer, "LayerElement");
                let n_type = tree.append_new(le_n, "Type");
                tree.append_attribute(n_type, "LayerElementNormal");
                let n_idx = tree.append_new(le_n, "TypedIndex");
                tree.append_attribute(n_idx, 0i32);
                if has_uvs {
                    let le_u = tree.append_new(layer, "LayerElement");
                    let u_type = tree.append_new(le_u, "Type");
                    tree.append_attribute(u_type, "LayerElementUV");
                    let u_idx = tree.append_new(le_u, "TypedIndex");
                    tree.append_attribute(u_idx, 0i32);
                }
            }),
        );

        emit.connect_oo(geom_id, model_id);
    }

    MeshTable {
        geometry_id_for_node,
    }
}
