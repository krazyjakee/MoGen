//! Loop subdivision for triangle meshes.
//!
//! Used by the `subdivide=N` post-pass on any mesh-producing node — most
//! valuably on `blob` outputs (where surface-nets produces a slightly
//! staircased mesh that one round of Loop smooths beautifully) and on raw
//! primitives the LLM wants to refine without re-tessellating.
//!
//! Each iteration replaces every triangle with four, then repositions every
//! vertex by a weighted average of its neighbours. After `N` iterations,
//! triangle count grows by 4^N — the lowering pass caps `N` to keep this in
//! check.

use std::collections::HashMap;

use glam::Vec3;
use mogen_core::Mesh;

use crate::cleanup::recompute_normals;

/// Apply `iterations` rounds of Loop subdivision. `iterations == 0` returns
/// a clone of the input untouched. Normals are recomputed at the end so the
/// result is shading-ready.
pub fn loop_subdivide(mesh: &Mesh, iterations: u32) -> Mesh {
    if iterations == 0 || mesh.indices.is_empty() {
        return mesh.clone();
    }
    let mut current = mesh.clone();
    for _ in 0..iterations {
        current = subdivide_once(&current);
    }
    recompute_normals(&current)
}

/// Key for the edge map. Vertex indices are sorted so `(a, b)` and `(b, a)`
/// collapse to the same key.
type EdgeKey = (u32, u32);

fn edge_key(a: u32, b: u32) -> EdgeKey {
    if a < b { (a, b) } else { (b, a) }
}

struct EdgeData {
    /// The two opposite-corner vertex ids across the (up to) two triangles
    /// sharing this edge. Boundary edges have only one entry.
    opposites: Vec<u32>,
}

fn subdivide_once(mesh: &Mesh) -> Mesh {
    let n_in_verts = mesh.positions.len();
    let has_uvs = mesh.uvs.len() == n_in_verts;

    // Pass 1: collect edge data and per-vertex neighbour set.
    let mut edges: HashMap<EdgeKey, EdgeData> = HashMap::new();
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); n_in_verts];

    let add_neighbour = |neighbours: &mut Vec<Vec<u32>>, v: u32, n: u32| {
        let nbs = &mut neighbours[v as usize];
        if !nbs.contains(&n) {
            nbs.push(n);
        }
    };

    for tri in mesh.indices.chunks_exact(3) {
        let (a, b, c) = (tri[0], tri[1], tri[2]);
        for &(u, v, opp) in &[(a, b, c), (b, c, a), (c, a, b)] {
            edges.entry(edge_key(u, v)).or_insert_with(|| EdgeData { opposites: Vec::new() })
                .opposites.push(opp);
        }
        add_neighbour(&mut neighbours, a, b);
        add_neighbour(&mut neighbours, a, c);
        add_neighbour(&mut neighbours, b, a);
        add_neighbour(&mut neighbours, b, c);
        add_neighbour(&mut neighbours, c, a);
        add_neighbour(&mut neighbours, c, b);
    }

    // Pass 2: compute boundary classification.
    // An edge is on the boundary if only one triangle references it.
    let mut is_boundary_vertex = vec![false; n_in_verts];
    for (key, data) in &edges {
        if data.opposites.len() == 1 {
            is_boundary_vertex[key.0 as usize] = true;
            is_boundary_vertex[key.1 as usize] = true;
        }
    }

    // Pass 3: allocate new vertex for every edge.
    let mut edge_to_new: HashMap<EdgeKey, u32> = HashMap::with_capacity(edges.len());
    let mut new_positions: Vec<[f32; 3]> = Vec::with_capacity(n_in_verts + edges.len());
    let mut new_uvs: Vec<[f32; 2]> = if has_uvs {
        Vec::with_capacity(n_in_verts + edges.len())
    } else {
        Vec::new()
    };

    // Slot the smoothed old-vertex positions first (indices 0..n_in_verts).
    for v in 0..n_in_verts {
        let p_old = Vec3::from_array(mesh.positions[v]);
        let p_new = if is_boundary_vertex[v] {
            // Boundary rule: 3/4 self + 1/8 of each neighbour that's also on
            // the boundary (there should be exactly two).
            let mut acc = Vec3::ZERO;
            let mut count = 0;
            for &n in &neighbours[v] {
                if is_boundary_vertex[n as usize]
                    && edges
                        .get(&edge_key(v as u32, n))
                        .map(|e| e.opposites.len() == 1)
                        .unwrap_or(false)
                {
                    acc += Vec3::from_array(mesh.positions[n as usize]);
                    count += 1;
                }
            }
            if count == 0 {
                p_old
            } else {
                p_old * 0.75 + acc * (1.0 / 8.0)
            }
        } else {
            let n = neighbours[v].len();
            if n == 0 {
                p_old
            } else {
                let beta = loop_beta(n);
                let mut acc = Vec3::ZERO;
                for &nb in &neighbours[v] {
                    acc += Vec3::from_array(mesh.positions[nb as usize]);
                }
                p_old * (1.0 - (n as f32) * beta) + acc * beta
            }
        };
        new_positions.push(p_new.to_array());
        if has_uvs {
            // UVs are not smoothed for old vertices — Loop's mask is for
            // positions; smoothing UVs would shift the texture pattern.
            new_uvs.push(mesh.uvs[v]);
        }
    }

    // Now allocate one new vertex per edge, with Loop's edge mask.
    for (&key, data) in &edges {
        let (a, b) = (key.0, key.1);
        let pa = Vec3::from_array(mesh.positions[a as usize]);
        let pb = Vec3::from_array(mesh.positions[b as usize]);
        let p = if data.opposites.len() == 2 {
            let pc = Vec3::from_array(mesh.positions[data.opposites[0] as usize]);
            let pd = Vec3::from_array(mesh.positions[data.opposites[1] as usize]);
            (pa + pb) * (3.0 / 8.0) + (pc + pd) * (1.0 / 8.0)
        } else {
            // Boundary edge: linear midpoint.
            (pa + pb) * 0.5
        };
        let new_idx = new_positions.len() as u32;
        new_positions.push(p.to_array());
        if has_uvs {
            // Linear midpoint UV (cheap and works for the bbox/planar UVs
            // primitives produce).
            let ua = mesh.uvs[a as usize];
            let ub = mesh.uvs[b as usize];
            new_uvs.push([(ua[0] + ub[0]) * 0.5, (ua[1] + ub[1]) * 0.5]);
        }
        edge_to_new.insert(key, new_idx);
    }

    // Pass 4: rebuild triangle list (each old tri → 4 new tris).
    let mut new_indices: Vec<u32> = Vec::with_capacity(mesh.indices.len() * 4);
    for tri in mesh.indices.chunks_exact(3) {
        let (a, b, c) = (tri[0], tri[1], tri[2]);
        let m_ab = edge_to_new[&edge_key(a, b)];
        let m_bc = edge_to_new[&edge_key(b, c)];
        let m_ca = edge_to_new[&edge_key(c, a)];
        new_indices.extend_from_slice(&[a, m_ab, m_ca]);
        new_indices.extend_from_slice(&[b, m_bc, m_ab]);
        new_indices.extend_from_slice(&[c, m_ca, m_bc]);
        new_indices.extend_from_slice(&[m_ab, m_bc, m_ca]);
    }

    // Normals get recomputed once at the end of `loop_subdivide`; we leave a
    // matching-length but zeroed normals array here to keep the Mesh field
    // shapes consistent for intermediate iterations.
    let normals = vec![[0.0_f32, 0.0, 0.0]; new_positions.len()];
    let uvs = if has_uvs {
        new_uvs
    } else {
        Vec::new()
    };

    Mesh {
        positions: new_positions,
        normals,
        uvs,
        indices: new_indices,
        ..Default::default()
    }
}

