//! Screen-space picking: convert a cursor position into the `NodeId` of the
//! triangle under the cursor. Runs on CPU against `FlatMesh::tri_node`.
//!
//! v1 is a flat Möller–Trumbore scan. Typical preview scenes are well under
//! 200k tris so the linear sweep is fine; if a prompt regression shows big
//! scenes pay for this per click, drop in a BVH without changing the API.
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

/// Walk every triangle, Möller–Trumbore, keep the nearest positive-t hit.
/// TODO: BVH if a scene with > 200k tris starts showing up in prompts.
fn ray_pick(mesh: &FlatMesh, ro: Vec3, rd: Vec3) -> Option<(f32, NodeId)> {
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
}
