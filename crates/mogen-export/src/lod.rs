//! Mesh-level LOD generation for the `bundle_lods_and_imposter` option.
//!
//! Each call to [`build_lod_meshes`] runs `meshopt::simplify` three times at
//! progressively lower triangle targets, producing the LOD1/LOD2/LOD3 stages
//! that the writer hangs off the source node via `MSFT_lod`. The original
//! mesh stays untouched as the LOD0 — it's emitted exactly as it would be
//! with the option off, so a viewer that doesn't understand `MSFT_lod` sees
//! the full-detail asset.
//!
//! Skinned meshes are skipped: `meshopt::simplify` collapses edges based on
//! position alone, which silently mangles `JOINTS_0` / `WEIGHTS_0` parallel
//! arrays and produces visually broken deformations. Static meshes are the
//! common case anyway — the building / furniture pipelines we ship target
//! exactly that population.

use mogen_core::{Mesh, SceneGraph};

/// Target triangle ratios for the three LOD stages, in descending detail.
/// LOD0 (the original) is always emitted at 100%; these fractions drive
/// LOD1, LOD2, and LOD3 respectively.
pub(crate) const LOD_RATIOS: [f32; 3] = [0.5, 0.25, 0.12];

/// Number of simplified LOD stages the `bundle_lods_and_imposter` export
/// attaches per mesh (LOD1..LOD3 — LOD0 is the untouched original). Exposed
/// so Studio's LOD preview can offer exactly the stages the export bundles.
pub const LOD_STAGE_COUNT: usize = LOD_RATIOS.len();

/// Screen-coverage thresholds stamped into `extras.MSFT_screencoverage`.
/// Per the spec this is parallel to `[source, LOD1, LOD2, LOD3]` — index 0
/// gates LOD0 (the source), entries 1..N gate each LOD. Values are the
/// minimum on-screen size below which the next-lower LOD takes over,
/// expressed as a fraction of the viewport. Tuned for typical
/// architectural-prop framing.
pub(crate) const SCREEN_COVERAGE: [f32; 4] = [0.5, 0.25, 0.1, 0.03];

/// Below this triangle count, simplification produces little real saving
/// and a one- or two-triangle LOD3 just wastes accessor entries. Treat
/// such meshes as "already small enough".
const MIN_TRIS_FOR_LOD: usize = 64;

