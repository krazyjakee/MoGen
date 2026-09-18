//! The conversion stage: an ordered list of passes over the [`MeshScene`].
//!
//! Today the passes only make the scene *emittable* — `poly` rejects
//! out-of-range indices and needs at least one triangle, and there is no point
//! writing a group with nothing under it. This is also where recovery passes
//! (primitive fitting, repetition and symmetry detection, CSG recovery) slot
//! in: each is a [`Pass`] that rewrites part of the IR and records what it did
//! in the [`Report`].
//!
//! Passes run in the order given and must be deterministic: iterate `Vec`s,
//! never maps.

use crate::ir::{MeshScene, Primitive};
use crate::Report;

pub trait Pass {
    fn name(&self) -> &'static str;
    fn run(&self, scene: &mut MeshScene, report: &mut Report);
}

/// The passes `mogen import` runs, in order.
pub fn default_passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(DropInvalidTriangles),
        Box::new(CompactVertices),
        Box::new(PruneEmpty),
    ]
}

pub fn run(scene: &mut MeshScene, report: &mut Report, passes: &[Box<dyn Pass>]) {
    for pass in passes {
        pass.run(scene, report);
    }
}

/// Remove triangles `poly` cannot represent or that draw nothing: an index out
/// of range, a vertex repeated within the triangle, or a non-finite position.
pub struct DropInvalidTriangles;

impl Pass for DropInvalidTriangles {
    fn name(&self) -> &'static str {
        "drop-invalid-triangles"
    }

    fn run(&self, scene: &mut MeshScene, report: &mut Report) {
        let mut dropped = 0;
        for prim in scene
            .meshes
            .iter_mut()
            .flat_map(|m| m.primitives.iter_mut())
        {
            let n = prim.positions.len() as u32;
            let finite = |i: u32| prim.positions[i as usize].iter().all(|c| c.is_finite());
            let mut kept = Vec::with_capacity(prim.indices.len());
            for tri in prim.indices.as_chunks::<3>().0 {
                let [a, b, c] = *tri;
                let ok = a < n
                    && b < n
                    && c < n
                    && a != b
                    && b != c
                    && a != c
                    && finite(a)
                    && finite(b)
                    && finite(c);
                if ok {
                    kept.extend_from_slice(tri);
                } else {
                    dropped += 1;
                }
            }
            // A trailing partial triangle is not a triangle.
            dropped += usize::from(!prim.indices.len().is_multiple_of(3));
            prim.indices = kept;
        }
        if dropped > 0 {
            report.note(format!(
                "dropped {dropped} degenerate or out-of-range triangle(s)"
            ));
        }
    }
}

/// Keep only the vertices the triangles use, renumbered in first-use order.
///
/// Readers expand non-indexed geometry and glTF buffers are often shared
/// between primitives, so without this a `poly` could carry thousands of
/// points it never draws. Also drops a normal or UV channel whose length
/// does not match the positions, which `poly` would reject outright.
pub struct CompactVertices;

impl Pass for CompactVertices {
    fn name(&self) -> &'static str {
        "compact-vertices"
    }

    fn run(&self, scene: &mut MeshScene, report: &mut Report) {
        for prim in scene
            .meshes
            .iter_mut()
            .flat_map(|m| m.primitives.iter_mut())
        {
            if !prim.uvs.is_empty() && prim.uvs.len() != prim.positions.len() {
                report.note("dropped a UV channel whose length did not match its positions");
                prim.uvs.clear();
            }
            if !prim.normals.is_empty() && prim.normals.len() != prim.positions.len() {
                prim.normals.clear();
            }
            compact(prim);
        }
    }
}

fn compact(prim: &mut Primitive) {
    const UNSEEN: u32 = u32::MAX;
    let mut remap = vec![UNSEEN; prim.positions.len()];
    let mut order: Vec<usize> = Vec::new();
    for idx in prim.indices.iter_mut() {
        let old = *idx as usize;
        if remap[old] == UNSEEN {
            remap[old] = order.len() as u32;
            order.push(old);
        }
        *idx = remap[old];
    }
    if order.len() == prim.positions.len() && order.iter().enumerate().all(|(i, &o)| i == o) {
        return;
    }
    prim.positions = order.iter().map(|&o| prim.positions[o]).collect();
    if !prim.normals.is_empty() {
        prim.normals = order.iter().map(|&o| prim.normals[o]).collect();
    }
    if !prim.uvs.is_empty() {
        prim.uvs = order.iter().map(|&o| prim.uvs[o]).collect();
    }
}

/// Drop primitives with no triangles, then nodes whose whole subtree has no
/// geometry (cameras, lights, empties, bones' helper nodes), then meshes and
/// materials nothing references any more. Indices are renumbered with order
/// preserved.
pub struct PruneEmpty;

