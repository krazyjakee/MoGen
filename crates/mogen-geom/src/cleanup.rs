//! Post-operation mesh cleanup: weld near-duplicate vertices, cull degenerate
//! triangles, and (optionally) recompute per-vertex normals from face normals.

use std::collections::HashMap;

use glam::Vec3;

use mogen_core::Mesh;

const WELD_EPS: f32 = 1e-4;
/// Normals are compared on a coarser grid than positions: the tiny orientation
/// jitter CSG output carries shouldn't stop two otherwise-identical vertices
/// from merging. Matches the tolerance `cull_coplanar_opposites` uses.
const NORMAL_EPS: f32 = 1e-3;

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

/// Key for grouping candidate-identical vertices into hash buckets. Every
/// channel is quantised, so a bucket hit only means "probably equal" — the
/// caller still verifies each candidate against the real epsilons.
#[derive(PartialEq, Eq, Hash)]
struct VertexKey {
    pos: [i64; 3],
    normal: [i64; 3],
    uv: [i64; 2],
    joints: [u16; 4],
    weights: [i64; 4],
    color: [i64; 4],
}

fn quant<const N: usize>(v: [f32; N], scale: f32) -> [i64; N] {
    v.map(|c| (c * scale).round() as i64)
}

fn near<const N: usize>(a: [f32; N], b: [f32; N], eps: f32) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() <= eps)
}

/// Merge vertices that agree on *every* attribute — position, normal, UV, and
/// any skinning / vertex-colour channels — to within an epsilon.
///
/// This is deliberately narrower than [`weld_vertices`], which merges purely by
/// position and *averages* the other channels. This pass never alters a
/// surviving vertex's data; it only drops exact duplicates. A vertex that
/// differs from its neighbour in any channel — a genuine UV seam, a shading
/// split — keeps its own copy. Triangle count and surface topology are
/// therefore unchanged, so a watertight mesh stays watertight.
///
/// Output order follows input vertex order, so the result is deterministic.
pub fn weld_identical_vertices(mesh: &Mesh) -> Mesh {
    let n_verts = mesh.positions.len();
    if n_verts == 0 {
        return mesh.clone();
    }
    // Channels are optional; a mismatched length means "absent" rather than
    // risking an index panic on a malformed mesh.
    let has_uvs = mesh.uvs.len() == n_verts;
    let has_skin = mesh.joints.len() == n_verts && mesh.weights.len() == n_verts;
    let has_colors = mesh.colors.len() == n_verts;

    let pos_scale = 1.0 / WELD_EPS;
    let n_scale = 1.0 / NORMAL_EPS;

    let mut buckets: HashMap<VertexKey, Vec<u32>> = HashMap::new();
    let mut remap = vec![0u32; n_verts];
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut joints: Vec<[u16; 4]> = Vec::new();
    let mut weights: Vec<[f32; 4]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();

    for (i, &p) in mesh.positions.iter().enumerate() {
        let nrm = mesh.normals[i];
        let uv = if has_uvs { mesh.uvs[i] } else { [0.0; 2] };
        let jnt = if has_skin { mesh.joints[i] } else { [0u16; 4] };
        let wgt = if has_skin { mesh.weights[i] } else { [0.0; 4] };
        let col = if has_colors { mesh.colors[i] } else { [0.0; 4] };

        let key = VertexKey {
            pos: quant(p, pos_scale),
            normal: quant(nrm, n_scale),
            // UVs are in world-space metres here, so they share the position grid.
            uv: quant(uv, pos_scale),
            joints: jnt,
            weights: quant(wgt, pos_scale),
            color: quant(col, pos_scale),
        };

        let list = buckets.entry(key).or_default();
        let mut found = None;
        for &j in list.iter() {
            let k = j as usize;
            if !near(p, positions[k], WELD_EPS) || !near(nrm, normals[k], NORMAL_EPS) {
                continue;
            }
            if has_uvs && !near(uv, uvs[k], WELD_EPS) {
                continue;
            }
            if has_skin && (jnt != joints[k] || !near(wgt, weights[k], WELD_EPS)) {
                continue;
            }
            if has_colors && !near(col, colors[k], WELD_EPS) {
                continue;
            }
            found = Some(j);
            break;
        }

        remap[i] = match found {
            Some(j) => j,
            None => {
                let j = positions.len() as u32;
                positions.push(p);
                normals.push(nrm);
                if has_uvs {
                    uvs.push(uv);
                }
                if has_skin {
                    joints.push(jnt);
                    weights.push(wgt);
                }
                if has_colors {
                    colors.push(col);
                }
                list.push(j);
                j
            }
        };
    }

    let indices: Vec<u32> = mesh.indices.iter().map(|i| remap[*i as usize]).collect();
    Mesh {
        positions,
        normals,
        uvs,
        indices,
        joints,
        weights,
        colors,
    }
}

