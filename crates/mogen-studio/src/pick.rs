//! Screen-space picking: convert a cursor position into the `NodeId` of the
//! triangle under the cursor. Runs on CPU against `FlatMesh::tri_node`.
//!
//! v1 is a flat Möller–Trumbore scan. Typical preview scenes are well under
//! 200k tris so the linear sweep is fine; if a prompt regression shows big
//! scenes pay for this per click, drop in a BVH without changing the API.

use eframe::egui::{Pos2, Rect};
use glam::{Mat4, Vec3, Vec4};
use mogen_core::NodeId;

use crate::viewer::{FlatMesh, OrbitCamera, FLOATS_PER_VERTEX};

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

    ray_pick(mesh, ray_origin, ray_dir)
}

fn unproject(inv_vp: &Mat4, ndc: Vec3) -> Vec3 {
    let p = *inv_vp * Vec4::new(ndc.x, ndc.y, ndc.z, 1.0);
    if p.w.abs() < 1e-6 {
        return Vec3::new(p.x, p.y, p.z);
    }
    Vec3::new(p.x / p.w, p.y / p.w, p.z / p.w)
}

/// Walk every triangle, Möller–Trumbore, keep the nearest positive-t hit.
/// TODO: BVH if a scene with > 200k tris starts showing up in prompts.
fn ray_pick(mesh: &FlatMesh, ro: Vec3, rd: Vec3) -> Option<NodeId> {
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
    best.map(|(_, n)| n)
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
        assert_eq!(hit, Some(id));
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
        assert_eq!(hit, None);
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
        assert_eq!(hit, Some(near));
    }
}
