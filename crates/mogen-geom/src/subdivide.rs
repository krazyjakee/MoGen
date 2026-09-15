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

use std::collections::BTreeMap as HashMap;

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
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
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
            edges
                .entry(edge_key(u, v))
                .or_insert_with(|| EdgeData {
                    opposites: Vec::new(),
                })
                .opposites
                .push(opp);
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
    let mut edge_to_new: HashMap<EdgeKey, u32> = HashMap::new();
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
            if count != 2 {
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
    let uvs = if has_uvs { new_uvs } else { Vec::new() };

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

/// Checked CSG path: geometric adjacency drives positions; render adjacency
/// drives interpolation of UVs. No tolerance weld, remeshing or hole closing.
pub fn loop_subdivide_geometric(mesh: &Mesh, iterations: u32) -> Result<Mesh, String> {
    if iterations == 0 {
        return Ok(mesh.clone());
    }
    if mesh.indices.len() % 3 != 0
        || mesh
            .indices
            .iter()
            .any(|&i| i as usize >= mesh.positions.len())
        || mesh.positions.iter().flatten().any(|p| !p.is_finite())
        || (!mesh.uvs.is_empty() && mesh.uvs.len() != mesh.positions.len())
    {
        return Err("Subdivision requires finite positions, complete triangles, valid indices and aligned UVs".into());
    }
    let growth = 4usize
        .checked_pow(iterations)
        .ok_or("Subdivision growth overflow")?;
    if mesh.indices.len() / 3 > 2_000_000 / growth {
        return Err(
            "Subdivision exceeds 2 million triangle limit; reduce subdivide or tessellation".into(),
        );
    }
    if !mesh.joints.is_empty() || !mesh.weights.is_empty() || !mesh.colors.is_empty() {
        return Err("CSG subdivision requires an unskinned mesh without vertex colors".into());
    }
    let mut current = mesh.clone();
    for _ in 0..iterations {
        let (ids, positions) = crate::shading::position_ids(&current);
        let mut geometric = Mesh {
            positions,
            indices: current.indices.iter().map(|&i| ids[i as usize]).collect(),
            ..Default::default()
        };
        let mut edges = HashMap::<EdgeKey, usize>::new();
        let mut orientations = HashMap::<EdgeKey, i32>::new();
        for t in geometric.indices.chunks_exact(3) {
            if t[0] == t[1] || t[1] == t[2] || t[2] == t[0] {
                return Err(
                    "Subdivision has collapsed geometric triangles; remove degenerate faces".into(),
                );
            }
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry(edge_key(a, b)).or_default() += 1;
                *orientations.entry(edge_key(a, b)).or_default() += if a < b { 1 } else { -1 };
            }
        }
        let mut boundary = vec![0; geometric.positions.len()];
        for (&(a, b), &count) in &edges {
            if count > 2 {
                return Err(
                    "Subdivision requires manifold edges; separate intersecting shells".into(),
                );
            }
            if count == 2 && orientations[&(a, b)] != 0 {
                return Err(
                    "Subdivision requires consistent face winding; orient the input shell".into(),
                );
            }
            if count == 1 {
                boundary[a as usize] += 1;
                boundary[b as usize] += 1;
            }
        }
        if boundary.iter().any(|&n| n != 0 && n != 2) {
            return Err("Subdivision requires two boundary neighbors per vertex; separate pinched boundaries".into());
        }
        // Edge-manifold is insufficient: two closed shells can touch at a
        // single position. Their disconnected vertex links are unsupported.
        let mut links = vec![Vec::<(u32, u32)>::new(); geometric.positions.len()];
        for t in geometric.indices.chunks_exact(3) {
            for i in 0..3 {
                links[t[i] as usize].push((t[(i + 1) % 3], t[(i + 2) % 3]));
            }
        }
        for link in links {
            if link.is_empty() {
                continue;
            }
            let mut adjacency = HashMap::<u32, Vec<u32>>::new();
            for (a, b) in link {
                adjacency.entry(a).or_default().push(b);
                adjacency.entry(b).or_default().push(a);
            }
            let mut pending = vec![*adjacency.keys().next().unwrap()];
            let mut visited = std::collections::BTreeSet::new();
            while let Some(v) = pending.pop() {
                if visited.insert(v) {
                    pending.extend(&adjacency[&v]);
                }
            }
            if visited.len() != adjacency.len() {
                return Err("Subdivision requires a connected vertex fan; separate shells touching at one point".into());
            }
        }
        let old_count = geometric.positions.len();
        geometric = subdivide_once(&geometric);
        let geom_edges: HashMap<_, _> = edges
            .keys()
            .enumerate()
            .map(|(i, &e)| (e, old_count + i))
            .collect();
        let mut render_edges = HashMap::new();
        for t in current.indices.chunks_exact(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                render_edges.insert(edge_key(a, b), ());
            }
        }
        let mut next = subdivide_once(&current);
        for (i, &id) in ids.iter().enumerate() {
            next.positions[i] = geometric.positions[id as usize];
        }
        for (i, &(a, b)) in render_edges.keys().enumerate() {
            next.positions[ids.len() + i] =
                geometric.positions[geom_edges[&edge_key(ids[a as usize], ids[b as usize])]];
        }
        if next.positions.iter().flatten().any(|v| !v.is_finite()) {
            return Err("Subdivision produced non-finite positions".into());
        }
        current = next;
    }
    Ok(current)
}

