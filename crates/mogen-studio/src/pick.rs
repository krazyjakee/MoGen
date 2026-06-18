//! Screen-space picking: convert a cursor position into the `NodeId` of the
//! triangle under the cursor. Runs on CPU against `FlatMesh::tri_node`.
//!
//! Triangles are tested with Möller–Trumbore, accelerated by a median-split
//! [`Bvh`] built lazily per `FlatMesh` (see [`crate::viewer::FlatMesh`]). The
//! tree is exact — it visits the same triangles a flat scan would, just far
//! fewer — so big procedural scenes no longer stall on a per-click linear
//! sweep while small scenes pay a negligible one-time build.
//!
//! Light nodes carry no geometry, so [`pick_node_or_light`] augments the
//! triangle scan with a separate billboard pass: each light's halo is a
//! screen-aligned disc of fixed pixel radius, and a click landing inside any
//! halo selects that light directly. Geometry behind the halo still wins
//! when the cursor is outside every halo, so the user can pick lit objects
//! through the cracks between light icons.

use eframe::egui::{Pos2, Rect};
use glam::{Mat4, Vec3, Vec4};
use mogen_core::NodeId;

use crate::gizmo::GIZMO_PIXEL_RADIUS;
use crate::viewer::{FlatMesh, OrbitCamera, ResolvedLight, FLOATS_PER_VERTEX};

/// Click radius around a light's projected halo, in viewport pixels. Matches
/// the halo glyph in `lights_gl.rs` (`handle_scale * 0.35` world units, where
/// `handle_scale` is calibrated against [`GIZMO_PIXEL_RADIUS`]) so the user
/// is clicking exactly the visible ring.
pub const LIGHT_HALO_PIXEL_RADIUS: f32 = GIZMO_PIXEL_RADIUS * 0.35;

/// Return the `NodeId` of the nearest triangle hit by a ray cast from the
/// camera through `cursor` (in egui screen coords). `None` when the cursor
/// misses the mesh entirely.
pub fn pick_node(
    camera: &OrbitCamera,
    viewport_rect: Rect,
    cursor: Pos2,
    mesh: &FlatMesh,
) -> Option<NodeId> {
    if mesh.indices.is_empty() || mesh.tri_node.is_empty() {
        return None;
    }
    let aspect = (viewport_rect.width() / viewport_rect.height()).max(0.01);
    let vp = camera.view_proj(aspect);
    let inv_vp = vp.inverse();

    // Convert egui screen coords to NDC. y flips because egui is top-down.
    let u = (cursor.x - viewport_rect.min.x) / viewport_rect.width().max(1.0);
    let v = (cursor.y - viewport_rect.min.y) / viewport_rect.height().max(1.0);
    let ndc_x = u * 2.0 - 1.0;
    let ndc_y = 1.0 - v * 2.0;

    let ray_origin = camera.eye();
    let far_world = unproject(&inv_vp, Vec3::new(ndc_x, ndc_y, 1.0));
    let ray_dir = (far_world - ray_origin).normalize_or_zero();
    if ray_dir.length_squared() < 1e-8 {
        return None;
    }

    ray_pick(mesh, ray_origin, ray_dir).map(|(_, n)| n)
}

