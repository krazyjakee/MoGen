//! Shading connectivity is separate from exact-position geometric adjacency.
use glam::Vec3;
use mogen_core::Mesh;
use std::collections::BTreeMap;

pub(crate) fn position_ids(mesh: &Mesh) -> (Vec<u32>, Vec<[f32; 3]>) {
    let mut keys = BTreeMap::new();
    let mut positions = vec![];
    let ids = mesh
        .positions
        .iter()
        .map(|p| {
            *keys
                .entry(p.map(|x| if x == 0.0 { 0 } else { x.to_bits() }))
                .or_insert_with(|| {
                    let id = positions.len() as u32;
                    positions.push(*p);
                    id
                })
        })
        .collect();
    (ids, positions)
}
/// Match roundoff-equivalent seam positions for shading only. A UV sphere's
/// sin(2π) endpoint can differ by a few ULPs; geometric subdivision remains
/// exact-position based. No surface positions are written by this grouping.
fn shading_ids(mesh: &Mesh) -> Vec<u32> {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for p in &mesh.positions {
        min = min.min(Vec3::from_array(*p));
        max = max.max(Vec3::from_array(*p));
    }
    let epsilon = ((max - min).max_element() * 2.0 * f32::EPSILON).max(1e-12);
    let mut buckets = std::collections::HashMap::<[i64; 3], Vec<u32>>::new();
    let mut positions = Vec::<Vec3>::new();
    mesh.positions
        .iter()
        .map(|p| {
            let p = Vec3::from_array(*p);
            let key = p
                .to_array()
                .map(|x| (x as f64 / epsilon as f64).floor() as i64);
            let mut found = None;
            'search: for x in -1..=1 {
                for y in -1..=1 {
                    for z in -1..=1 {
                        if let Some(ids) = buckets.get(&[key[0] + x, key[1] + y, key[2] + z]) {
                            for &id in ids {
                                if (positions[id as usize] - p).length_squared()
                                    <= epsilon * epsilon
                                {
                                    found = Some(id);
                                    break 'search;
                                }
                            }
                        }
                    }
                }
            }
            found.unwrap_or_else(|| {
                let id = positions.len() as u32;
                positions.push(p);
                buckets.entry(key).or_default().push(id);
                id
            })
        })
        .collect()
}
fn root(parents: &mut [usize], mut i: usize) -> usize {
    while parents[i] != i {
        parents[i] = parents[parents[i]];
        i = parents[i];
    }
    i
}
/// Rebuild angle-weighted corner normals across manifold edges below `degrees`.
/// Positions, winding and all per-corner attributes remain bit-identical. UV
/// seams share normals but keep independent render vertices. 0 means faceted.
pub fn crease_normals(mesh: &Mesh, degrees: f32) -> Mesh {
    let ids = shading_ids(mesh);
    let mut edges = BTreeMap::<(u32, u32), Vec<(usize, usize)>>::new();
    let mut faces = Vec::new();
    let mut parents: Vec<_> = (0..mesh.indices.len()).collect();
    for (f, t) in mesh.indices.chunks_exact(3).enumerate() {
        let p = t.map_array(mesh);
        faces.push((p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero());
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            let (u, v) = (ids[t[a] as usize], ids[t[b] as usize]);
            let corners = if u < v {
                (f * 3 + a, f * 3 + b)
            } else {
                (f * 3 + b, f * 3 + a)
            };
            edges.entry((u.min(v), u.max(v))).or_default().push(corners);
        }
    }
    let threshold = degrees.to_radians().cos();
    if degrees > 0.0 {
        for adjacent in edges.values() {
            if let [a, b] = adjacent.as_slice() {
                if faces[a.0 / 3].dot(faces[b.0 / 3]) + 1e-6 >= threshold {
                    for (x, y) in [(a.0, b.0), (a.1, b.1)] {
                        let x = root(&mut parents, x);
                        let y = root(&mut parents, y);
                        parents[y] = x;
                    }
                }
            }
        }
    }
    let mut sums = vec![Vec3::ZERO; parents.len()];
    for (f, t) in mesh.indices.chunks_exact(3).enumerate() {
        let p = t.map_array(mesh);
        for c in 0..3 {
            let a = (p[(c + 1) % 3] - p[c]).normalize_or_zero();
            let b = (p[(c + 2) % 3] - p[c]).normalize_or_zero();
            let angle = a.cross(b).length().atan2(a.dot(b));
            let r = root(&mut parents, f * 3 + c);
            sums[r] += faces[f] * angle;
        }
    }
    let mut out = Mesh::default();
    let mut vertices = BTreeMap::new();
    for (corner, &old) in mesh.indices.iter().enumerate() {
        let r = root(&mut parents, corner);
        let id = *vertices.entry((old, r)).or_insert_with(|| {
            let i = old as usize;
            let id = out.positions.len() as u32;
            out.positions.push(mesh.positions[i]);
            let normal = sums[r].normalize_or_zero();
            out.normals.push(
                if normal == Vec3::ZERO {
                    Vec3::Y
                } else {
                    normal
                }
                .to_array(),
            );
            if mesh.has_uvs() {
                out.uvs.push(mesh.uvs[i]);
            }
            if !mesh.colors.is_empty() {
                out.colors.push(mesh.colors[i]);
            }
            if !mesh.joints.is_empty() {
                out.joints.push(mesh.joints[i]);
            }
            if !mesh.weights.is_empty() {
                out.weights.push(mesh.weights[i]);
            }
            id
        });
        out.indices.push(id);
    }
    out
}
trait TrianglePositions {
    fn map_array(&self, mesh: &Mesh) -> [Vec3; 3];
}
impl TrianglePositions for [u32] {
    fn map_array(&self, mesh: &Mesh) -> [Vec3; 3] {
        [0, 1, 2].map(|i| Vec3::from_array(mesh.positions[self[i] as usize]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crease_splits_preserve_corner_attributes_and_winding() {
        let mesh = Mesh {
            positions: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            indices: vec![0, 1, 2, 1, 0, 3],
            normals: vec![[0.; 3]; 4],
            uvs: vec![[0., 0.], [1., 0.], [0., 1.], [1., 1.]],
            colors: vec![[0.2, 0.3, 0.4, 1.]; 4],
            ..Default::default()
        };
        let sharp = crease_normals(&mesh, 40.);
        let smooth = crease_normals(&mesh, 180.);
        assert_eq!(sharp.positions.len(), 6);
        assert_eq!(smooth.positions.len(), 4);
        for (&a, &b) in mesh.indices.iter().zip(&sharp.indices) {
            assert_eq!(mesh.positions[a as usize], sharp.positions[b as usize]);
            assert_eq!(mesh.uvs[a as usize], sharp.uvs[b as usize]);
            assert_eq!(mesh.colors[a as usize], sharp.colors[b as usize]);
        }
        assert_eq!(sharp.normals[sharp.indices[0] as usize], [0., 0., 1.]);
        for n in &smooth.normals {
            assert!((Vec3::from_array(*n).length() - 1.).abs() < 1e-5);
        }
    }
    #[test]
    fn uv_seams_share_smooth_normals_without_merging_uvs() {
        let mesh = Mesh {
            positions: vec![
                [0., 0., 0.],
                [1., 0., 0.],
                [0., 1., 0.],
                [1., 0., 0.],
                [0., 0., 0.],
                [0., 0., 1.],
            ],
            indices: vec![0, 1, 2, 3, 4, 5],
            normals: vec![[0.; 3]; 6],
            uvs: vec![
                [0., 0.],
                [1., 0.],
                [0., 1.],
                [0.2, 0.3],
                [0.4, 0.5],
                [1., 1.],
            ],
            ..Default::default()
        };
        let out = crease_normals(&mesh, 180.);
        assert_eq!(out.normals[0], out.normals[4]);
        assert_ne!(out.uvs[0], out.uvs[4]);
    }
}