impl Pass for PruneEmpty {
    fn name(&self) -> &'static str {
        "prune-empty"
    }

    fn run(&self, scene: &mut MeshScene, report: &mut Report) {
        for mesh in &mut scene.meshes {
            mesh.primitives.retain(|p| p.triangle_count() > 0);
        }

        // Which nodes have geometry at or below them. Nothing orders children
        // after their parents in the Vec, so recurse from the roots.
        let mut keep = vec![false; scene.nodes.len()];
        fn mark(scene: &MeshScene, i: usize, keep: &mut [bool], depth: usize) -> bool {
            if depth > scene.nodes.len() {
                return false; // a cycle; readers should never produce one
            }
            let node = &scene.nodes[i];
            let mut any = node
                .mesh
                .is_some_and(|m| !scene.meshes[m].primitives.is_empty());
            for &c in &node.children {
                any |= mark(scene, c, keep, depth + 1);
            }
            keep[i] = any;
            any
        }
        for &r in &scene.roots {
            mark(scene, r, &mut keep, 0);
        }
        let pruned = keep.iter().filter(|k| !**k).count();
        if pruned > 0 {
            report.skip_n("nodes without geometry", pruned);
        }

        // Renumber nodes.
        let mut node_map = vec![usize::MAX; scene.nodes.len()];
        let mut nodes = Vec::new();
        for (i, node) in scene.nodes.iter().enumerate() {
            if keep[i] {
                node_map[i] = nodes.len();
                nodes.push(node.clone());
            }
        }
        for node in &mut nodes {
            node.children = node
                .children
                .iter()
                .filter(|&&c| keep[c])
                .map(|&c| node_map[c])
                .collect();
            if node
                .mesh
                .is_some_and(|m| scene.meshes[m].primitives.is_empty())
            {
                node.mesh = None;
            }
        }
        scene.roots = scene
            .roots
            .iter()
            .filter(|&&r| keep[r])
            .map(|&r| node_map[r])
            .collect();

        // Renumber meshes.
        let mut mesh_used = vec![false; scene.meshes.len()];
        for node in &nodes {
            if let Some(m) = node.mesh {
                mesh_used[m] = true;
            }
        }
        let mut mesh_map = vec![usize::MAX; scene.meshes.len()];
        let mut meshes = Vec::new();
        for (i, mesh) in scene.meshes.drain(..).enumerate() {
            if mesh_used[i] {
                mesh_map[i] = meshes.len();
                meshes.push(mesh);
            }
        }
        for node in &mut nodes {
            node.mesh = node.mesh.map(|m| mesh_map[m]);
        }

        // Renumber materials.
        let mut mat_used = vec![false; scene.materials.len()];
        for p in meshes.iter().flat_map(|m| &m.primitives) {
            if let Some(m) = p.material {
                mat_used[m] = true;
            }
        }
        let mut mat_map = vec![usize::MAX; scene.materials.len()];
        let mut materials = Vec::new();
        for (i, mat) in scene.materials.drain(..).enumerate() {
            if mat_used[i] {
                mat_map[i] = materials.len();
                materials.push(mat);
            }
        }
        for p in meshes.iter_mut().flat_map(|m| m.primitives.iter_mut()) {
            p.material = p.material.map(|m| mat_map[m]);
        }

        scene.nodes = nodes;
        scene.meshes = meshes;
        scene.materials = materials;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{MeshData, Node};
    use glam::{Quat, Vec3};

    fn node(mesh: Option<usize>, children: Vec<usize>) -> Node {
        Node {
            source_name: None,
            name: String::new(),
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
            mesh,
            children,
            role: None,
            tags: Vec::new(),
        }
    }

    fn tri_prim(indices: Vec<u32>) -> Primitive {
        Primitive {
            positions: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [9.0; 3]],
            indices,
            ..Default::default()
        }
    }

    #[test]
    fn degenerate_triangles_are_dropped_and_vertices_compacted() {
        let mut scene = MeshScene {
            nodes: vec![node(Some(0), vec![])],
            roots: vec![0],
            meshes: vec![MeshData {
                primitives: vec![tri_prim(vec![2, 1, 0, 0, 0, 1, 0, 1, 7])],
                ..Default::default()
            }],
            materials: vec![],
        };
        let mut report = Report::default();
        run(&mut scene, &mut report, &default_passes());
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.indices, vec![0, 1, 2]);
        assert_eq!(
            p.positions,
            vec![[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0; 3]]
        );
        assert!(report.notes[0].contains("2 degenerate"));
    }

    #[test]
    fn empty_subtrees_are_pruned_and_indices_renumbered() {
        // 0: empty root with an empty child (1) and a mesh child (2).
        // 3: a second root with nothing.
        let mut scene = MeshScene {
            nodes: vec![
                node(None, vec![1, 2]),
                node(None, vec![]),
                node(Some(1), vec![]),
                node(Some(0), vec![]),
            ],
            roots: vec![0, 3],
            meshes: vec![
                MeshData {
                    primitives: vec![tri_prim(vec![0, 0, 0])],
                    ..Default::default()
                },
                MeshData {
                    primitives: vec![tri_prim(vec![0, 1, 2])],
                    ..Default::default()
                },
            ],
            materials: vec![],
        };
        let mut report = Report::default();
        run(&mut scene, &mut report, &default_passes());
        assert_eq!(scene.nodes.len(), 2);
        assert_eq!(scene.roots, vec![0]);
        assert_eq!(scene.nodes[0].children, vec![1]);
        assert_eq!(scene.nodes[1].mesh, Some(0));
        assert_eq!(scene.meshes.len(), 1);
        assert_eq!(
            report.skipped,
            vec![("nodes without geometry".to_string(), 2)]
        );
    }
}
