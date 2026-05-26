//! Signed-distance primitives and smooth-blend helpers for the `blob`
//! container.
//!
//! Unlike `csg_smooth` (which is a vertex-fillet approximation applied to a
//! sharp mesh-CSG union), this module evaluates true SDFs on a sample grid;
//! `isosurface::blob_to_mesh` then meshes the field with surface nets.
//! Concave junctions (smooth eye sockets, blended nasal cavities) and large
//! `blend` radii — both common on organic shapes — work correctly here where
//! the vertex-fillet path breaks down.
//!
//! Every primitive returns a true SDF in its own local frame; `BlobChild`
//! holds the inverse-transform so callers can evaluate `child.sample(p)` with
//! `p` in the blob's frame. Scale factors compress the distance result by
//! the smallest scale axis — an approximate Lipschitz correction that's good
//! enough for marching surface-nets meshing (we care about where the
//! zero-isosurface sits, not the gradient magnitude away from it).

use glam::{Mat4, Vec3};

/// One implicit primitive inside a `blob`. Position/rotation/scale are baked
/// into `inv` so `sample()` can transform a world point into the primitive's
/// local frame in one matvec.
#[derive(Debug, Clone)]
pub struct BlobChild {
    pub prim: SdfPrim,
    pub op: SdfOp,
    /// Inverse of the child's local-to-blob transform.
    pub inv: Mat4,
    /// Smallest scale component of the forward transform — distances scale
    /// linearly with the local frame, so we multiply the local SDF by this to
    /// keep results conservative in blob space.
    pub scale_min: f32,
}

#[derive(Debug, Clone, Copy)]
pub enum SdfOp {
    Add,
    Subtract,
}

/// Implicit primitives supported as `blob` children. Every variant has a
/// closed-form SDF.
#[derive(Debug, Clone, Copy)]
pub enum SdfPrim {
    Sphere { radius: f32 },
    /// Axis-aligned ellipsoid with the given half-extents along x/y/z.
    /// Approximate SDF (the exact ellipsoid SDF needs Newton iteration); for
    /// surface-nets this approximation places the zero-iso correctly.
    Ellipsoid { half: Vec3 },
    /// Axis-aligned box with the given half-extents along x/y/z.
    Box { half: Vec3 },
    /// Box with rounded edges: half-extents minus the round radius, then
    /// shrunk-and-grown.
    RoundedBox { half: Vec3, radius: f32 },
    /// Capsule aligned along local Y axis. `height` is the centre-to-centre
    /// length between the two hemispherical end caps.
    Capsule { radius: f32, height: f32 },
    /// Closed cylinder aligned along local Y axis. `height` is the full
    /// extent (caps sit at ±height/2).
    Cylinder { radius: f32, height: f32 },
    /// Torus in the local XZ plane. `major` is ring radius, `minor` is tube
    /// radius.
    Torus { major: f32, minor: f32 },
}

impl SdfPrim {
    fn sample_local(&self, p: Vec3) -> f32 {
        match *self {
            SdfPrim::Sphere { radius } => p.length() - radius,
            SdfPrim::Ellipsoid { half } => {
                // Approximate ellipsoid SDF (Inigo Quilez): scales sphere by
                // axes. Not exact but the zero-iso matches the surface; the
                // gradient is slightly off, which only affects smin shape
                // near the surface — visually fine for organic blends.
                let r = (p / half).length();
                let r2 = (p / (half * half)).length();
                if r2 == 0.0 {
                    -half.min_element()
                } else {
                    r * (r - 1.0) / r2
                }
            }
            SdfPrim::Box { half } => {
                let q = p.abs() - half;
                q.max(Vec3::ZERO).length() + q.x.max(q.y.max(q.z)).min(0.0)
            }
            SdfPrim::RoundedBox { half, radius } => {
                let q = p.abs() - (half - Vec3::splat(radius));
                q.max(Vec3::ZERO).length() + q.x.max(q.y.max(q.z)).min(0.0) - radius
            }
            SdfPrim::Capsule { radius, height } => {
                let h = height.max(0.0) * 0.5;
                let y_clamped = p.y.clamp(-h, h);
                let d = Vec3::new(p.x, p.y - y_clamped, p.z);
                d.length() - radius
            }
            SdfPrim::Cylinder { radius, height } => {
                let h = height.max(0.0) * 0.5;
                let d = Vec3::new((p.x * p.x + p.z * p.z).sqrt() - radius, p.y.abs() - h, 0.0);
                let inside = d.x.max(d.y).min(0.0);
                let outside = Vec3::new(d.x.max(0.0), d.y.max(0.0), 0.0).length();
                inside + outside
            }
            SdfPrim::Torus { major, minor } => {
                let q = Vec3::new((p.x * p.x + p.z * p.z).sqrt() - major, p.y, 0.0);
                q.length() - minor
            }
        }
    }