/// True when the mesh is a closed 2-manifold: every undirected edge is shared
/// by exactly two triangles after welding coincident vertices on the same
/// epsilon `csg.rs` uses before handing meshes to Manifold. Used by the
/// export-time merge pass to gate which leaves can be CSG-unioned together —
/// open primitives (`plane`, `disc`, `curved_plane`, `spline_ribbon`, decals)
/// would trip `Manifold::from_mesh_f32` and panic, so they have to pass
/// through the merge unchanged.
pub fn is_closed_manifold(mesh: &Mesh) -> bool {
    if mesh.indices.is_empty() || mesh.indices.len() % 3 != 0 {
        return false;
    }
    let scale = 1.0 / WELD_EPS.max(1e-9);
    let mut canon: Vec<u32> = Vec::with_capacity(mesh.positions.len());
    let mut bucket: HashMap<[i64; 3], u32> = HashMap::new();
    for p in &mesh.positions {
        let key = [
            (p[0] * scale).round() as i64,
            (p[1] * scale).round() as i64,
            (p[2] * scale).round() as i64,
        ];
        let next = bucket.len() as u32;
        let id = *bucket.entry(key).or_insert(next);
        canon.push(id);
    }
    let mut edge_counts: HashMap<(u32, u32), u32> = HashMap::new();
    for tri in mesh.indices.chunks_exact(3) {
        let a = canon[tri[0] as usize];
        let b = canon[tri[1] as usize];
        let c = canon[tri[2] as usize];
        if a == b || b == c || a == c {
            continue;
        }
        for (u, v) in [(a, b), (b, c), (c, a)] {
            let key = if u < v { (u, v) } else { (v, u) };
            *edge_counts.entry(key).or_insert(0) += 1;
        }
    }
    if edge_counts.is_empty() {
        return false;
    }
    edge_counts.values().all(|&c| c == 2)
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
        colors: mesh.colors.clone(),
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
        assign_per_face_uvs(&with_normals)
    }
}

