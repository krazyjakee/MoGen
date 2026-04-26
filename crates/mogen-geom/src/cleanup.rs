//! Post-operation mesh cleanup: weld near-duplicate vertices, cull degenerate
//! triangles, and (optionally) recompute per-vertex normals from face normals.

use std::collections::HashMap;

use glam::Vec3;

use mogen_core::Mesh;

const WELD_EPS: f32 = 1e-4;

/// Merge vertices whose positions are within `eps`. Normals of merged vertices
/// are averaged and renormalized. Indices are rewritten to reference the
/// canonical vertex for each cluster.
pub fn weld_vertices(mesh: &Mesh, eps: f32) -> Mesh {
    let scale = 1.0 / eps.max(1e-9);
    let has_uvs = mesh.has_uvs();
    let mut buckets: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    let mut remap = vec![u32::MAX; mesh.positions.len()];
    let mut new_positions: Vec<[f32; 3]> = Vec::new();
    let mut new_normals: Vec<Vec3> = Vec::new();
    let mut new_uvs: Vec<[f32; 2]> = Vec::new();
    let mut new_uv_sums: Vec<[f32; 2]> = Vec::new();
    let mut new_counts: Vec<u32> = Vec::new();

    for (i, p) in mesh.positions.iter().enumerate() {
        let key = [
            (p[0] * scale).round() as i64,
            (p[1] * scale).round() as i64,
            (p[2] * scale).round() as i64,
        ];
        let merged_id = {
            let list = buckets.entry(key).or_default();
            let mut found = None;
            for &idx in list.iter() {
                let q = new_positions[idx as usize];
                let d = (p[0] - q[0]).hypot(p[1] - q[1]).hypot(p[2] - q[2]);
                if d <= eps {
                    found = Some(idx);
                    break;
                }
            }
            if let Some(idx) = found {
                idx
            } else {
                let idx = new_positions.len() as u32;
                new_positions.push(*p);
                new_normals.push(Vec3::ZERO);
                new_counts.push(0);
                if has_uvs {
                    new_uv_sums.push([0.0, 0.0]);
                }
                list.push(idx);
                idx
            }
        };
        remap[i] = merged_id;
        let n = Vec3::from_array(mesh.normals[i]);
        new_normals[merged_id as usize] += n;
        new_counts[merged_id as usize] += 1;
        if has_uvs {
            let uv = mesh.uvs[i];
            new_uv_sums[merged_id as usize][0] += uv[0];
            new_uv_sums[merged_id as usize][1] += uv[1];
        }
    }

    let normals: Vec<[f32; 3]> = new_normals
        .iter()
        .zip(new_counts.iter())
        .map(|(sum, _)| {
            let n = sum.normalize_or_zero();
            [n.x, n.y, n.z]
        })
        .collect();

    if has_uvs {
        new_uvs = new_uv_sums
            .iter()
            .zip(new_counts.iter())
            .map(|(sum, count)| {
                let c = (*count).max(1) as f32;
                [sum[0] / c, sum[1] / c]
            })
            .collect();
    }

    let indices: Vec<u32> = mesh.indices.iter().map(|i| remap[*i as usize]).collect();
    Mesh {
        positions: new_positions,
        normals,
        uvs: new_uvs,
        indices,
        ..Default::default()
    }
}

/// Drop triangles with (near) zero area, as well as any triangle whose indices
/// collapsed to fewer than three distinct vertices.
pub fn cull_degenerate(mesh: &Mesh) -> Mesh {
    let mut indices = Vec::with_capacity(mesh.indices.len());
    for tri in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [tri[0], tri[1], tri[2]];
        if a == b || b == c || a == c {
            continue;
        }
        let pa = Vec3::from_array(mesh.positions[a as usize]);
        let pb = Vec3::from_array(mesh.positions[b as usize]);
        let pc = Vec3::from_array(mesh.positions[c as usize]);
        let area2 = (pb - pa).cross(pc - pa).length();
        if area2 < 1e-10 {
            continue;
        }
        indices.extend_from_slice(&[a, b, c]);
    }
    Mesh {
        positions: mesh.positions.clone(),
        normals: mesh.normals.clone(),
        uvs: mesh.uvs.clone(),
        indices,
        ..Default::default()
    }
}

