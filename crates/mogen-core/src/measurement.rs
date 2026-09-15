//! Measurements of the current static, tessellated scene in world coordinates.
use crate::{Aabb, NodeId, SceneGraph};
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

/// Triangle-surface distance, not signed penetration or a solid containment test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceDistance {
    pub distance: f64,
    pub closest_first: [f32; 3],
    pub closest_second: [f32; 3],
    pub direction_first_to_second: Option<[f32; 3]>,
    pub lower_bound: f64,
    pub exact: bool,
    pub tolerance: f64,
    pub status: String,
    pub work: usize,
    pub triangle_tests: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartMeasurement {
    pub name: String,
    pub bounds: Aabb,
    pub dimensions: [f32; 3],
    /// Signed distance to the default world ground plane, Y=0.
    pub ground_clearance: f32,
}

/// Tight bounds of rendered triangles; combine child bounds in world space.
/// Transforming local AABB corners alone overestimates rotated curved parts.
pub fn world_part_measurements(scene: &SceneGraph) -> Vec<Option<PartMeasurement>> {
    let worlds = scene.world_transforms();
    let mut bounds = vec![Aabb::empty(); scene.nodes.len()];
    for (i, node) in scene.nodes.iter().enumerate() {
        if let Some(mesh) = &node.mesh {
            for &index in &mesh.indices {
                if let Some(p) = mesh.positions.get(index as usize) {
                    bounds[i].expand(worlds[i].transform_point3(Vec3::from_array(*p)));
                }
            }
        }
    }
    let mut order = Vec::new();
    let mut pending = scene.roots.clone();
    while let Some(id) = pending.pop() {
        order.push(id);
        pending.extend(&scene.get(id).children);
    }
    for id in order.into_iter().rev() {
        if let Some(parent) = scene.get(id).parent {
            let child = bounds[id.0 as usize];
            bounds[parent.0 as usize].merge(child);
        }
    }
    bounds
        .into_iter()
        .enumerate()
        .map(|(i, bounds)| {
            (!bounds.is_empty()).then(|| PartMeasurement {
                name: scene.nodes[i].name.clone(),
                dimensions: (bounds.max - bounds.min).to_array(),
                ground_clearance: bounds.min.y,
                bounds,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipMeasurement {
    pub child: String,
    pub target: String,
    pub mode: String,
    pub intent: String,
    pub child_point: [f32; 3],
    pub target_point: [f32; 3],
    pub direction: [f32; 3],
    pub intended_signed_offset: f32,
    pub measured_signed_offset: f32,
    pub residual: f32,
    pub tolerance: f32,
    pub satisfied: bool,
    pub evidence: String,
}

/// Recheck semantic anchors and actual grounding vertices after transforms.
/// An anchor alignment is not a claim that curved triangle surfaces touch.
pub fn relationship_measurements(scene: &SceneGraph) -> Vec<RelationshipMeasurement> {
    let worlds = scene.world_transforms();
    scene
        .relationships
        .iter()
        .map(|r| {
            let tw = worlds[r.target.0 as usize];
            let normal = tw
                .inverse()
                .transpose()
                .transform_vector3(Vec3::from_array(r.target_normal))
                .normalize_or_zero();
            let target = tw.transform_point3(Vec3::from_array(r.target_anchor));
            let mut child =
                worlds[r.child.0 as usize].transform_point3(Vec3::from_array(r.child_anchor));
            if r.mode == "ground" {
                if let Some(p) = lowest_point(scene, r.child, &worlds, normal) {
                    child = p;
                }
            }
            let intended = r.clearance - r.insertion;
            let measured = (child - target).dot(normal);
            let residual = if r.mode == "ground" {
                (measured - intended).abs()
            } else {
                (child - target - normal * intended).length()
            };
            RelationshipMeasurement {
                child: scene.get(r.child).name.clone(),
                target: scene.get(r.target).name.clone(),
                mode: r.mode.clone(),
                intent: if r.insertion > 0.0 {
                    "intentional_insertion"
                } else if r.clearance > 0.0 {
                    "intentional_clearance"
                } else {
                    "contact"
                }
                .into(),
                child_point: child.to_array(),
                target_point: target.to_array(),
                direction: normal.to_array(),
                intended_signed_offset: intended,
                measured_signed_offset: measured,
                residual,
                tolerance: r.tolerance,
                satisfied: residual.is_finite() && residual <= r.tolerance,
                evidence: if r.mode == "ground" {
                    "rendered vertices against authored connector plane"
                } else {
                    "authored anchors; surface contact requires measure"
                }
                .into(),
            }
        })
        .collect()
}
fn lowest_point(scene: &SceneGraph, root: NodeId, worlds: &[Mat4], normal: Vec3) -> Option<Vec3> {
    let mut result: Option<Vec3> = None;
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        let node = scene.get(id);
        pending.extend(&node.children);
        if let Some(mesh) = &node.mesh {
            for &index in &mesh.indices {
                if let Some(p) = mesh.positions.get(index as usize) {
                    let p = worlds[id.0 as usize].transform_point3(Vec3::from_array(*p));
                    if result.is_none_or(|q| p.dot(normal) < q.dot(normal)) {
                        result = Some(p);
                    }
                }
            }
        }
    }
    result
}
