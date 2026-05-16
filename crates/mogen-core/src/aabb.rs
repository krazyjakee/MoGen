//! Axis-aligned bounding boxes in local or world space. Used by the DSL
//! lowering pass to synthesize default connectors on groups/CSG nodes, and by
//! the validator to detect disconnected part clusters.

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

use crate::{Mesh, NodeId, SceneGraph};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn empty() -> Self {
        Self { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) }
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    pub fn from_points<'a>(points: impl IntoIterator<Item = &'a [f32; 3]>) -> Self {
        let mut a = Self::empty();
        for p in points {
            a.expand(Vec3::from_array(*p));
        }
        a
    }

    pub fn from_mesh(mesh: &Mesh) -> Self {
        Self::from_points(&mesh.positions)
    }

    pub fn expand(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn merge(&mut self, other: Aabb) {
        if other.is_empty() {
            return;
        }
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    pub fn center(&self) -> Vec3 {
        0.5 * (self.min + self.max)
    }

    /// Return the 8 corners of the AABB — used when transforming into another
    /// space where the axis-aligned property is not preserved.
    pub fn corners(&self) -> [Vec3; 8] {
        let (lo, hi) = (self.min, self.max);
        [
            Vec3::new(lo.x, lo.y, lo.z),
            Vec3::new(hi.x, lo.y, lo.z),
            Vec3::new(lo.x, hi.y, lo.z),
            Vec3::new(hi.x, hi.y, lo.z),
            Vec3::new(lo.x, lo.y, hi.z),
            Vec3::new(hi.x, lo.y, hi.z),
            Vec3::new(lo.x, hi.y, hi.z),
            Vec3::new(hi.x, hi.y, hi.z),
        ]
    }

    /// Transform by `m` and re-axis-align. For rotated boxes the result is the
    /// tight AABB of the 8 transformed corners.
    pub fn transformed(&self, m: Mat4) -> Aabb {
        if self.is_empty() {
            return *self;
        }
        let mut out = Aabb::empty();
        for c in self.corners() {
            out.expand(m.transform_point3(c));
        }
        out
    }

    /// Grow the AABB outward by `pad` on every axis.
    pub fn inflated(&self, pad: f32) -> Aabb {
        if self.is_empty() {
            return *self;
        }
        Aabb {
            min: self.min - Vec3::splat(pad),
            max: self.max + Vec3::splat(pad),
        }
    }

    /// True if `self` and `other` overlap or touch (inclusive of boundaries).
    pub fn intersects(&self, other: &Aabb) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
            && self.min.z <= other.max.z
            && self.max.z >= other.min.z
    }
}

/// Local-space AABB of a node's subtree: its own mesh plus each child's AABB
/// transformed into the node's local space. Returns `None` if the subtree
/// contains no mesh vertices.
pub fn subtree_local_aabb(graph: &SceneGraph, node_id: NodeId) -> Option<Aabb> {
    let node = &graph.nodes[node_id.0 as usize];
    let mut aabb = Aabb::empty();
    if let Some(mesh) = &node.mesh {
        aabb.merge(Aabb::from_mesh(mesh));
    }
    for child_id in &node.children {
        if let Some(child_aabb) = subtree_local_aabb(graph, *child_id) {
            let child = &graph.nodes[child_id.0 as usize];
            aabb.merge(child_aabb.transformed(child.transform.to_mat4()));
        }
    }
    if aabb.is_empty() { None } else { Some(aabb) }
}

/// World-space AABB of a node's own mesh (excluding descendants). Returns
/// `None` if the node has no mesh.
pub fn node_world_aabb(graph: &SceneGraph, node_id: NodeId, world: Mat4) -> Option<Aabb> {
    let node = &graph.nodes[node_id.0 as usize];
    let mesh = node.mesh.as_ref()?;
    let local = Aabb::from_mesh(mesh);
    if local.is_empty() {
        return None;
    }
    Some(local.transformed(world))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersects_touching_edges() {
        let a = Aabb { min: Vec3::ZERO, max: Vec3::ONE };
        let b = Aabb { min: Vec3::new(1.0, 0.0, 0.0), max: Vec3::new(2.0, 1.0, 1.0) };
        assert!(a.intersects(&b));
    }

    #[test]
    fn disjoint_boxes() {
        let a = Aabb { min: Vec3::ZERO, max: Vec3::ONE };
        let b = Aabb { min: Vec3::splat(2.0), max: Vec3::splat(3.0) };
        assert!(!a.intersects(&b));
    }

    #[test]
    fn inflate_brings_near_misses_into_contact() {
        let a = Aabb { min: Vec3::ZERO, max: Vec3::ONE };
        let b = Aabb { min: Vec3::new(1.001, 0.0, 0.0), max: Vec3::new(2.0, 1.0, 1.0) };
        assert!(!a.intersects(&b));
        assert!(a.inflated(0.002).intersects(&b));
    }

    #[test]
    fn rotated_transform_is_still_tight() {
        let unit = Aabb { min: Vec3::splat(-0.5), max: Vec3::splat(0.5) };
        let m = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_4);
        let rotated = unit.transformed(m);
        // Rotated unit cube has half-extent sqrt(2)/2 ≈ 0.707 along X and Z.
        assert!((rotated.max.x - 0.70710677).abs() < 1e-4);
        assert!((rotated.max.y - 0.5).abs() < 1e-4);
    }
}