/// Pick the nearest POI marker (`kind == "poi"`) under the cursor, ignoring
/// any non-POI geometry in front of it. `is_poi` reports whether a flattened
/// triangle's owning node is a POI marker.
///
/// POI debug spheres are scattered visualisation that frequently sits *inside*
/// enclosing geometry — a cave's rock shell wraps every marker, and building
/// walls hide markers in other rooms — so the depth-ordered [`pick_node`] would
/// report the enclosing surface instead and the marker would never tooltip.
/// Filtering the scan to POI triangles lets the nearest marker along the ray
/// win regardless of what occludes it, the same way [`pick_light`] lets a light
/// halo win over geometry behind it. This is what makes POI hover tooltips
/// behave identically across every generator (cave, building, …).
pub fn pick_poi(
    camera: &OrbitCamera,
    viewport_rect: Rect,
    cursor: Pos2,
    mesh: &FlatMesh,
    is_poi: impl Fn(NodeId) -> bool,
) -> Option<NodeId> {
    if mesh.indices.is_empty() || mesh.tri_node.is_empty() {
        return None;
    }
    let aspect = (viewport_rect.width() / viewport_rect.height()).max(0.01);
    let vp = camera.view_proj(aspect);
    let inv_vp = vp.inverse();

    let u = (cursor.x - viewport_rect.min.x) / viewport_rect.width().max(1.0);
    let v = (cursor.y - viewport_rect.min.y) / viewport_rect.height().max(1.0);
    let ndc_x = u * 2.0 - 1.0;
    let ndc_y = 1.0 - v * 2.0;

    let ray_origin = camera.eye();
    let far_world = unproject(&inv_vp, Vec3::new(ndc_x, ndc_y, 1.0));
    let ray_dir = (far_world - ray_origin).normalize_or_zero();
    if ray_dir.length_squared() < 1e-8 {
        return None;
    }

    ray_pick_filtered(mesh, ray_origin, ray_dir, &is_poi).map(|(_, n)| n)
}

/// Combined picker. First tests `lights` as billboard halos at
/// [`LIGHT_HALO_PIXEL_RADIUS`] pixels — a hit inside any halo wins
/// immediately, so a user clicking the visible icon always selects the
/// light even when geometry sits behind it. With no halo hit, falls back
/// to the triangle scan in [`pick_node`]. Returns the NodeId of the
/// selected light or scene node, or `None` when the cursor misses both.
pub fn pick_node_or_light(
    camera: &OrbitCamera,
    viewport_rect: Rect,
    cursor: Pos2,
    mesh: &FlatMesh,
    lights: &[ResolvedLight],
) -> Option<NodeId> {
    if let Some(id) = pick_light(camera, viewport_rect, cursor, lights) {
        return Some(id);
    }
    pick_node(camera, viewport_rect, cursor, mesh)
}

/// Project each light to screen space and return the NodeId of the one
/// whose projected halo is nearest the cursor — but only when the cursor
/// lies *inside* the halo (within [`LIGHT_HALO_PIXEL_RADIUS`] pixels).
/// Lights behind the camera (negative clip-space w) are skipped.
pub fn pick_light(
    camera: &OrbitCamera,
    viewport_rect: Rect,
    cursor: Pos2,
    lights: &[ResolvedLight],
) -> Option<NodeId> {
    if lights.is_empty() {
        return None;
    }
    let aspect = (viewport_rect.width() / viewport_rect.height()).max(0.01);
    let vp = camera.view_proj(aspect);
    let mut best: Option<(f32, NodeId)> = None;
    for l in lights {
        let Some(screen) = project_to_screen(vp, l.position, viewport_rect) else {
            continue;
        };
        let dx = screen.x - cursor.x;
        let dy = screen.y - cursor.y;
        let d2 = dx * dx + dy * dy;
        if d2 > LIGHT_HALO_PIXEL_RADIUS * LIGHT_HALO_PIXEL_RADIUS {
            continue;
        }
        match best {
            Some((bd, _)) if bd <= d2 => {}
            _ => best = Some((d2, l.node)),
        }
    }
    best.map(|(_, n)| n)
}

fn project_to_screen(viewproj: Mat4, world: Vec3, viewport: Rect) -> Option<Pos2> {
    let clip = viewproj * Vec4::new(world.x, world.y, world.z, 1.0);
    if clip.w <= 1e-4 {
        return None; // behind the camera
    }
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    let sx = viewport.min.x + (ndc_x * 0.5 + 0.5) * viewport.width();
    // egui's y origin is at the top; NDC y points up, so flip.
    let sy = viewport.min.y + (1.0 - (ndc_y * 0.5 + 0.5)) * viewport.height();
    Some(Pos2::new(sx, sy))
}

fn unproject(inv_vp: &Mat4, ndc: Vec3) -> Vec3 {
    let p = *inv_vp * Vec4::new(ndc.x, ndc.y, ndc.z, 1.0);
    if p.w.abs() < 1e-6 {
        return Vec3::new(p.x, p.y, p.z);
    }
    Vec3::new(p.x / p.w, p.y / p.w, p.z / p.w)
}

