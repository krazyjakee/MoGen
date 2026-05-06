//! Metaball / blob primitive — N implicit-field spheres unioned with smooth
//! blending.
//!
//! Authors today get the same effect by writing N explicit `sphere` nodes
//! inside a `union (smooth=k) { … }`, but that requires hand-authoring each
//! centre and radius inline. `metaball` collapses the entire pattern to one
//! node with a `points=` list of centres, a `radii=` list (or scalar
//! `radius=`) and a single `blend=` knob.
//!
//! Implementation reuses `union_smooth` (the existing vertex-fillet
//! approximation in `crate::csg_smooth`); when `blend <= 0` we collapse to
//! sharp `union_many` for cheaper output.
//!
//! Use cases: creature bodies (torso + thigh masses), slime, clouds, cell
//! clusters, pumpkin lobes, soft ammo pouches, asymmetric organic props.

use glam::{Mat4, Vec3};
use mogen_core::{Mesh, UvMode};

use crate::primitives::sphere::sphere_mesh;
use crate::xform::transform_mesh;
use crate::{union_many, union_smooth};

/// Build a metaball: N spheres centred at `points` with radii `radii`,
/// unioned with `union_smooth(blend)`. Returns an empty mesh if `points`
/// is empty.
///
/// Per-point radii: callers pass a `radii` slice with at least one element.
/// `radii.len() == 1` broadcasts that scalar across every point;
/// `radii.len() == points.len()` uses one per point. Anything in between is
/// clamped against `points.len() - 1` (the last radius is reused for any
/// extra points, matching `spline_tube_mesh`'s convention).
///
/// `blend` is the smooth-union radius in metres (passed straight to
/// `union_smooth`); `blend <= 0` collapses to sharp `union_many`.
///
/// `rings` / `segments` set the per-sphere tessellation. Defaults at the
/// DSL layer are smaller than `sphere`'s defaults because metaballs stack N
/// spheres and the resulting CSG cleanup is the actual cost driver, not
/// per-sphere tris.
pub fn metaball_mesh(
    points: &[[f32; 3]],
    radii: &[f32],
    blend: f32,
    rings: u32,
    segments: u32,
    mode: UvMode,
) -> Mesh {
    if points.is_empty() {
        return Mesh::default();
    }
    if radii.is_empty() {
        return Mesh::default();
    }

    // Build one sphere per centre, transformed into the metaball's shared
    // frame. We reuse `transform_mesh` so the spheres compose correctly
    // under `union_*` (which expects everything in one coordinate system).
    let radius_at = |i: usize| -> f32 {
        if radii.len() == 1 {
            return radii[0];
        }
        radii[i.min(radii.len() - 1)]
    };

    let meshes: Vec<Mesh> = points
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let r = radius_at(i).max(1e-4);
            let centre = Vec3::new(c[0], c[1], c[2]);
            let s = sphere_mesh(r, rings, segments, mode);
            transform_mesh(&s, Mat4::from_translation(centre))
        })
        .collect();

    if meshes.len() == 1 {
        return meshes.into_iter().next().unwrap();
    }
    if blend > 0.0 && blend.is_finite() {
        union_smooth(&meshes, blend)
    } else {
        union_many(&meshes)
    }
}

#[cfg(all(test, feature = "csg"))]
mod tests {
    use super::*;

    #[test]
    fn single_point_returns_one_sphere() {
        let m = metaball_mesh(
            &[[0.0, 0.0, 0.0]], &[0.5], 0.0, 8, 12, UvMode::Fit,
        );
        // Vertex count matches a plain sphere with the same tess.
        let s = sphere_mesh(0.5, 8, 12, UvMode::Fit);
        assert_eq!(m.positions.len(), s.positions.len());
    }

    #[test]
    fn empty_points_returns_empty() {
        let m = metaball_mesh(&[], &[0.5], 0.0, 8, 12, UvMode::Fit);
        assert!(m.positions.is_empty());
    }

    #[test]
    fn three_points_produce_united_mesh() {
        // Three overlapping spheres along X; the union must produce
        // a non-empty result with X spread > the largest single sphere.
        let m = metaball_mesh(
            &[[-0.4, 0.0, 0.0], [0.0, 0.0, 0.0], [0.4, 0.0, 0.0]],
            &[0.4],
            0.1,
            12, 16,
            UvMode::Fit,
        );
        assert!(!m.positions.is_empty());
        let max_x = m.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
        let min_x = m.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
        assert!(max_x > 0.7, "expected union to extend past one sphere, got max_x={max_x}");
        assert!(min_x < -0.7, "expected union to extend past one sphere, got min_x={min_x}");
    }

    #[test]
    fn per_point_radii_apply() {
        // Pass two centres with different radii; the union extent on X
        // should match the larger sphere on each side.
        let m = metaball_mesh(
            &[[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
            &[0.3, 0.7],
            0.0,
            10, 14,
            UvMode::Fit,
        );
        let max_x = m.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
        let min_x = m.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
        // Right side: 1.0 + 0.7; left side: -1.0 - 0.3.
        assert!((max_x - 1.7).abs() < 0.05, "max_x off: {max_x}");
        assert!((min_x - (-1.3)).abs() < 0.05, "min_x off: {min_x}");
    }
}