/// Assign UVs by per-face planar projection: every triangle is projected onto
/// the plane perpendicular to its own geometric normal, using an orthonormal
/// tangent basis built from that normal. One UV unit per world-space metre;
/// the material's `uv_scale` sets the final tiling frequency.
///
/// This is the right fallback for the faceted convex solids the `hull`
/// primitive (and CSG booleans) produce. An axis-snapped projection — pick the
/// dominant world axis and drop it — foreshortens any face that is not aligned
/// to that axis: a 45° ramp's texture compresses by `cos θ` along the slope.
/// Projecting onto the face's actual tangent plane removes that skew, so
/// sloped and sheared faces tile at the same texel density as flat ones.
///
/// Choosing a basis per face means a vertex shared by faces with different
/// normals needs a different UV per face, so the mesh is first unwelded into
/// independent triangles. That is only *locally* necessary, though: coplanar
/// neighbours — the common case for walls, floors and box-with-holes CSG —
/// derive the same basis and so project to the same UV. A re-weld pass on the
/// full attribute tuple therefore restores their shared vertices while leaving
/// the genuine seams split, keeping the vertex count close to the input's
/// instead of a flat `n_tris * 3`. The smooth per-vertex normals are carried
/// through unchanged, so shading and watertight geometry are unaffected.
pub fn assign_per_face_uvs(mesh: &Mesh) -> Mesh {
    let n_verts = mesh.positions.len();
    let has_skin = mesh.joints.len() == n_verts && mesh.weights.len() == n_verts;
    let has_colors = mesh.colors.len() == n_verts;
    let n_tris = mesh.indices.len() / 3;
    let mut positions = Vec::with_capacity(n_tris * 3);
    let mut normals = Vec::with_capacity(n_tris * 3);
    let mut uvs = Vec::with_capacity(n_tris * 3);
    let mut joints = Vec::with_capacity(if has_skin { n_tris * 3 } else { 0 });
    let mut weights = Vec::with_capacity(if has_skin { n_tris * 3 } else { 0 });
    let mut colors = Vec::with_capacity(if has_colors { n_tris * 3 } else { 0 });
    let mut indices = Vec::with_capacity(n_tris * 3);
    for tri in mesh.indices.chunks_exact(3) {
        let [i0, i1, i2] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let p0 = Vec3::from_array(mesh.positions[i0]);
        let p1 = Vec3::from_array(mesh.positions[i1]);
        let p2 = Vec3::from_array(mesh.positions[i2]);
        // Geometric face normal — robust against the smooth per-vertex normals
        // disagreeing across the triangle. Degenerate tris fall back to +Z;
        // their zero area makes the choice invisible anyway.
        let mut n = (p1 - p0).cross(p2 - p0).normalize_or_zero();
        if n.length_squared() < 0.5 {
            n = Vec3::Z;
        }
        // Orthonormal tangent basis in the plane ⟂ n. Seed with the world axis
        // least aligned with n so the cross product never degenerates.
        let (ax, ay, az) = (n.x.abs(), n.y.abs(), n.z.abs());
        let seed = if ax <= ay && ax <= az {
            Vec3::X
        } else if ay <= az {
            Vec3::Y
        } else {
            Vec3::Z
        };
        let t = seed.cross(n).normalize_or_zero();
        let b = n.cross(t);
        let project = |p: Vec3| -> [f32; 2] { [p.dot(t), p.dot(b)] };
        let base = positions.len() as u32;
        for &i in &[i0, i1, i2] {
            positions.push(mesh.positions[i]);
            normals.push(mesh.normals[i]);
            if has_skin {
                joints.push(mesh.joints[i]);
                weights.push(mesh.weights[i]);
            }
            if has_colors {
                colors.push(mesh.colors[i]);
            }
        }
        uvs.push(project(p0));
        uvs.push(project(p1));
        uvs.push(project(p2));
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
    weld_identical_vertices(&Mesh {
        positions,
        normals,
        uvs,
        indices,
        joints,
        weights,
        colors,
    })
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
    fn box_is_closed_manifold_after_canonicalisation() {
        // box_mesh emits per-face vertex copies; the closed-manifold check
        // must canonicalise positions on the same epsilon csg.rs uses, so
        // the cube reads as closed even though its index list references 24
        // separate vertices.
        let m = crate::box_mesh([1.0, 1.0, 1.0], mogen_core::UvMode::default());
        assert!(is_closed_manifold(&m));
    }

    #[test]
    fn open_primitives_are_not_closed() {
        let plane = crate::plane_mesh([1.0, 1.0], mogen_core::UvMode::default());
        let disc = crate::disc_mesh(0.5, 16, mogen_core::UvMode::default());
        assert!(!is_closed_manifold(&plane));
        assert!(!is_closed_manifold(&disc));
    }

    #[test]
    fn empty_mesh_is_not_closed() {
        let empty = Mesh::default();
        assert!(!is_closed_manifold(&empty));
    }

    #[test]
    fn per_face_uvs_do_not_foreshorten_a_sloped_face() {
        // A 45° triangle: the edge p0->p1 rises one unit in Z over one in X, so
        // its true length is √2. An axis-snapped projection would drop one axis
        // and report length 1; the per-face tangent projection must preserve √2
        // so the texture tiles at the same density as on a flat face.
        let mesh = Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0; 3]; 3],
            indices: vec![0, 1, 2],
            ..Default::default()
        };
        let out = assign_per_face_uvs(&mesh);
        assert_eq!(out.uvs.len(), 3);
        let du = out.uvs[1][0] - out.uvs[0][0];
        let dv = out.uvs[1][1] - out.uvs[0][1];
        let uv_len = (du * du + dv * dv).sqrt();
        assert!(
            (uv_len - 2.0_f32.sqrt()).abs() < 1e-5,
            "edge UV span {uv_len} should equal the true edge length √2"
        );
    }

    #[test]
    fn per_face_uvs_reweld_coplanar_neighbours() {
        // Two coplanar triangles forming a unit quad, handed in as a triangle
        // soup with no sharing. They share the same face normal, so they get
        // the same tangent basis and the same UVs along the shared diagonal —
        // the re-weld must recover the 4 shared corners from 6 soup vertices
        // without touching the triangle count.
        let mesh = Mesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 6],
            indices: vec![0, 1, 2, 3, 4, 5],
            ..Default::default()
        };
        let out = assign_per_face_uvs(&mesh);
        assert_eq!(out.indices.len(), 6, "triangle count must not change");
        assert_eq!(out.positions.len(), 4, "coplanar neighbours should re-weld");
        assert_eq!(out.uvs.len(), out.positions.len());
    }

    #[test]
    fn per_face_uvs_keep_genuine_seams_split() {
        // Two triangles meeting at a right angle along the shared edge
        // (0,0,0)-(1,0,0). Their face normals differ, so the shared edge is a
        // real UV seam: those vertices must NOT be merged.
        let mesh = Mesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            // Distinct per-vertex normals too — nothing here may collapse.
            normals: vec![
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [0.0, -1.0, 0.0],
                [0.0, -1.0, 0.0],
                [0.0, -1.0, 0.0],
            ],
            indices: vec![0, 1, 2, 3, 4, 5],
            ..Default::default()
        };
        let out = assign_per_face_uvs(&mesh);
        assert_eq!(out.indices.len(), 6);
        assert_eq!(out.positions.len(), 6, "a real seam must stay unwelded");
    }

    #[test]
    fn per_face_uvs_preserve_skin_and_colour_channels() {
        // Regression guard: the old `..Default::default()` silently dropped
        // joints/weights/colors. They must survive per-vertex.
        let mesh = Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            indices: vec![0, 1, 2],
            joints: vec![[1, 0, 0, 0], [2, 0, 0, 0], [3, 0, 0, 0]],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
            colors: vec![
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0, 1.0],
            ],
            ..Default::default()
        };
        let out = assign_per_face_uvs(&mesh);
        assert_eq!(out.positions.len(), 3);
        // Vertices differ in joints/colors, so ordering is the input ordering.
        assert_eq!(out.joints, mesh.joints);
        assert_eq!(out.weights, mesh.weights);
        assert_eq!(out.colors, mesh.colors);
    }

    #[test]
    fn weld_identical_keeps_vertices_differing_only_in_skin() {
        // Same position/normal/uv but different joint bindings — merging these
        // would silently rebind geometry to the wrong bone.
        let mesh = Mesh {
            positions: vec![[0.0, 0.0, 0.0]; 2],
            normals: vec![[0.0, 0.0, 1.0]; 2],
            uvs: vec![[0.0, 0.0]; 2],
            indices: vec![0, 1],
            joints: vec![[1, 0, 0, 0], [7, 0, 0, 0]],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; 2],
            ..Default::default()
        };
        let out = weld_identical_vertices(&mesh);
        assert_eq!(out.positions.len(), 2);
        assert_eq!(out.indices, vec![0, 1]);
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