/// Produce up to three progressively simpler copies of `source`. Returns an
/// empty vector when LOD generation is skipped (skinned mesh, too small, or
/// the simplifier couldn't reduce further); callers should fall back to
/// the LOD0-only path in that case.
pub(crate) fn build_lod_meshes(source: &Mesh) -> Vec<Mesh> {
    if source.is_skinned() {
        return Vec::new();
    }
    let tri_count = source.indices.len() / 3;
    if tri_count < MIN_TRIS_FOR_LOD {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(LOD_RATIOS.len());
    let mut prev_index_count = source.indices.len();

    for &ratio in &LOD_RATIOS {
        let raw_target = (source.indices.len() as f32 * ratio) as usize;
        // Triangle list: clamp to a multiple of 3.
        let target = (raw_target / 3) * 3;
        if target == 0 || target >= prev_index_count {
            break;
        }

        // `target_error` of 0.05 is relative to mesh extents — meshopt's
        // recommended default for "moderate quality" reductions. The
        // simplifier may produce fewer indices than requested if topology
        // forces it (locked borders etc.).
        let new_indices = meshopt::simplify_decoder(
            &source.indices,
            &source.positions,
            target,
            0.05,
            meshopt::SimplifyOptions::None,
            None,
        );
        if new_indices.is_empty() || new_indices.len() >= prev_index_count {
            // No further reduction possible (mesh is already as simple as
            // meshopt can make it under topology constraints). Stop rather
            // than emit identical LODs.
            break;
        }
        prev_index_count = new_indices.len();

        // Reuse the original vertex buffer; meshopt's simplifier returns
        // indices into the same position array. Unreferenced vertices stay
        // in the buffer — wasted bytes are bounded and the alternative
        // (`optimize_vertex_fetch`) would force a parallel remap of
        // normals/UVs we don't otherwise need.
        out.push(Mesh {
            positions: source.positions.clone(),
            normals: source.normals.clone(),
            uvs: source.uvs.clone(),
            joints: Vec::new(),
            weights: Vec::new(),
            indices: new_indices,
        });
    }

    out
}

/// Clone `scene`, swapping every mesh for its `stage`-th simplified LOD so
/// the result is exactly the geometry a `bundle_lods_and_imposter` export
/// would ship at that detail level. `stage` is 1-based: `1` → LOD1 (≈50%
/// tris), `2` → LOD2 (≈25%), `3` → LOD3 (≈12%). A node whose mesh is
/// skinned, too small, or can't be simplified that far keeps its original
/// geometry — the same per-mesh fallback the GLB writer applies, so the
/// preview never shows geometry the export wouldn't.
///
/// `stage == 0` (or out of range) returns a plain clone (full detail). Used
/// by Studio's viewport LOD preview; the export path itself attaches all
/// stages via `MSFT_lod` rather than collapsing to one.
pub fn scene_with_lod(scene: &SceneGraph, stage: usize) -> SceneGraph {
    let mut out = scene.clone();
    if stage == 0 || stage > LOD_STAGE_COUNT {
        return out;
    }
    for node in &mut out.nodes {
        let Some(mesh) = &node.mesh else { continue };
        let lods = build_lod_meshes(mesh);
        if let Some(reduced) = lods.into_iter().nth(stage - 1) {
            node.mesh = Some(reduced);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a UV-sphere-ish mesh dense enough to clear MIN_TRIS_FOR_LOD.
    /// We don't need a real sphere — just a triangle soup the simplifier
    /// can actually reduce.
    fn dense_grid(side: usize) -> Mesh {
        let mut positions = Vec::with_capacity(side * side);
        let mut normals = Vec::with_capacity(side * side);
        for y in 0..side {
            for x in 0..side {
                positions.push([x as f32, y as f32, 0.0]);
                normals.push([0.0, 0.0, 1.0]);
            }
        }
        let mut indices = Vec::with_capacity((side - 1) * (side - 1) * 6);
        for y in 0..(side - 1) {
            for x in 0..(side - 1) {
                let i = (y * side + x) as u32;
                let s = side as u32;
                indices.extend_from_slice(&[i, i + 1, i + s, i + 1, i + s + 1, i + s]);
            }
        }
        Mesh::new(positions, normals, indices)
    }

    #[test]
    fn lods_are_strictly_smaller_than_source() {
        let m = dense_grid(20); // 400 verts, ~722 tris — plenty of headroom.
        let lods = build_lod_meshes(&m);
        assert!(!lods.is_empty(), "expected at least one LOD for a dense mesh");
        let mut prev = m.indices.len();
        for lod in &lods {
            assert!(
                lod.indices.len() < prev,
                "LOD {} did not reduce ({} >= {})",
                lod.indices.len(),
                lod.indices.len(),
                prev
            );
            prev = lod.indices.len();
        }
    }

    #[test]
    fn small_mesh_skips_lod_generation() {
        let m = dense_grid(4); // 16 verts, 18 tris — under MIN_TRIS_FOR_LOD.
        let lods = build_lod_meshes(&m);
        assert!(lods.is_empty(), "small meshes should skip LOD generation");
    }

    #[test]
    fn scene_with_lod_swaps_dense_meshes_and_keeps_small_ones() {
        use mogen_core::{SceneGraph, Transform};
        let mut scene = SceneGraph::new();
        let big = scene.add_root("big", "box", Transform::default());
        scene.set_mesh(big, dense_grid(20));
        let small = scene.add_root("small", "box", Transform::default());
        scene.set_mesh(small, dense_grid(4));

        let src_big = scene.nodes[big.0 as usize].mesh.clone().unwrap();
        let src_small = scene.nodes[small.0 as usize].mesh.clone().unwrap();

        let lod1 = scene_with_lod(&scene, 1);
        let out_big = lod1.nodes[big.0 as usize].mesh.as_ref().unwrap();
        let out_small = lod1.nodes[small.0 as usize].mesh.as_ref().unwrap();
        assert!(
            out_big.indices.len() < src_big.indices.len(),
            "dense mesh should be simplified at LOD1"
        );
        assert_eq!(
            out_small.indices.len(),
            src_small.indices.len(),
            "sub-threshold mesh should keep its original geometry"
        );

        // Stage 0 / out of range = untouched full-detail clone.
        let full = scene_with_lod(&scene, 0);
        assert_eq!(
            full.nodes[big.0 as usize].mesh.as_ref().unwrap().indices.len(),
            src_big.indices.len()
        );
    }

    #[test]
    fn scene_with_lod_out_of_range_returns_full_detail() {
        use mogen_core::{SceneGraph, Transform};
        let mut scene = SceneGraph::new();
        let node = scene.add_root("big", "box", Transform::default());
        scene.set_mesh(node, dense_grid(20));
        let src_len = scene.nodes[node.0 as usize].mesh.as_ref().unwrap().indices.len();

        let out = scene_with_lod(&scene, LOD_STAGE_COUNT + 1);
        assert_eq!(
            out.nodes[node.0 as usize].mesh.as_ref().unwrap().indices.len(),
            src_len,
            "out-of-range stage should return full-detail clone"
        );
    }

    #[test]
    fn skinned_mesh_skips_lod_generation() {
        let mut m = dense_grid(20);
        m.joints = vec![[0, 0, 0, 0]; m.positions.len()];
        m.weights = vec![[1.0, 0.0, 0.0, 0.0]; m.positions.len()];
        assert!(m.is_skinned());
        let lods = build_lod_meshes(&m);
        assert!(lods.is_empty(), "skinned meshes should skip LOD generation");
    }
}
