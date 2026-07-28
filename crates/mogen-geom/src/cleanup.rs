//! Post-operation mesh cleanup: weld near-duplicate vertices, cull degenerate
//! triangles, and (optionally) recompute per-vertex normals from face normals.

use std::collections::HashMap;

use glam::Vec3;

use mogen_core::Mesh;

const WELD_EPS: f32 = 1e-4;

/// Grid that face normals are snapped to before a UV tangent basis is derived
/// from them. Matches the normal tolerance `cull_coplanar_opposites` uses to
/// decide two triangles share a plane.
const NORMAL_SNAP: f32 = 1e-3;

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

/// True when a mesh bounds no meaningful volume: it is empty, or it is a flat
/// or collapsed shell whose enclosed volume vanishes relative to its own
/// extent. Manifold happily returns such a sheet — the convex hull of coplanar
/// points comes back as back-to-back triangle fans, not as an empty mesh — so
/// anything that must produce a *solid* tests this rather than testing for
/// emptiness.
///
/// The tolerance is scale-relative (volume against the cube of the bounding
/// diagonal), so a legitimately thin-but-solid slab still passes at any unit
/// scale while an exactly-planar hull does not.
pub fn is_degenerate_solid(mesh: &Mesh) -> bool {
    if mesh.positions.is_empty() || mesh.indices.len() < 3 {
        return true;
    }
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for p in &mesh.positions {
        let v = Vec3::from_array(*p);
        lo = lo.min(v);
        hi = hi.max(v);
    }
    let diag = (hi - lo).length();
    if !diag.is_finite() || diag <= 0.0 {
        return true;
    }
    mesh.solid_volume() <= 1e-6 * diag * diag * diag
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
/// normals needs a different UV per face, so the projection pass unwelds into
/// independent triangles. It then re-shares every corner that came out
/// identical, via [`dedup_exact_vertices`] — coplanar neighbours within one
/// face agree on all three attributes, so only genuine UV seams cost a
/// duplicate. The smooth per-vertex normals are carried through untouched, so
/// shading and watertight geometry are unchanged either way.
pub fn assign_per_face_uvs(mesh: &Mesh) -> Mesh {
    let n_tris = mesh.indices.len() / 3;
    let mut positions = Vec::with_capacity(n_tris * 3);
    let mut normals = Vec::with_capacity(n_tris * 3);
    let mut uvs = Vec::with_capacity(n_tris * 3);
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
        // Snap the normal to a coarse grid before deriving a basis from it, so
        // that every triangle tiling one flat face derives the *same* basis.
        // Triangulating a quad hands the cross product different edge vectors
        // per triangle, and the results disagree in the last ULP (and in the
        // sign bit of zero components, which winding alone flips). Left alone,
        // that splits the shared corners of a flat face into UVs like 0.45 vs
        // 0.44999996 — a hairline texture discontinuity mid-face, and a
        // duplicate vertex that dedup can do nothing with. The grid is the same
        // 1e-3 `cull_coplanar_opposites` uses to decide two faces share a plane;
        // it rotates the basis by at most ~0.06°, uniformly across the face, so
        // the projection is unchanged in everything but name.
        n = snap_normal(n);
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
        }
        uvs.push(project(p0));
        uvs.push(project(p1));
        uvs.push(project(p2));
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
    dedup_exact_vertices(&Mesh {
        positions,
        normals,
        uvs,
        indices,
        ..Default::default()
    })
}

/// Map `-0.0` to `+0.0`, leaving every other value bit-identical. Signed zero is
/// numerically invisible but breaks any hash keyed on raw float bits.
fn unsign_zero(v: f32) -> f32 {
    if v == 0.0 {
        0.0
    } else {
        v
    }
}

/// Quantize a unit normal onto a `NORMAL_SNAP` grid and renormalize, so normals
/// that agree to within float noise become bit-identical. Renormalizing is
/// itself float maths, but it is a pure function of the snapped input, so equal
/// inputs still yield equal outputs — which is the property callers need.
fn snap_normal(n: Vec3) -> Vec3 {
    let q = |v: f32| unsign_zero((v / NORMAL_SNAP).round() * NORMAL_SNAP);
    let snapped = Vec3::new(q(n.x), q(n.y), q(n.z)).normalize_or_zero();
    if snapped.length_squared() < 0.5 {
        return n;
    }
    Vec3::new(
        unsign_zero(snapped.x),
        unsign_zero(snapped.y),
        unsign_zero(snapped.z),
    )
}