/// Recompute per-vertex normals by averaging face normals of adjacent
/// triangles. Callers should typically weld first so seams are smoothed.
pub fn recompute_normals(mesh: &Mesh) -> Mesh {
    let mut acc = vec![Vec3::ZERO; mesh.positions.len()];
    for tri in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let pa = Vec3::from_array(mesh.positions[a]);
        let pb = Vec3::from_array(mesh.positions[b]);
        let pc = Vec3::from_array(mesh.positions[c]);
        let n = (pb - pa).cross(pc - pa);
        acc[a] += n;
        acc[b] += n;
        acc[c] += n;
    }
    let normals: Vec<[f32; 3]> = acc
        .iter()
        .map(|n| {
            let v = n.normalize_or_zero();
            [v.x, v.y, v.z]
        })
        .collect();
    Mesh {
        positions: mesh.positions.clone(),
        normals,
        uvs: mesh.uvs.clone(),
        indices: mesh.indices.clone(),
        ..Default::default()
    }
}

/// Drop pairs of triangles that share a plane, have opposite-facing normals,
/// and cover the same three vertex positions. This is the case produced when
/// two solid boxes touch along a face (their interior-facing triangulations
/// are coincident and antiparallel): CSG union happily keeps both because
/// there is no volumetric overlap to resolve, but they cancel visually and
/// leave interior geometry visible through nearby openings. Idempotent — if
/// CSG already removed the pair, this pass is a no-op.
pub fn cull_coplanar_opposites(mesh: &Mesh) -> Mesh {
    let scale = 1.0 / WELD_EPS.max(1e-9);
    let quant_pos = |p: [f32; 3]| -> [i64; 3] {
        [
            (p[0] * scale).round() as i64,
            (p[1] * scale).round() as i64,
            (p[2] * scale).round() as i64,
        ]
    };
    // Normals are quantized to a coarser grid than positions: tiny orientation
    // jitter from CSG output shouldn't split otherwise-matching faces.
    let n_scale = 1.0 / 1e-3;
    let quant_n = |n: Vec3| -> [i64; 3] {
        [
            (n.x * n_scale).round() as i64,
            (n.y * n_scale).round() as i64,
            (n.z * n_scale).round() as i64,
        ]
    };

    let tri_count = mesh.indices.len() / 3;
    let mut removed = vec![false; tri_count];
    // Key: (sorted vertex-position triple, quantized normal).
    let mut pending: HashMap<([[i64; 3]; 3], [i64; 3]), usize> = HashMap::new();

    for i in 0..tri_count {
        let a = mesh.indices[i * 3] as usize;
        let b = mesh.indices[i * 3 + 1] as usize;
        let c = mesh.indices[i * 3 + 2] as usize;
        let pa = Vec3::from_array(mesh.positions[a]);
        let pb = Vec3::from_array(mesh.positions[b]);
        let pc = Vec3::from_array(mesh.positions[c]);
        let n = (pb - pa).cross(pc - pa).normalize_or_zero();
        if n.length_squared() < 0.5 {
            continue;
        }
        let mut verts = [quant_pos(mesh.positions[a]), quant_pos(mesh.positions[b]), quant_pos(mesh.positions[c])];
        verts.sort();
        let n_key = quant_n(n);
        let neg_key = [-n_key[0], -n_key[1], -n_key[2]];
        if let Some(&other) = pending.get(&(verts, neg_key)) {
            removed[i] = true;
            removed[other] = true;
            pending.remove(&(verts, neg_key));
        } else {
            pending.insert((verts, n_key), i);
        }
    }

    let mut indices = Vec::with_capacity(mesh.indices.len());
    for i in 0..tri_count {
        if removed[i] {
            continue;
        }
        indices.extend_from_slice(&mesh.indices[i * 3..i * 3 + 3]);
    }
    Mesh {
        positions: mesh.positions.clone(),
        normals: mesh.normals.clone(),
        uvs: mesh.uvs.clone(),
        indices,
        joints: mesh.joints.clone(),
        weights: mesh.weights.clone(),
    }
}