/// Nearest positive-t triangle hit along the ray, accelerated by the mesh's
/// lazily-built [`Bvh`]. The BVH is exact (it tests the same triangles a flat
/// scan would, just far fewer of them), so results match the old linear sweep.
fn ray_pick(mesh: &FlatMesh, ro: Vec3, rd: Vec3) -> Option<(f32, NodeId)> {
    mesh.picking_bvh().raycast(mesh, ro, rd, None)
}

/// `ray_pick` restricted to triangles whose owning node satisfies `keep`.
/// Used by [`pick_poi`] to find the nearest POI marker along the ray while
/// ignoring occluding geometry the predicate rejects.
fn ray_pick_filtered(
    mesh: &FlatMesh,
    ro: Vec3,
    rd: Vec3,
    keep: &impl Fn(NodeId) -> bool,
) -> Option<(f32, NodeId)> {
    mesh.picking_bvh()
        .raycast(mesh, ro, rd, Some(keep as &dyn Fn(NodeId) -> bool))
}

/// Median-split bounding-volume hierarchy over a flattened triangle soup.
///
/// Built once per `FlatMesh` (see [`FlatMesh::picking_bvh`]) and queried by
/// the screen-space picker. Nodes are stored in a flat arena; an interior node
/// references its two children by arena index, a leaf references a contiguous
/// run of triangles in `tri_order` (indices into the mesh's triangle list).
pub(crate) struct Bvh {
    nodes: Vec<BvhNode>,
    /// Triangle indices grouped so each leaf owns a contiguous slice.
    tri_order: Vec<u32>,
}

struct BvhNode {
    bmin: Vec3,
    bmax: Vec3,
    /// Leaf: start offset into `tri_order`. Interior: left child arena index.
    a: u32,
    /// Leaf: triangle count (> 0). Interior: right child arena index (`b` is
    /// only a count when `leaf` is set, so the two never collide).
    b: u32,
    leaf: bool,
}

/// Triangles per leaf. Small enough that the per-leaf linear test is cheap,
/// large enough to keep the tree shallow.
const BVH_LEAF_TRIS: usize = 4;

