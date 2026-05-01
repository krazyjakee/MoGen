//! Surface queries on triangulated meshes: closest point, distance, normal.
//!
//! Builds a flat triangle list with a coarse uniform-grid spatial index and
//! runs Eberly's point-to-triangle distance routine to answer "what's the
//! closest surface point to `p`?". Used by both the smooth-CSG seam fillet
//! and the `conform` deformation pass.
//!
//! Memory layout: triangles are stored as resolved `[Vec3; 3]` triplets
//! (rather than as indices into the source mesh) so queries don't have to
//! double-indirect through `mesh.indices`. Per-triangle face normals are
//! cached at build time; smooth vertex normals are copied from the source
//! mesh when present so closest-point queries can return barycentric-
//! interpolated normals (mirroring how the renderer shades the surface).
//!
//! The grid `for_each_candidate` walk is identical to the one previously
//! locked inside `csg_smooth`. It expands outward from the cell containing
//! the query point, ring-by-ring, and stops once no closer triangle could
//! live in any further ring.

use glam::Vec3;

use mogen_core::Mesh;

/// A point on a target mesh's surface, returned by [`SurfaceIndex::closest_point`].
#[derive(Debug, Clone, Copy)]
pub struct SurfacePoint {
    /// World-space (mesh-local) position of the closest surface point.
    pub pos: Vec3,
    /// Smooth normal at that point. If the source mesh had vertex normals,
    /// this is the barycentric blend of the triangle's three vertex normals;
    /// otherwise it's the triangle's face normal.
    pub normal: Vec3,
    /// Index of the owning triangle in the source mesh's triangle list
    /// (not vertex list; multiply by 3 to get into `mesh.indices`).
    pub tri: u32,
    /// Barycentric coordinates `[wa, wb, wc]` such that
    /// `pos = wa*A + wb*B + wc*C` with `wa + wb + wc == 1`.
    pub bary: Vec3,
}

/// Spatial index over a mesh's triangles supporting closest-point and
/// distance queries.
pub struct SurfaceIndex {
    tris: Vec<[Vec3; 3]>,
    aabbs: Vec<(Vec3, Vec3)>,
    grid: Option<UniformGrid>,
    tri_normals: Vec<Vec3>,
    /// Per-triangle vertex normals as parallel triplets to `tris`. Empty when
    /// the source mesh has no vertex normals.
    vert_normals: Vec<[Vec3; 3]>,
}

impl SurfaceIndex {
    /// Build an index from a mesh. O(N) in triangle count; the grid build
    /// is the dominant term.
    pub fn build(mesh: &Mesh) -> Self {
        let tri_count = mesh.indices.len() / 3;
        let mut tris = Vec::with_capacity(tri_count);
        let mut aabbs = Vec::with_capacity(tri_count);
        let mut tri_normals = Vec::with_capacity(tri_count);
        let has_vnorms = mesh.normals.len() == mesh.positions.len() && !mesh.normals.is_empty();
        let mut vert_normals = if has_vnorms {
            Vec::with_capacity(tri_count)
        } else {
            Vec::new()
        };

        for chunk in mesh.indices.chunks_exact(3) {
            let ia = chunk[0] as usize;
            let ib = chunk[1] as usize;
            let ic = chunk[2] as usize;
            let a = Vec3::from_array(mesh.positions[ia]);
            let b = Vec3::from_array(mesh.positions[ib]);
            let c = Vec3::from_array(mesh.positions[ic]);
            tris.push([a, b, c]);
            let lo = a.min(b).min(c);
            let hi = a.max(b).max(c);
            aabbs.push((lo, hi));
            let face_n = (b - a).cross(c - a).normalize_or_zero();
            tri_normals.push(face_n);
            if has_vnorms {
                vert_normals.push([
                    Vec3::from_array(mesh.normals[ia]),
                    Vec3::from_array(mesh.normals[ib]),
                    Vec3::from_array(mesh.normals[ic]),
                ]);
            }
        }
        let grid = UniformGrid::build(&aabbs);
        Self { tris, aabbs, grid, tri_normals, vert_normals }
    }

    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// Closest point on the surface to `p`. Returns `None` only for an
    /// empty index.
    pub fn closest_point(&self, p: Vec3) -> Option<SurfacePoint> {
        if self.tris.is_empty() {
            return None;
        }

        let mut best_d = f32::INFINITY;
        let mut best_tri = 0u32;
        let mut best_pos = Vec3::ZERO;
        let mut best_bary = Vec3::ZERO;

        match &self.grid {
            Some(grid) => {
                grid.for_each_candidate(p, &mut |tri_idx, _current_best| {
                    let (lo, hi) = self.aabbs[tri_idx];
                    if aabb_distance(p, lo, hi) < best_d {
                        let (d, proj, bary) = point_triangle_closest(p, &self.tris[tri_idx]);
                        if d < best_d {
                            best_d = d;
                            best_tri = tri_idx as u32;
                            best_pos = proj;
                            best_bary = bary;
                        }
                    }
                    best_d
                });
            }
            None => {
                for i in 0..self.tris.len() {
                    let (lo, hi) = self.aabbs[i];
                    if aabb_distance(p, lo, hi) >= best_d {
                        continue;
                    }
                    let (d, proj, bary) = point_triangle_closest(p, &self.tris[i]);
                    if d < best_d {
                        best_d = d;
                        best_tri = i as u32;
                        best_pos = proj;
                        best_bary = bary;
                    }
                }
            }
        }

        let normal = self.normal_at(best_tri as usize, best_bary);
        Some(SurfacePoint { pos: best_pos, normal, tri: best_tri, bary: best_bary })
    }