    /// Tight AABB in the primitive's local frame. The `blob` AABB pads this
    /// by `3 * blend` so smooth-mins have room to bulge outward.
    pub fn local_aabb(&self) -> (Vec3, Vec3) {
        match *self {
            SdfPrim::Sphere { radius } => (Vec3::splat(-radius), Vec3::splat(radius)),
            SdfPrim::Ellipsoid { half } => (-half, half),
            SdfPrim::Box { half } => (-half, half),
            SdfPrim::RoundedBox { half, .. } => (-half, half),
            SdfPrim::Capsule { radius, height } => {
                let h = height.max(0.0) * 0.5 + radius;
                (Vec3::new(-radius, -h, -radius), Vec3::new(radius, h, radius))
            }
            SdfPrim::Cylinder { radius, height } => {
                let h = height.max(0.0) * 0.5;
                (Vec3::new(-radius, -h, -radius), Vec3::new(radius, h, radius))
            }
            SdfPrim::Torus { major, minor } => (
                Vec3::new(-(major + minor), -minor, -(major + minor)),
                Vec3::new(major + minor, minor, major + minor),
            ),
        }
    }
}

impl BlobChild {
    /// Build a `BlobChild` from a primitive and its local-to-blob transform.
    pub fn new(prim: SdfPrim, op: SdfOp, xform: Mat4) -> Self {
        let (s, _, _) = decompose_scale(&xform);
        let scale_min = s.x.min(s.y).min(s.z).max(1e-6);
        Self { prim, op, inv: xform.inverse(), scale_min }
    }

    /// Evaluate this primitive's SDF at point `p` in the blob's frame.
    pub fn sample(&self, p: Vec3) -> f32 {
        let local = self.inv.transform_point3(p);
        self.prim.sample_local(local) * self.scale_min
    }

    /// World-space AABB after applying the child's transform, computed from
    /// the 8 corners of the local AABB. Slightly loose but always valid.
    pub fn world_aabb(&self) -> (Vec3, Vec3) {
        let (lo, hi) = self.prim.local_aabb();
        let xform = self.inv.inverse();
        let corners = [
            Vec3::new(lo.x, lo.y, lo.z),
            Vec3::new(hi.x, lo.y, lo.z),
            Vec3::new(lo.x, hi.y, lo.z),
            Vec3::new(hi.x, hi.y, lo.z),
            Vec3::new(lo.x, lo.y, hi.z),
            Vec3::new(hi.x, lo.y, hi.z),
            Vec3::new(lo.x, hi.y, hi.z),
            Vec3::new(hi.x, hi.y, hi.z),
        ];
        let mut wmin = Vec3::splat(f32::INFINITY);
        let mut wmax = Vec3::splat(f32::NEG_INFINITY);
        for c in corners {
            let w = xform.transform_point3(c);
            wmin = wmin.min(w);
            wmax = wmax.max(w);
        }
        (wmin, wmax)
    }
}

/// Polynomial smooth-min (Inigo Quilez). `k > 0` is the blend radius in
/// scene units; the function reduces to `min(a, b)` as `k → 0`.
pub fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

/// Polynomial smooth-max derived from smin: `smax(a, b, k) = -smin(-a, -b, k)`.
/// Used by smooth subtraction: `smax(field, -d_carver, k)` carves a smooth
/// cavity instead of leaving a sharp rim.
pub fn smax(a: f32, b: f32, k: f32) -> f32 {
    -smin(-a, -b, k)
}

/// Combine an ordered list of `BlobChild`s into a single field value at `p`.
/// Add children blend via `smin`; subtract children carve via `smax`. Order
/// matches author intent: layer additions, then carve cavities.
pub fn evaluate_field(children: &[BlobChild], p: Vec3, k: f32) -> f32 {
    let mut field = f32::INFINITY;
    for c in children {
        let d = c.sample(p);
        match c.op {
            SdfOp::Add => field = smin(field, d, k),
            SdfOp::Subtract => field = smax(field, -d, k),
        }
    }
    field
}