/// Loop's β for a vertex with `n` neighbours. Returns the per-neighbour
/// weight; the centre vertex weight is `1 - n*β`.
fn loop_beta(n: usize) -> f32 {
    if n == 3 {
        3.0 / 16.0
    } else {
        // Warren's modification (smoother, used by most modern subdiv impls).
        3.0 / (8.0 * n as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::sphere_mesh;
    use mogen_core::UvMode;

    #[test]
    fn zero_iters_returns_clone() {
        let s = sphere_mesh(0.5, 8, 12, UvMode::Fit);
        let r = loop_subdivide(&s, 0);
        assert_eq!(r.indices.len(), s.indices.len());
        assert_eq!(r.positions.len(), s.positions.len());
    }

    #[test]
    fn one_iter_quadruples_triangles() {
        let s = sphere_mesh(0.5, 8, 12, UvMode::Fit);
        let r = loop_subdivide(&s, 1);
        assert_eq!(r.indices.len(), s.indices.len() * 4);
    }

    #[test]
    fn subdivided_sphere_stays_near_surface() {
        // Loop on a UV sphere should produce a smoother sphere — every
        // vertex should still lie close to the 0.5-radius surface (a little
        // inward because Loop is approximating, never exactly on it).
        let s = sphere_mesh(0.5, 8, 12, UvMode::Fit);
        let r = loop_subdivide(&s, 2);
        for p in &r.positions {
            let len = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            assert!(
                (len - 0.5).abs() < 0.07,
                "subdivided vertex strayed: dist={len}",
            );
        }
    }

    #[test]
    fn normals_are_unit_length_after_subdivide() {
        let s = sphere_mesh(0.5, 6, 8, UvMode::Fit);
        let r = loop_subdivide(&s, 1);
        for n in &r.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!(
                (len - 1.0).abs() < 1e-3,
                "non-unit normal after subdivide: |n|={len}",
            );
        }
    }
}