fn axis_val(v: Vec3, axis: usize) -> f32 {
    match axis {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

impl Bvh {
    pub(crate) fn build(vertices: &[f32], indices: &[u32], stride: usize) -> Bvh {
        let tri_count = indices.len() / 3;
        let mut tri_min = Vec::with_capacity(tri_count);
        let mut tri_max = Vec::with_capacity(tri_count);
        let mut centroid = Vec::with_capacity(tri_count);
        for t in 0..tri_count {
            let v0 = read_pos(vertices, indices[t * 3] as usize, stride);
            let v1 = read_pos(vertices, indices[t * 3 + 1] as usize, stride);
            let v2 = read_pos(vertices, indices[t * 3 + 2] as usize, stride);
            let mn = v0.min(v1).min(v2);
            let mx = v0.max(v1).max(v2);
            tri_min.push(mn);
            tri_max.push(mx);
            centroid.push((mn + mx) * 0.5);
        }
        let mut tri_order: Vec<u32> = (0..tri_count as u32).collect();
        let mut nodes: Vec<BvhNode> = Vec::new();
        if tri_count > 0 {
            build_node(
                &mut nodes,
                &mut tri_order,
                &tri_min,
                &tri_max,
                &centroid,
                0,
                tri_count,
            );
        }
        Bvh { nodes, tri_order }
    }

    /// Nearest triangle hit along the ray, optionally restricted to triangles
    /// whose owning node satisfies `keep`. Returns `(t, node)`.
    pub(crate) fn raycast(
        &self,
        mesh: &FlatMesh,
        ro: Vec3,
        rd: Vec3,
        keep: Option<&dyn Fn(NodeId) -> bool>,
    ) -> Option<(f32, NodeId)> {
        if self.nodes.is_empty() {
            return None;
        }
        let stride = FLOATS_PER_VERTEX;
        let inv = Vec3::new(1.0 / rd.x, 1.0 / rd.y, 1.0 / rd.z);
        let mut best: Option<(f32, NodeId)> = None;
        let mut stack: Vec<u32> = Vec::with_capacity(64);
        stack.push(0);
        while let Some(ni) = stack.pop() {
            let node = &self.nodes[ni as usize];
            let Some(t_enter) = slab(node.bmin, node.bmax, ro, inv) else {
                continue;
            };
            // Whole sub-tree is farther than the current best hit — skip it.
            if let Some((bt, _)) = best {
                if t_enter > bt {
                    continue;
                }
            }
            if node.leaf {
                for k in node.a..node.a + node.b {
                    let tri_i = self.tri_order[k as usize] as usize;
                    let node_id = mesh.tri_node.get(tri_i).copied().unwrap_or(NodeId(0));
                    if let Some(f) = keep {
                        if !f(node_id) {
                            continue;
                        }
                    }
                    let i0 = mesh.indices[tri_i * 3] as usize;
                    let i1 = mesh.indices[tri_i * 3 + 1] as usize;
                    let i2 = mesh.indices[tri_i * 3 + 2] as usize;
                    let v0 = read_pos(&mesh.vertices, i0, stride);
                    let v1 = read_pos(&mesh.vertices, i1, stride);
                    let v2 = read_pos(&mesh.vertices, i2, stride);
                    if let Some(t) = intersect_tri(ro, rd, v0, v1, v2) {
                        match best {
                            Some((bt, _)) if bt <= t => {}
                            _ => best = Some((t, node_id)),
                        }
                    }
                }
            } else {
                stack.push(node.a);
                stack.push(node.b);
            }
        }
        best
    }
}

/// Ray vs AABB slab test. Returns the entry distance (clamped to 0 when the
/// ray starts inside) or `None` on a miss. `inv` is the component-wise
/// reciprocal of the ray direction.
fn slab(bmin: Vec3, bmax: Vec3, ro: Vec3, inv: Vec3) -> Option<f32> {
    let t1 = (bmin - ro) * inv;
    let t2 = (bmax - ro) * inv;
    let tmin = t1.min(t2);
    let tmax = t1.max(t2);
    let t_enter = tmin.max_element();
    let t_exit = tmax.min_element();
    if t_exit >= t_enter.max(0.0) {
        Some(t_enter.max(0.0))
    } else {
        None
    }
}

/// Recursively build a node covering `tri_order[start..start + count]`, pushing
/// it (and its descendants) onto `nodes`. Returns the node's arena index.
fn build_node(
    nodes: &mut Vec<BvhNode>,
    tri_order: &mut [u32],
    tri_min: &[Vec3],
    tri_max: &[Vec3],
    centroid: &[Vec3],
    start: usize,
    count: usize,
) -> u32 {
    let mut bmin = Vec3::splat(f32::INFINITY);
    let mut bmax = Vec3::splat(f32::NEG_INFINITY);
    for &ti in &tri_order[start..start + count] {
        bmin = bmin.min(tri_min[ti as usize]);
        bmax = bmax.max(tri_max[ti as usize]);
    }
    let node_idx = nodes.len() as u32;
    nodes.push(BvhNode {
        bmin,
        bmax,
        a: start as u32,
        b: count as u32,
        leaf: true,
    });
    if count <= BVH_LEAF_TRIS {
        return node_idx;
    }

    // Split on the axis of greatest centroid spread, at the centroid midpoint.
    let mut cmin = Vec3::splat(f32::INFINITY);
    let mut cmax = Vec3::splat(f32::NEG_INFINITY);
    for &ti in &tri_order[start..start + count] {
        cmin = cmin.min(centroid[ti as usize]);
        cmax = cmax.max(centroid[ti as usize]);
    }
    let extent = cmax - cmin;
    let axis = if extent.x >= extent.y && extent.x >= extent.z {
        0
    } else if extent.y >= extent.z {
        1
    } else {
        2
    };
    if axis_val(extent, axis) < 1e-8 {
        return node_idx; // all centroids coincide — keep as a leaf
    }
    let mid = (axis_val(cmin, axis) + axis_val(cmax, axis)) * 0.5;

    let slice = &mut tri_order[start..start + count];
    let mut left = 0usize;
    for j in 0..slice.len() {
        if axis_val(centroid[slice[j] as usize], axis) < mid {
            slice.swap(left, j);
            left += 1;
        }
    }
    // Degenerate midpoint split (everything on one side): fall back to a median
    // split so we always make progress and the tree stays balanced.
    if left == 0 || left == count {
        slice.sort_unstable_by(|&p, &q| {
            axis_val(centroid[p as usize], axis)
                .partial_cmp(&axis_val(centroid[q as usize], axis))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        left = count / 2;
    }

    let left_child = build_node(nodes, tri_order, tri_min, tri_max, centroid, start, left);
    let right_child = build_node(
        nodes,
        tri_order,
        tri_min,
        tri_max,
        centroid,
        start + left,
        count - left,
    );
    nodes[node_idx as usize].a = left_child;
    nodes[node_idx as usize].b = right_child;
    nodes[node_idx as usize].leaf = false;
    node_idx
}

fn read_pos(vertices: &[f32], vi: usize, stride: usize) -> Vec3 {
    let base = vi * stride;
    Vec3::new(vertices[base], vertices[base + 1], vertices[base + 2])
}

fn intersect_tri(ro: Vec3, rd: Vec3, v0: Vec3, v1: Vec3, v2: Vec3) -> Option<f32> {
    const EPS: f32 = 1e-6;
    let edge1 = v1 - v0;
    let edge2 = v2 - v0;
    let h = rd.cross(edge2);
    let a = edge1.dot(h);
    if a.abs() < EPS {
        return None; // ray parallel to triangle
    }
    let f = 1.0 / a;
    let s = ro - v0;
    let u = f * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(edge1);
    let v = f * rd.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * edge2.dot(q);
    if t > EPS { Some(t) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::flatten::flatten;
    use mogen_core::{Mesh, SceneGraph, Transform};

    fn unit_quad_at(origin: Vec3) -> Mesh {
        let mut m = Mesh::new(
            vec![
                [origin.x - 0.5, origin.y - 0.5, origin.z],
                [origin.x + 0.5, origin.y - 0.5, origin.z],
                [origin.x + 0.5, origin.y + 0.5, origin.z],
                [origin.x - 0.5, origin.y + 0.5, origin.z],
            ],
            vec![[0.0, 0.0, 1.0]; 4],
            vec![0, 1, 2, 0, 2, 3],
        );
        m.uvs = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        m
    }

    #[test]
    fn ray_hits_forward_quad_returns_its_node() {
        let mut scene = SceneGraph::new();
        let id = scene.add_root("a", "box", Transform::IDENTITY);
        scene.set_mesh(id, unit_quad_at(Vec3::ZERO));
        let mesh = flatten(&scene, None);

        // Fire a ray from -Z toward origin, right at the quad.
        let hit = ray_pick(&mesh, Vec3::new(0.0, 0.0, -2.0), Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(hit.map(|(_, n)| n), Some(id));
    }

    #[test]
    fn ray_miss_returns_none() {
        let mut scene = SceneGraph::new();
        let id = scene.add_root("a", "box", Transform::IDENTITY);
        scene.set_mesh(id, unit_quad_at(Vec3::ZERO));
        let mesh = flatten(&scene, None);

        // Aim 10 units to the right — outside the [-0.5, 0.5] quad extent.
        let hit = ray_pick(
            &mesh,
            Vec3::new(10.0, 0.0, -2.0),
            Vec3::new(0.0, 0.0, 1.0),
        );
        assert!(hit.is_none());
    }

    #[test]
    fn poi_filter_picks_through_an_occluder() {
        // A near "box" occludes a far "poi". The unfiltered scan returns the
        // box (it's closer), but the POI-filtered scan ignores the box and
        // returns the marker behind it — the cave/building tooltip case.
        let mut scene = SceneGraph::new();
        let occluder = scene.add_root("shell", "box", Transform::IDENTITY);
        scene.set_mesh(occluder, unit_quad_at(Vec3::new(0.0, 0.0, 0.0)));
        let marker = scene.add_root("bed_0", "poi", Transform::IDENTITY);
        scene.set_mesh(marker, unit_quad_at(Vec3::new(0.0, 0.0, 1.0)));
        let mesh = flatten(&scene, None);

        let ro = Vec3::new(0.0, 0.0, -2.0);
        let rd = Vec3::new(0.0, 0.0, 1.0);
        assert_eq!(ray_pick(&mesh, ro, rd).map(|(_, n)| n), Some(occluder));

        let kinds: std::collections::HashMap<NodeId, String> = (0..scene.nodes.len())
            .map(|i| (NodeId(i as u32), scene.nodes[i].kind.clone()))
            .collect();
        let hit = ray_pick_filtered(&mesh, ro, rd, &|n| {
            kinds.get(&n).map(|k| k == "poi").unwrap_or(false)
        });
        assert_eq!(hit.map(|(_, n)| n), Some(marker));
    }

    #[test]
    fn ray_picks_the_closer_node() {
        let mut scene = SceneGraph::new();
        let near = scene.add_root("near", "box", Transform::IDENTITY);
        scene.set_mesh(near, unit_quad_at(Vec3::new(0.0, 0.0, 0.0)));
        let far = scene.add_root("far", "box", Transform::IDENTITY);
        scene.set_mesh(far, unit_quad_at(Vec3::new(0.0, 0.0, 1.0)));
        let mesh = flatten(&scene, None);

        let hit = ray_pick(&mesh, Vec3::new(0.0, 0.0, -2.0), Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(hit.map(|(_, n)| n), Some(near));
    }

    /// Brute-force reference: the linear Möller–Trumbore scan the BVH replaces.
    fn brute_pick(mesh: &FlatMesh, ro: Vec3, rd: Vec3) -> Option<(f32, NodeId)> {
        let stride = FLOATS_PER_VERTEX;
        let mut best: Option<(f32, NodeId)> = None;
        for tri_i in 0..mesh.indices.len() / 3 {
            let i0 = mesh.indices[tri_i * 3] as usize;
            let i1 = mesh.indices[tri_i * 3 + 1] as usize;
            let i2 = mesh.indices[tri_i * 3 + 2] as usize;
            let v0 = read_pos(&mesh.vertices, i0, stride);
            let v1 = read_pos(&mesh.vertices, i1, stride);
            let v2 = read_pos(&mesh.vertices, i2, stride);
            if let Some(t) = intersect_tri(ro, rd, v0, v1, v2) {
                let node = mesh.tri_node.get(tri_i).copied().unwrap_or(NodeId(0));
                match best {
                    Some((bt, _)) if bt <= t => {}
                    _ => best = Some((t, node)),
                }
            }
        }
        best
    }

    #[test]
    fn bvh_matches_brute_force_over_many_rays() {
        // A field of quads at distinct, well-separated depths (so the nearest
        // hit is never an ambiguous tie) including overlapping columns, then
        // fire a deterministic spread of rays and assert the accelerated pick
        // agrees with the brute-force scan on every one.
        let mut scene = SceneGraph::new();
        let mut lcg: u64 = 0x1234_5678;
        let mut next = || {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((lcg >> 33) as f32) / (u32::MAX as f32) // [0,1)
        };
        for i in 0..60 {
            let x = (next() - 0.5) * 6.0;
            let y = (next() - 0.5) * 6.0;
            // 0.5 spacing keeps depths distinct enough to avoid float ties.
            let z = i as f32 * 0.5;
            let id = scene.add_root(&format!("q{i}"), "box", Transform::IDENTITY);
            scene.set_mesh(id, unit_quad_at(Vec3::new(x, y, z)));
        }
        let mesh = flatten(&scene, None);
        assert!(mesh.indices.len() / 3 > BVH_LEAF_TRIS, "want a real tree");

        for _ in 0..400 {
            let ro = Vec3::new((next() - 0.5) * 8.0, (next() - 0.5) * 8.0, -5.0);
            let target = Vec3::new((next() - 0.5) * 8.0, (next() - 0.5) * 8.0, 20.0);
            let rd = (target - ro).normalize();
            let got = ray_pick(&mesh, ro, rd).map(|(_, n)| n);
            let want = brute_pick(&mesh, ro, rd).map(|(_, n)| n);
            assert_eq!(got, want, "ray {ro:?} -> {rd:?}");
        }
    }
}
