use anyhow::{bail, Result};
use mogen_core::Mesh;

use crate::cleanup::recompute_normals;

/// Raw triangle mesh from explicit vertex positions, UVs, and triangle indices.
///
/// This is the escape hatch for geometry that no parametric primitive captures
/// and whose texture mapping must be carried through *exactly* — e.g. a map
/// converter re-expressing an engine's native blocks, where each face already
/// has authored per-vertex UVs (an atlas sub-rectangle, a sign, a panel) that
/// a procedural `Tile`/`Fit` projection can never reproduce. Unlike every other
/// primitive, `poly` ignores `UvMode`: the supplied `uvs` are the UVs.
///
/// Normals are recomputed (area-weighted) from the triangle winding, so the
/// caller decides flat vs. smooth purely by how it shares vertices: give each
/// flat face its own non-shared corners and the averaged normal collapses to
/// that face's plane (flat shading); share corners across faces and adjacent
/// faces blend (smooth shading).
///
/// `uvs` may be empty (no UV channel — solid-colour materials only); otherwise
/// it must match `positions` in length. `indices` must be a non-empty multiple
/// of three and stay in range.
pub fn poly_mesh(
    positions: &[[f32; 3]],
    uvs: &[[f32; 2]],
    indices: &[u32],
) -> Result<Mesh> {
    if positions.len() < 3 {
        bail!(
            "`poly` requires at least 3 vertices in `points=[[x,y,z], …]`, got {}",
            positions.len()
        );
    }
    if !uvs.is_empty() && uvs.len() != positions.len() {
        bail!(
            "`poly` `uvs=` length ({}) must equal `points=` length ({}) — one UV per vertex",
            uvs.len(),
            positions.len()
        );
    }
    if indices.is_empty() || indices.len() % 3 != 0 {
        bail!(
            "`poly` `indices=` length ({}) must be a non-zero multiple of 3 (one per triangle corner)",
            indices.len()
        );
    }
    let n = positions.len() as u32;
    if let Some(&bad) = indices.iter().find(|&&i| i >= n) {
        bail!(
            "`poly` `indices=` references vertex {bad}, but only {n} points were given (valid range 0..{})",
            n - 1
        );
    }
    let base = Mesh {
        positions: positions.to_vec(),
        normals: vec![[0.0, 0.0, 0.0]; positions.len()],
        uvs: uvs.to_vec(),
        indices: indices.to_vec(),
        ..Default::default()
    };
    Ok(recompute_normals(&base))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poly_carries_explicit_uvs_verbatim() {
        // A single quad with an atlas sub-rectangle UV that no projection
        // would invent: U in [0.25, 0.5], V in [0.5, 0.75].
        let positions = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let uvs = [[0.25, 0.5], [0.5, 0.5], [0.5, 0.75], [0.25, 0.75]];
        let indices = [0, 1, 2, 0, 2, 3];
        let m = poly_mesh(&positions, &uvs, &indices).unwrap();
        assert_eq!(m.uvs, uvs, "explicit UVs must survive unchanged");
        assert!(m.has_uvs());
        // Flat quad in the XY plane → every normal is ±Z.
        for nrm in &m.normals {
            assert!(nrm[0].abs() < 1e-5 && nrm[1].abs() < 1e-5);
            assert!((nrm[2].abs() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn poly_rejects_mismatched_uv_count() {
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]];
        let uvs = [[0.0, 0.0], [1.0, 0.0]]; // one short
        assert!(poly_mesh(&positions, &uvs, &[0, 1, 2]).is_err());
    }

    #[test]
    fn poly_rejects_out_of_range_index() {
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]];
        assert!(poly_mesh(&positions, &[], &[0, 1, 9]).is_err());
    }

    #[test]
    fn poly_allows_no_uv_channel() {
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]];
        let m = poly_mesh(&positions, &[], &[0, 1, 2]).unwrap();
        assert!(!m.has_uvs(), "empty uvs means no UV channel");
    }
}