    /// Unsigned distance from `p` to the surface. Returns `f32::INFINITY`
    /// for an empty index.
    pub fn unsigned_distance(&self, p: Vec3) -> f32 {
        if self.tris.is_empty() {
            return f32::INFINITY;
        }
        let mut best = f32::INFINITY;

        match &self.grid {
            Some(grid) => {
                grid.for_each_candidate(p, &mut |tri_idx, current_best| {
                    let (lo, hi) = self.aabbs[tri_idx];
                    if aabb_distance(p, lo, hi) >= current_best {
                        return current_best;
                    }
                    let d = point_triangle_distance(p, &self.tris[tri_idx]);
                    if d < best {
                        best = d;
                    }
                    best
                });
            }
            None => {
                for (i, tri) in self.tris.iter().enumerate() {
                    let (lo, hi) = self.aabbs[i];
                    if aabb_distance(p, lo, hi) >= best {
                        continue;
                    }
                    let d = point_triangle_distance(p, tri);
                    if d < best {
                        best = d;
                    }
                }
            }
        }

        best
    }

    fn normal_at(&self, tri: usize, bary: Vec3) -> Vec3 {
        if let Some(vn) = self.vert_normals.get(tri) {
            let blended = vn[0] * bary.x + vn[1] * bary.y + vn[2] * bary.z;
            let n = blended.normalize_or_zero();
            if n != Vec3::ZERO {
                return n;
            }
        }
        self.tri_normals[tri]
    }
}

/// Coarse uniform grid that maps each cell to the indices of triangles
/// whose AABB overlaps it. Point queries expand outward from the
/// containing cell until they hit a non-empty ring.
struct UniformGrid {
    origin: Vec3,
    cell: Vec3,
    res: [i32; 3],
    /// `cells[idx]` holds triangle indices for that voxel.
    cells: Vec<Vec<u32>>,
}

impl UniformGrid {
    fn build(aabbs: &[(Vec3, Vec3)]) -> Option<Self> {
        if aabbs.is_empty() {
            return None;
        }
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for (lo, hi) in aabbs {
            min = min.min(*lo);
            max = max.max(*hi);
        }
        let extent = (max - min).max(Vec3::splat(1e-4));
        // Aim for ~cube_root(N) cells per axis so each cell holds O(1)
        // triangles on average. Clamped so we don't go silly on tiny
        // meshes or blow up on giant ones.
        let target = ((aabbs.len() as f32).cbrt()).clamp(2.0, 32.0);
        let res = [
            (target * extent.x / extent.max_element()).max(1.0).ceil() as i32,
            (target * extent.y / extent.max_element()).max(1.0).ceil() as i32,
            (target * extent.z / extent.max_element()).max(1.0).ceil() as i32,
        ];
        let cell = Vec3::new(
            extent.x / res[0] as f32,
            extent.y / res[1] as f32,
            extent.z / res[2] as f32,
        );
        let total = (res[0] * res[1] * res[2]) as usize;
        let mut cells: Vec<Vec<u32>> = (0..total).map(|_| Vec::new()).collect();

        for (i, (lo, hi)) in aabbs.iter().enumerate() {
            let lo_c = world_to_cell(*lo, min, cell, res);
            let hi_c = world_to_cell(*hi, min, cell, res);
            for cz in lo_c[2]..=hi_c[2] {
                for cy in lo_c[1]..=hi_c[1] {
                    for cx in lo_c[0]..=hi_c[0] {
                        let idx = ((cz * res[1] + cy) * res[0] + cx) as usize;
                        cells[idx].push(i as u32);
                    }
                }
            }
        }

        Some(Self { origin: min, cell, res, cells })
    }