#[cfg(test)]
mod geometric_tests {
    use super::*;
    #[test]
    fn seam_split_closed_mesh_matches_geometric_positions() {
        let mesh = Mesh {
            positions: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            indices: vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
            ..Default::default()
        };
        let split = Mesh {
            positions: mesh
                .indices
                .iter()
                .map(|&i| mesh.positions[i as usize])
                .collect(),
            indices: (0..12).collect(),
            uvs: vec![[0., 0.]; 12],
            ..Default::default()
        };
        for level in [1, 2] {
            let a = loop_subdivide_geometric(&mesh, level).unwrap();
            let b = loop_subdivide_geometric(&split, level).unwrap();
            for (&i, &j) in a.indices.iter().zip(&b.indices) {
                assert_eq!(a.positions[i as usize], b.positions[j as usize]);
            }
            for p in &b.positions {
                assert!(p.iter().all(|x| *x >= 0. && *x <= 1.));
            }
        }
    }
    #[test]
    fn boundaries_slivers_nonmanifold_and_growth() {
        let mesh = Mesh {
            positions: vec![[0., 0., 0.], [1., 0., 0.], [1., 1e-8, 0.], [0., 1., 0.]],
            indices: vec![0, 1, 2, 0, 2, 3],
            ..Default::default()
        };
        let out = loop_subdivide_geometric(&mesh, 2).unwrap();
        assert_eq!(out.indices.len(), 96);
        assert!(out
            .positions
            .iter()
            .flatten()
            .all(|x| x.is_finite() && *x >= 0. && *x <= 1.));
        let mut bad = mesh.clone();
        bad.indices.extend_from_slice(&[0, 2, 1]);
        assert!(loop_subdivide_geometric(&bad, 1)
            .unwrap_err()
            .contains("manifold"));
        assert!(loop_subdivide_geometric(&mesh, 20).is_err());
    }
    #[test]
    fn invalid_indices_winding_and_disconnected_fans_are_rejected() {
        let mut mesh = Mesh {
            positions: vec![
                [0., 0., 0.],
                [1., 0., 0.],
                [0., 1., 0.],
                [-1., 0., 0.],
                [0., -1., 0.],
            ],
            indices: vec![0, 1, 2, 0, 3, 4],
            ..Default::default()
        };
        assert!(loop_subdivide_geometric(&mesh, 1).is_err());
        mesh.indices = vec![0, 1, 2, 0, 1, 3];
        assert!(loop_subdivide_geometric(&mesh, 1)
            .unwrap_err()
            .contains("winding"));
        mesh.indices = vec![0, 1, 99];
        assert!(loop_subdivide_geometric(&mesh, 1).is_err());
        mesh.indices = vec![0, 1];
        assert!(loop_subdivide_geometric(&mesh, 1).is_err());
        mesh.indices = vec![0, 1, 2];
        mesh.positions[0][0] = f32::NAN;
        assert!(loop_subdivide_geometric(&mesh, 1).is_err());
    }
}