/// AABB enclosing every child's transformed bounds, padded by `pad` on each
/// side. Returns `None` if `children` is empty.
pub fn blob_aabb(children: &[BlobChild], pad: f32) -> Option<(Vec3, Vec3)> {
    if children.is_empty() {
        return None;
    }
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for c in children {
        // Only additive children contribute to the bounds — a subtract
        // carves out of existing geometry, it can't grow the silhouette.
        if !matches!(c.op, SdfOp::Add) {
            continue;
        }
        let (clo, chi) = c.world_aabb();
        lo = lo.min(clo);
        hi = hi.max(chi);
    }
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    Some((lo - Vec3::splat(pad), hi + Vec3::splat(pad)))
}

fn decompose_scale(m: &Mat4) -> (Vec3, glam::Quat, Vec3) {
    let (s, r, t) = m.to_scale_rotation_translation();
    (s, r, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sphere_sdf_is_signed_distance() {
        let s = SdfPrim::Sphere { radius: 1.0 };
        assert!((s.sample_local(Vec3::ZERO) + 1.0).abs() < 1e-5);
        assert!((s.sample_local(Vec3::new(1.0, 0.0, 0.0))).abs() < 1e-5);
        assert!((s.sample_local(Vec3::new(2.0, 0.0, 0.0)) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn box_sdf_at_corner_is_zero() {
        let b = SdfPrim::Box { half: Vec3::new(1.0, 1.0, 1.0) };
        assert!(b.sample_local(Vec3::new(1.0, 1.0, 1.0)).abs() < 1e-5);
        assert!(b.sample_local(Vec3::ZERO) < 0.0);
        assert!(b.sample_local(Vec3::new(2.0, 0.0, 0.0)) > 0.0);
    }

    #[test]
    fn smin_reduces_to_min_at_zero_k() {
        assert!((smin(1.0, 2.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((smin(-3.0, 5.0, 0.0) + 3.0).abs() < 1e-6);
    }

    #[test]
    fn smin_blends_between_inputs() {
        let m = smin(0.05, 0.05, 0.2);
        assert!(m < 0.05, "expected smoothing to pull below sharp min, got {m}");
    }

    #[test]
    fn evaluate_field_two_spheres_blend() {
        let a = BlobChild::new(
            SdfPrim::Sphere { radius: 0.5 },
            SdfOp::Add,
            Mat4::from_translation(Vec3::new(-0.4, 0.0, 0.0)),
        );
        let b = BlobChild::new(
            SdfPrim::Sphere { radius: 0.5 },
            SdfOp::Add,
            Mat4::from_translation(Vec3::new(0.4, 0.0, 0.0)),
        );
        // Midpoint inside both spheres — field should be strongly negative.
        let mid = evaluate_field(&[a.clone(), b.clone()], Vec3::ZERO, 0.2);
        assert!(mid < 0.0, "midpoint should be inside the blob, got {mid}");
        // Far away — outside both.
        let far = evaluate_field(&[a, b], Vec3::new(10.0, 0.0, 0.0), 0.2);
        assert!(far > 0.0, "far point should be outside, got {far}");
    }

    #[test]
    fn subtract_carves_out_field() {
        let outer = BlobChild::new(
            SdfPrim::Sphere { radius: 1.0 },
            SdfOp::Add,
            Mat4::IDENTITY,
        );
        let hole = BlobChild::new(
            SdfPrim::Sphere { radius: 0.4 },
            SdfOp::Subtract,
            Mat4::from_translation(Vec3::new(0.7, 0.0, 0.0)),
        );
        // Centre is still inside the outer sphere.
        let c = evaluate_field(&[outer.clone(), hole.clone()], Vec3::ZERO, 0.0);
        assert!(c < 0.0, "centre should be inside, got {c}");
        // Point inside the carved cavity should now read as outside.
        let h = evaluate_field(&[outer, hole], Vec3::new(0.7, 0.0, 0.0), 0.0);
        assert!(h > 0.0, "cavity centre should read as outside, got {h}");
    }

    #[test]
    fn blob_aabb_ignores_subtracts() {
        let outer = BlobChild::new(
            SdfPrim::Sphere { radius: 1.0 },
            SdfOp::Add,
            Mat4::IDENTITY,
        );
        let hole = BlobChild::new(
            SdfPrim::Sphere { radius: 0.4 },
            SdfOp::Subtract,
            Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)),
        );
        let (lo, hi) = blob_aabb(&[outer, hole], 0.0).unwrap();
        // AABB should be ~[-1, 1] on each axis, not stretched to 10.
        assert!(hi.x < 2.0, "subtract should not grow AABB, got hi.x={}", hi.x);
        assert!(lo.x > -2.0);
    }
}