    fn cell_index(&self, c: [i32; 3]) -> usize {
        ((c[2] * self.res[1] + c[1]) * self.res[0] + c[0]) as usize
    }

    /// Expand outward from the cell containing `p` ring-by-ring; for every
    /// candidate triangle index, call `f(tri_idx, current_best)` and use
    /// its return value as the new running best distance. Stops when the
    /// nearest possible distance to any further ring exceeds the running
    /// best (no closer triangle can exist beyond that point).
    fn for_each_candidate<F: FnMut(usize, f32) -> f32>(&self, p: Vec3, f: &mut F) {
        let center = world_to_cell(p, self.origin, self.cell, self.res);
        let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut radius = 0;
        let max_radius = self.res[0].max(self.res[1]).max(self.res[2]);
        let mut best = f32::INFINITY;

        loop {
            let mut any_in_ring = false;
            let lo = [
                (center[0] - radius).max(0),
                (center[1] - radius).max(0),
                (center[2] - radius).max(0),
            ];
            let hi = [
                (center[0] + radius).min(self.res[0] - 1),
                (center[1] + radius).min(self.res[1] - 1),
                (center[2] + radius).min(self.res[2] - 1),
            ];

            for cz in lo[2]..=hi[2] {
                for cy in lo[1]..=hi[1] {
                    for cx in lo[0]..=hi[0] {
                        // Only visit the outer shell of the ring on
                        // iterations beyond 0; cells inside were covered
                        // by smaller radii already.
                        if radius > 0 {
                            let on_shell = cx == lo[0]
                                || cx == hi[0]
                                || cy == lo[1]
                                || cy == hi[1]
                                || cz == lo[2]
                                || cz == hi[2];
                            if !on_shell {
                                continue;
                            }
                        }
                        let idx = self.cell_index([cx, cy, cz]);
                        if !visited.insert(idx) {
                            continue;
                        }
                        any_in_ring = true;
                        for &tri in &self.cells[idx] {
                            best = f(tri as usize, best);
                        }
                    }
                }
            }

            radius += 1;
            // Stop once the ring is provably farther than the best
            // distance we've already found — outer rings can't help.
            let ring_dist = (radius as f32 - 1.0)
                * self.cell.x.min(self.cell.y).min(self.cell.z);
            if ring_dist >= best && radius > 1 {
                break;
            }
            if radius > max_radius {
                break;
            }
            if !any_in_ring && radius > max_radius {
                break;
            }
        }
    }
}

fn world_to_cell(p: Vec3, origin: Vec3, cell: Vec3, res: [i32; 3]) -> [i32; 3] {
    let r = (p - origin) / cell;
    [
        (r.x.floor() as i32).clamp(0, res[0] - 1),
        (r.y.floor() as i32).clamp(0, res[1] - 1),
        (r.z.floor() as i32).clamp(0, res[2] - 1),
    ]
}

