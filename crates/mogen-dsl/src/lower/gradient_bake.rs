//! Bake material gradients into per-vertex `Mesh.colors` (`COLOR_0`).
//!
//! Runs as the last lowering pass: every other transform that touches
//! geometry — primitive emission, CSG, conform, attach, skin binding — has
//! settled by now, so the AABB we sample against matches what the exporter
//! will see. Each mesh is sampled in *its own local frame*: a `linear` Y
//! gradient on a tall column covers the column's full height regardless of
//! where the column sits in the world, and a `radial` gradient on a sphere
//! falls off relative to that sphere's own bounds, not the scene's.
//!
//! The output is stored on `Mesh.colors`; the exporter packs it into
//! `COLOR_0`, which the glTF spec defines as a multiplier on `baseColorFactor`.

use mogen_core::{Aabb, Gradient, GradientAxis, GradientKind, SceneGraph};

pub fn bake_gradients(graph: &mut SceneGraph) {
    for ni in 0..graph.nodes.len() {
        // Snapshot the gradient (Clone is cheap — a Gradient is ~4 stops max in
        // practice) so we can drop the immutable borrow before mutating mesh
        // colours on the same node.
        let gradient = graph.nodes[ni]
            .material
            .and_then(|mid| graph.materials.get(mid.0 as usize))
            .and_then(|m| m.gradient.clone());
        let Some(gradient) = gradient else { continue };
        let Some(mesh) = graph.nodes[ni].mesh.as_mut() else { continue };
        if mesh.positions.is_empty() {
            continue;
        }
        let aabb = match local_aabb(&mesh.positions) {
            Some(a) => a,
            None => continue,
        };
        let mut colors = Vec::with_capacity(mesh.positions.len());
        for p in &mesh.positions {
            let t = sample_t(&gradient, &aabb, *p);
            colors.push(gradient.sample(t));
        }
        mesh.colors = colors;
    }
}

fn local_aabb(positions: &[[f32; 3]]) -> Option<Aabb> {
    if positions.is_empty() {
        return None;
    }
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in positions {
        for i in 0..3 {
            if p[i] < min[i] {
                min[i] = p[i];
            }
            if p[i] > max[i] {
                max[i] = p[i];
            }
        }
    }
    Some(Aabb {
        min: glam::Vec3::from_array(min),
        max: glam::Vec3::from_array(max),
    })
}

fn sample_t(g: &Gradient, aabb: &Aabb, p: [f32; 3]) -> f32 {
    match g.kind {
        GradientKind::Linear { axis } => {
            let (lo, hi, v) = match axis {
                GradientAxis::X => (aabb.min.x, aabb.max.x, p[0]),
                GradientAxis::Y => (aabb.min.y, aabb.max.y, p[1]),
                GradientAxis::Z => (aabb.min.z, aabb.max.z, p[2]),
            };
            let span = hi - lo;
            if span <= f32::EPSILON {
                0.0
            } else {
                ((v - lo) / span).clamp(0.0, 1.0)
            }
        }
        GradientKind::Radial => {
            // Sample as a fraction of the distance from the AABB centre to the
            // furthest corner. That keeps `t=0` planted at the centre and `t=1`
            // exactly at the silhouette regardless of the bounding shape's
            // proportions — a stretched box and a cube both reach full edge
            // colour at their actual edges.
            let center = (aabb.min + aabb.max) * 0.5;
            let half = (aabb.max - aabb.min) * 0.5;
            let radius = half.length().max(f32::EPSILON);
            let d = (glam::Vec3::from_array(p) - center).length();
            (d / radius).clamp(0.0, 1.0)
        }
    }
}