/// Merge vertices that are *numerically equal* on position, normal and UV,
/// rewriting indices onto the survivors.
///
/// Unlike [`weld_vertices`], this makes no geometric judgement: it merges only
/// what is already interchangeable, so it can never move a vertex, average a
/// normal, or smear a UV across a seam. That makes it safe to run after a pass
/// that unwelds by construction — [`assign_per_face_uvs`] emits three vertices
/// per triangle, and the coplanar triangles tiling one flat face re-share their
/// corners here instead of shipping five duplicates of every box corner.
///
/// Keys are `f32::to_bits` after collapsing `-0.0` to `+0.0`, so the two zeroes
/// merge — they compare equal as floats and render identically, and CSG output
/// produces both spellings for the same coordinate. Survivors keep first-seen
/// order, which makes the output deterministic.
///
/// Deliberately private: the key covers position/normal/UV only, and joints,
/// weights and colours are dropped rather than remapped. That is sound for the
/// one caller — CSG output is never skinned or vertex-coloured — but would
/// silently corrupt a skinned mesh, so this must not become public without
/// folding those attributes into both the key and the output.
fn dedup_exact_vertices(mesh: &Mesh) -> Mesh {
    let has_uvs = mesh.has_uvs();
    let mut seen: HashMap<([u32; 3], [u32; 3], [u32; 2]), u32> = HashMap::new();
    let mut remap = vec![0u32; mesh.positions.len()];
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(mesh.positions.len());
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(mesh.normals.len());
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(mesh.uvs.len());

    let bits = |v: f32| unsign_zero(v).to_bits();
    for i in 0..mesh.positions.len() {
        let p = mesh.positions[i];
        let n = mesh.normals[i];
        let uv = if has_uvs { mesh.uvs[i] } else { [0.0, 0.0] };
        let key = (
            [bits(p[0]), bits(p[1]), bits(p[2])],
            [bits(n[0]), bits(n[1]), bits(n[2])],
            [bits(uv[0]), bits(uv[1])],
        );
        remap[i] = *seen.entry(key).or_insert_with(|| {
            let idx = positions.len() as u32;
            positions.push(p);
            normals.push(n);
            if has_uvs {
                uvs.push(uv);
            }
            idx
        });
    }

    Mesh {
        positions,
        normals,
        uvs,
        indices: mesh.indices.iter().map(|&i| remap[i as usize]).collect(),
        ..Default::default()
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
    fn an_ordinary_box_is_not_degenerate() {
        let mesh = crate::box_mesh([2.0, 3.0, 4.0], mogen_core::UvMode::default());
        // Sanity-check the volume the guard reads, so a failure below points at
        // the threshold rather than at the measurement.
        let v = mesh.solid_volume();
        assert!((v - 24.0).abs() < 1e-3, "got {v}");
        assert!(!is_degenerate_solid(&mesh));
    }

    #[test]
    fn a_flat_sheet_bounds_no_volume() {
        // Two back-to-back triangle pairs spanning a unit square: closed enough
        // to survive a CSG round trip, but enclosing nothing. This is the shape
        // Manifold hands back for the convex hull of coplanar points.
        let mesh = Mesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            indices: vec![0, 1, 2, 0, 2, 3, 0, 2, 1, 0, 3, 2],
            ..Default::default()
        };
        assert!(is_degenerate_solid(&mesh));
    }

    #[test]
    fn a_thin_slab_is_still_a_solid() {
        // The tolerance is scale-relative, so a wafer far thinner than any
        // modelled part must not be mistaken for a degenerate sheet.
        let mesh = crate::box_mesh([1.0, 1e-3, 1.0], mogen_core::UvMode::default());
        assert!(!is_degenerate_solid(&mesh));
    }

    #[test]
    fn empty_mesh_is_degenerate() {
        assert!(is_degenerate_solid(&Mesh::default()));
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
    fn per_face_uvs_reshare_corners_within_one_flat_face() {
        // A unit quad split into two coplanar triangles sharing the diagonal
        // 1–2. Both triangles derive the same tangent basis from the same
        // normal, so the shared corners project to the same UV and must come
        // back welded: 4 verts, not 6. Two triangles' worth of indices remain.
        let mesh = Mesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            indices: vec![0, 1, 2, 1, 3, 2],
            ..Default::default()
        };
        let out = assign_per_face_uvs(&mesh);
        assert_eq!(out.positions.len(), 4, "coplanar corners should re-share");
        assert_eq!(out.indices.len(), 6);
        // The diagonal really is shared, not two coincident copies.
        assert_eq!(out.indices[1], out.indices[3]);
        assert_eq!(out.indices[2], out.indices[5]);
    }

    #[test]
    fn per_face_uvs_keep_a_seam_across_a_normal_discontinuity() {
        // Two triangles meeting along edge 1–2 at a right angle: one in the XY
        // plane, one in the YZ plane. They need different tangent bases, so the
        // shared edge must stay duplicated — 6 verts, no merging across the
        // seam — or the texture would smear around the corner.
        let mesh = Mesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [1.0, 0.0, 1.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            indices: vec![0, 1, 2, 1, 3, 2],
            ..Default::default()
        };
        let out = assign_per_face_uvs(&mesh);
        assert_eq!(out.positions.len(), 6, "a UV seam must not be welded away");
    }

    #[test]
    fn dedup_exact_vertices_preserves_the_triangle_soup() {
        // Identical corners collapse; the resolved triangles are unchanged.
        let mesh = Mesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 6],
            uvs: vec![
                [0.0, 0.0],
                [1.0, 0.0],
                [0.0, 1.0],
                [0.0, 0.0],
                [1.0, 0.0],
                [0.0, 1.0],
            ],
            indices: vec![0, 1, 2, 3, 4, 5],
            ..Default::default()
        };
        let out = dedup_exact_vertices(&mesh);
        assert_eq!(out.positions.len(), 3);
        assert_eq!(out.indices, vec![0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn dedup_exact_vertices_splits_on_any_differing_attribute() {
        // Same position and normal, different UV — these are not interchangeable
        // and merging them would smear a texture seam.
        let mesh = Mesh {
            positions: vec![[0.0, 0.0, 0.0]; 2],
            normals: vec![[0.0, 0.0, 1.0]; 2],
            uvs: vec![[0.0, 0.0], [1.0, 0.0]],
            indices: vec![0, 1],
            ..Default::default()
        };
        assert_eq!(dedup_exact_vertices(&mesh).positions.len(), 2);

        // Same position and UV, different normal — a hard shading edge.
        let mesh = Mesh {
            positions: vec![[0.0, 0.0, 0.0]; 2],
            normals: vec![[0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
            uvs: vec![[0.0, 0.0]; 2],
            indices: vec![0, 1],
            ..Default::default()
        };
        assert_eq!(dedup_exact_vertices(&mesh).positions.len(), 2);
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
