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

use mogen_core::Mesh;

/// Per-LOD index list. The positions / normals / UVs of the source mesh are
/// reused as-is — the simplifier only produces a new index buffer, so
/// returning just indices avoids cloning O(|V|) vertex data per stage.
pub(crate) type LodIndices = Vec<u32>;

/// Target triangle ratios for the three LOD stages, in descending detail.
/// LOD0 (the original) is always emitted at 100%; these fractions drive
/// LOD1, LOD2, and LOD3 respectively.
pub(crate) const LOD_RATIOS: [f32; 3] = [0.5, 0.25, 0.12];

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

/// Produce up to three progressively simpler index lists for `source`.
/// Returns an empty vector when LOD generation is skipped (skinned mesh,
/// too small, or the simplifier couldn't reduce further); callers reuse the
/// source mesh's position / normal / UV accessors and only swap the index
/// buffer per LOD stage.
pub(crate) fn build_lod_meshes(source: &Mesh) -> Vec<LodIndices> {
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

        out.push(new_indices);
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
        for indices in &lods {
            assert!(
                indices.len() < prev,
                "LOD {} did not reduce ({} >= {})",
                indices.len(),
                indices.len(),
                prev
            );
            prev = indices.len();
        }
    }

    #[test]
    fn small_mesh_skips_lod_generation() {
        let m = dense_grid(4); // 16 verts, 18 tris — under MIN_TRIS_FOR_LOD.
        let lods = build_lod_meshes(&m);
        assert!(lods.is_empty(), "small meshes should skip LOD generation");
    }

    #[test]
    fn skinned_mesh_skips_lod_generation() {
        let mut m = dense_grid(20);
        m.joints = vec![[0, 0, 0, 0]; m.positions.len()];
        m.weights = vec![[1.0, 0.0, 0.0, 0.0]; m.positions.len()];
        assert!(m.is_skinned());
        assert!(
            build_lod_meshes(&m).is_empty(),
            "skinned meshes should skip LOD generation"
        );
    }
}