/// Apply the standard post-CSG finalisation: cull degenerates → recompute
/// normals → assign triplanar UVs if the CSG result lacked them. With the
/// Manifold-backed CSG the input is already watertight and welded by
/// construction, so there is no boundary stitching or hole repair to do —
/// we just need normals (Manifold doesn't emit them) and a UV fallback for
/// the case where one operand had no UVs and we routed positions-only
/// through the boolean. The cull pass is kept as cheap insurance against
/// any zero-area triangles produced by numerically tight intersections.
pub fn clean_csg_output(mesh: &Mesh) -> Mesh {
    let culled = cull_degenerate(mesh);
    let with_normals = recompute_normals(&culled);
    if with_normals.has_uvs() {
        with_normals
    } else {
        assign_triplanar_uvs(&with_normals)
    }
}

/// Assign UVs by triplanar projection: for each vertex, pick the dominant
/// normal axis and project the position onto the remaining two axes. Produces
/// one UV unit per world-space meter. Tiling frequency is then controlled by
/// the texture itself or an explicit tiling factor (future work).
///
/// Compared with per-triangle projection, per-vertex projection costs a
/// texture seam when adjacent verts pick different axes — but it preserves
/// the indexed-mesh layout, which matters for the exporter.
pub fn assign_triplanar_uvs(mesh: &Mesh) -> Mesh {
    let uvs: Vec<[f32; 2]> = mesh
        .positions
        .iter()
        .zip(mesh.normals.iter())
        .map(|(p, n)| {
            let (ax, ay, az) = (n[0].abs(), n[1].abs(), n[2].abs());
            if ax >= ay && ax >= az {
                [p[2], p[1]]
            } else if ay >= az {
                [p[0], p[2]]
            } else {
                [p[0], p[1]]
            }
        })
        .collect();
    Mesh {
        positions: mesh.positions.clone(),
        normals: mesh.normals.clone(),
        uvs,
        indices: mesh.indices.clone(),
        joints: mesh.joints.clone(),
        weights: mesh.weights.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weld_merges_coincident_vertices() {
        // Two triangles sharing an edge but each with its own vertex copies.
        let mesh = Mesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                // Duplicate of index 1, within eps.
                [1.0 + 1e-6, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 6],
            indices: vec![0, 1, 2, 3, 4, 5],
            ..Default::default()
        };
        let welded = weld_vertices(&mesh, 1e-4);
        // Expect 4 unique verts (origin, +x, +y, +xy).
        assert_eq!(welded.positions.len(), 4);
        assert_eq!(welded.indices.len(), 6);
    }

    #[test]
    fn cull_removes_zero_area_triangles() {
        let mesh = Mesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [2.0, 0.0, 0.0], // collinear with 0 and 1 — zero area
                [0.0, 1.0, 0.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            indices: vec![0, 1, 2, 0, 1, 3],
            ..Default::default()
        };
        let culled = cull_degenerate(&mesh);
        assert_eq!(culled.indices.len(), 3); // Only the second tri survives.
    }

    #[test]
    fn coplanar_opposites_cancel() {
        // Two triangles at the same vertices with opposite winding → both drop.
        let mesh = Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            indices: vec![0, 1, 2, 0, 2, 1],
            ..Default::default()
        };
        let out = cull_coplanar_opposites(&mesh);
        assert!(out.indices.is_empty(), "opposite-facing coincident tris must cancel");
    }

    #[test]
    fn coplanar_same_direction_survives() {
        // Duplicate triangles in the same direction are NOT a cancellation case.
        let mesh = Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            indices: vec![0, 1, 2, 0, 1, 2],
            ..Default::default()
        };
        let out = cull_coplanar_opposites(&mesh);
        assert_eq!(out.indices.len(), 6);
    }

    #[test]
    fn recompute_normals_averages_adjacent_face_normals() {
        // Single triangle in the XY plane — vertex normal should be +Z.
        let mesh = Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0; 3]; 3],
            indices: vec![0, 1, 2],
            ..Default::default()
        };
        let result = recompute_normals(&mesh);
        for n in &result.normals {
            assert!((n[2] - 1.0).abs() < 1e-6);
        }
    }
}