fn aabb_distance(p: Vec3, lo: Vec3, hi: Vec3) -> f32 {
    let dx = (lo.x - p.x).max(0.0).max(p.x - hi.x);
    let dy = (lo.y - p.y).max(0.0).max(p.y - hi.y);
    let dz = (lo.z - p.z).max(0.0).max(p.z - hi.z);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Closest distance from a point to a triangle, after Eberly (Geometric
/// Tools). Returns 0 when `p` is on the triangle, never negative. Thin
/// wrapper around [`point_triangle_closest`] for callers that don't need
/// the closest point or barycentric coordinates.
fn point_triangle_distance(p: Vec3, tri: &[Vec3; 3]) -> f32 {
    point_triangle_closest(p, tri).0
}

/// Returns `(distance, closest_point, barycentric)`. `barycentric` is
/// `[wa, wb, wc]` with `wa + wb + wc == 1`, identifying the closest point
/// as `wa*A + wb*B + wc*C`.
fn point_triangle_closest(p: Vec3, tri: &[Vec3; 3]) -> (f32, Vec3, Vec3) {
    let [a, b, c] = *tri;
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;

    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return ((p - a).length(), a, Vec3::new(1.0, 0.0, 0.0));
    }

    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return ((p - b).length(), b, Vec3::new(0.0, 1.0, 0.0));
    }

    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        let proj = a + ab * v;
        return ((p - proj).length(), proj, Vec3::new(1.0 - v, v, 0.0));
    }

    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return ((p - c).length(), c, Vec3::new(0.0, 0.0, 1.0));
    }

    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let v = d2 / (d2 - d6);
        let proj = a + ac * v;
        return ((p - proj).length(), proj, Vec3::new(1.0 - v, 0.0, v));
    }

    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let v = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        let proj = b + (c - b) * v;
        return ((p - proj).length(), proj, Vec3::new(0.0, 1.0 - v, v));
    }

    // Inside the triangle's projected region — barycentric on the plane.
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    let proj = a + ab * v + ac * w;
    ((p - proj).length(), proj, Vec3::new(1.0 - v - w, v, w))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{plane_mesh, sphere_mesh};
    use mogen_core::UvMode;

    #[test]
    fn point_triangle_distance_basic() {
        let tri = [Vec3::ZERO, Vec3::X, Vec3::Y];
        // On a vertex
        assert!(point_triangle_distance(Vec3::ZERO, &tri) < 1e-6);
        // Above centroid
        let centroid = (tri[0] + tri[1] + tri[2]) / 3.0;
        let p = centroid + Vec3::Z * 2.0;
        let d = point_triangle_distance(p, &tri);
        assert!((d - 2.0).abs() < 1e-4);
        // Outside, off the X edge
        let p = Vec3::new(0.5, -1.0, 0.0);
        let d = point_triangle_distance(p, &tri);
        assert!((d - 1.0).abs() < 1e-4);
    }

    #[test]
    fn point_triangle_closest_returns_consistent_barycentric() {
        let tri = [Vec3::ZERO, Vec3::X, Vec3::Y];
        // Above centroid: barycentric should be ~(1/3, 1/3, 1/3) and the
        // reconstructed point should match `proj`.
        let centroid = (tri[0] + tri[1] + tri[2]) / 3.0;
        let p = centroid + Vec3::Z * 2.0;
        let (_, proj, bary) = point_triangle_closest(p, &tri);
        assert!((bary.x - 1.0 / 3.0).abs() < 1e-4);
        assert!((bary.y - 1.0 / 3.0).abs() < 1e-4);
        assert!((bary.z - 1.0 / 3.0).abs() < 1e-4);
        let reconstructed = tri[0] * bary.x + tri[1] * bary.y + tri[2] * bary.z;
        assert!((reconstructed - proj).length() < 1e-4);
    }

    #[test]
    fn closest_point_on_plane_lies_in_plane() {
        let mesh = plane_mesh([2.0, 2.0], UvMode::default());
        let idx = SurfaceIndex::build(&mesh);
        let sp = idx.closest_point(Vec3::new(0.3, 1.5, -0.4)).unwrap();
        // Plane is XZ at y=0; closest projection should land at y≈0.
        assert!(sp.pos.y.abs() < 1e-4, "pos = {:?}", sp.pos);
        // Normal should point up (+Y).
        assert!(sp.normal.y > 0.99, "normal = {:?}", sp.normal);
        // Distance from the query point to the projection equals the y component.
        let d = idx.unsigned_distance(Vec3::new(0.3, 1.5, -0.4));
        assert!((d - 1.5).abs() < 1e-4);
    }

    #[test]
    fn closest_point_on_sphere_has_radial_normal() {
        let mesh = sphere_mesh(1.0, 16, 24, UvMode::default());
        let idx = SurfaceIndex::build(&mesh);
        // Query a point well outside the sphere along +X. Closest surface
        // point should be near (1, 0, 0) with normal pointing +X (the
        // sphere has smooth vertex normals).
        let sp = idx.closest_point(Vec3::new(2.0, 0.0, 0.0)).unwrap();
        assert!((sp.pos - Vec3::X).length() < 0.1);
        assert!(sp.normal.x > 0.95, "normal = {:?}", sp.normal);
    }

    #[test]
    fn empty_mesh_has_infinite_distance() {
        let m = mogen_core::Mesh::default();
        let idx = SurfaceIndex::build(&m);
        assert!(idx.is_empty());
        assert_eq!(idx.unsigned_distance(Vec3::ZERO), f32::INFINITY);
        assert!(idx.closest_point(Vec3::ZERO).is_none());
    }
}
