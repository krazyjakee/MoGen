//! `ResolvedGeometry` → `Mesh`. The phase-2 sink.
//!
//! The text sink writes `.mog` source and lets the ordinary lowering path
//! build geometry. The `building` generator cannot do that — it is *inside*
//! that path — so it needs the solved shapes as meshes directly. This is that
//! translation, and nothing else: no solving, no fallbacks, no repair.
//!
//! # Why this does not call `extrude_mesh`
//!
//! [`mogen_geom::extrude_mesh`] returns `Mesh::default()` when its earcut
//! gives up, and returns a *capless tube* when the ring is self-intersecting.
//! Both are silent. A wall with no caps looks solid from outside and is a hole
//! from every other angle, and the first place anyone notices is the engine.
//!
//! So prisms are built here instead, by ear-clipping a ring that
//! [`Shape::check`] has already certified simple and then stitching the two
//! caps to the sides. That gives a closure argument rather than a hope: every
//! side edge is used once by its quad and once by the neighbouring quad, and
//! every cap edge is used once by the cap and once by a side. `closed_check`
//! asserts it in tests rather than trusting the argument.
//!
//! Hulls are the exception — those go to Manifold, because computing a convex
//! hull is exactly what it is for, and it returns a closed mesh or an empty
//! one, never a subtly broken one. An empty return is caught, not passed on.

use glam::{Mat4, Quat, Vec3};
use mogen_core::Mesh;

use super::super::ir::P2;
use super::super::plan;
use super::super::resolved::{Placement, Shape, ShapeError, Solid};

/// Why a shape could not become a mesh.
///
/// Distinct from [`ShapeError`], which is about the shape being wrong.
/// These are about this sink being unable to render a shape that was fine.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(in crate::lower::arch) enum MeshError {
    /// The shape failed its own validity check.
    Shape(ShapeError),
    /// Ear clipping stalled with polygon left over. For a ring that passed
    /// [`plan::ring_is_simple`] this should be unreachable; it is an error
    /// rather than a panic because "should be unreachable" has a poor record.
    Triangulation,
    /// Manifold returned nothing for a hull.
    EmptyHull,
    /// The build is missing the `csg` feature, so hulls are unavailable.
    NoCsg,
}

impl std::fmt::Display for MeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeshError::Shape(e) => write!(f, "invalid shape: {e:?}"),
            MeshError::Triangulation => write!(f, "could not triangulate the plan ring"),
            MeshError::EmptyHull => write!(f, "hull came back empty"),
            MeshError::NoCsg => write!(f, "hulls need the `csg` feature"),
        }
    }
}

/// One solid's mesh, in its own placement frame.
///
/// The placement stays *out* of the mesh deliberately. A roof segment carries
/// a rotation, and a caller that wants it as a node transform — which is how
/// the `building` generator emits — must not receive it pre-baked.
pub(in crate::lower::arch) fn solid_mesh(solid: &Solid) -> Result<Mesh, MeshError> {
    shape_mesh(&solid.shape)
}

pub(in crate::lower::arch) fn shape_mesh(shape: &Shape) -> Result<Mesh, MeshError> {
    shape.check().map_err(MeshError::Shape)?;
    match shape {
        Shape::Prism { poly, base, top } => prism(&poly.outer, &poly.holes, *base, *top),
        Shape::Hull { points } => hull(points),
    }
}

/// The matrix a caller applies to put a shape's mesh where its solid lives.
pub(in crate::lower::arch) fn placement_matrix(p: &Placement) -> Mat4 {
    Mat4::from_rotation_translation(
        Quat::from_rotation_y(p.rotation),
        Vec3::from(p.translation),
    )
}

// ---------------------------------------------------------------------------
// prisms
// ---------------------------------------------------------------------------

/// A plan polygon swept from `base` to `top`.
///
/// Winding: the outer ring arrives counter-clockwise in plan (`+x` right,
/// `+z` "up" on the page). Viewed from above — down `-y` — counter-clockwise
/// in that plane is *clockwise* on screen, so the top cap's outward normal
/// falls out of reversing the ring order, and the bottom cap keeps it. Getting
/// this backwards produces an inside-out solid that renders fine in a viewer
/// with backface culling off and fails every CSG operation afterwards.
pub(in crate::lower::arch) fn prism(
    outer: &[P2],
    holes: &[Vec<P2>],
    base: f32,
    top: f32,
) -> Result<Mesh, MeshError> {
    let mut rings: Vec<Vec<P2>> = Vec::with_capacity(1 + holes.len());
    let mut o = outer.to_vec();
    plan::normalise_ccw(&mut o);
    rings.push(o);
    for h in holes {
        let mut h = h.clone();
        plan::normalise_ccw(&mut h);
        // A hole winds against its outer ring, so the wall it presents faces
        // inward. Reversing here rather than asking the caller to means a
        // producer cannot get it wrong.
        h.reverse();
        rings.push(h);
    }

    let flat = bridge_holes(&rings);
    let tris = ear_clip(&flat).ok_or(MeshError::Triangulation)?;

    let mut m = Mesh::default();
    // Caps. Both are built from the same triangulation, so a hole in the top
    // is a hole in the bottom by construction.
    for (y, up) in [(base, false), (top, true)] {
        let first = m.positions.len() as u32;
        for p in &flat {
            m.positions.push([p[0], y, p[1]]);
            m.normals.push(if up { [0.0, 1.0, 0.0] } else { [0.0, -1.0, 0.0] });
            m.uvs.push([p[0], p[1]]);
        }
        for t in &tris {
            let (a, b, c) = (t[0] as u32, t[1] as u32, t[2] as u32);
            if up {
                m.indices.extend([first + a, first + c, first + b]);
            } else {
                m.indices.extend([first + a, first + b, first + c]);
            }
        }
    }

    // Sides, one quad per original ring edge — not per bridged edge, or the
    // bridge cuts would each get a pair of coincident zero-width walls.
    for ring in &rings {
        let n = ring.len();
        let mut u = 0.0f32;
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            let seg = plan::distance(a, b);
            let Some(d) = plan::normalise(plan::sub(b, a)) else { continue };
            // Outward for a CCW ring viewed with +z up the page.
            let nrm = [d[1], 0.0, -d[0]];
            let first = m.positions.len() as u32;
            for (p, y, uu, vv) in [
                (a, base, u, base),
                (b, base, u + seg, base),
                (b, top, u + seg, top),
                (a, top, u, top),
            ] {
                m.positions.push([p[0], y, p[1]]);
                m.normals.push(nrm);
                m.uvs.push([uu, vv]);
            }
            // Reversed relative to the obvious `0,1,2 / 0,2,3`. The ring is
            // counter-clockwise in the (x, z) *plan*, but a plan is drawn with
            // +z up the page while the world sees it from +y looking down, and
            // that view flips the handedness. Winding the quad the intuitive
            // way points every side face inward.
            m.indices.extend([first, first + 2, first + 1, first, first + 3, first + 2]);
            u += seg;
        }
    }

    Ok(m)
}

/// Cut each hole into the outer ring with a pair of coincident bridge edges,
/// giving one simple ring the ear clipper can chew.
///
/// The classic approach, and the reason the side walls are built from the
/// original rings rather than from this: a bridge edge is a real edge in the
/// cap triangulation and a fiction everywhere else.
fn bridge_holes(rings: &[Vec<P2>]) -> Vec<P2> {
    let mut outer = rings[0].clone();
    for hole in &rings[1..] {
        if hole.len() < 3 {
            continue;
        }
        // Join at the closest pair of vertices. Not the textbook
        // ray-cast-to-the-right rule, but the rings here are wall footprints
        // and slab outlines — convex-ish and well separated — and the closest
        // pair cannot cross another edge in that setting. `ring_is_simple` on
        // the result is the backstop.
        let mut best = (0usize, 0usize, f32::INFINITY);
        for (i, a) in outer.iter().enumerate() {
            for (j, b) in hole.iter().enumerate() {
                let d = plan::distance(*a, *b);
                if d < best.2 {
                    best = (i, j, d);
                }
            }
        }
        let (i, j, _) = best;
        let mut merged = Vec::with_capacity(outer.len() + hole.len() + 2);
        merged.extend_from_slice(&outer[..=i]);
        for k in 0..hole.len() {
            merged.push(hole[(j + k) % hole.len()]);
        }
        merged.push(hole[j]);
        merged.extend_from_slice(&outer[i..]);
        outer = merged;
    }
    outer
}

/// Ear clipping on a simple counter-clockwise ring.
///
/// O(n²), which is the right complexity here: the biggest ring this sees is a
/// slab outline with a handful of stair holes, and a faster algorithm would be
/// more code to get subtly wrong for no measurable gain.
fn ear_clip(ring: &[P2]) -> Option<Vec<[usize; 3]>> {
    let n = ring.len();
    if n < 3 {
        return None;
    }
    let mut idx: Vec<usize> = (0..n).collect();
    if plan::signed_area2(ring) < 0.0 {
        idx.reverse();
    }
    let mut tris = Vec::with_capacity(n.saturating_sub(2));
    let mut guard = 0;
    while idx.len() > 3 {
        guard += 1;
        if guard > n * n + 16 {
            return None;
        }
        let mut clipped = false;
        for k in 0..idx.len() {
            let (ia, ib, ic) = (
                idx[(k + idx.len() - 1) % idx.len()],
                idx[k],
                idx[(k + 1) % idx.len()],
            );
            let (a, b, c) = (ring[ia], ring[ib], ring[ic]);
            // Convex in a CCW ring means a positive cross product. A reflex or
            // collinear vertex is never an ear.
            if plan::perp_dot(plan::sub(b, a), plan::sub(c, b)) <= 0.0 {
                continue;
            }
            // Only reflex vertices can block an ear, and only if they are
            // *strictly* inside it. Both refinements matter here rather than
            // being micro-optimisations: bridging a hole leaves two pairs of
            // coincident vertices sitting exactly on the bridge edges, and an
            // inclusive test reads those as blockers, so every candidate ear
            // is rejected and the clipper stalls on its first hole.
            let blocked = idx.iter().enumerate().any(|(k2, &i)| {
                if i == ia || i == ib || i == ic {
                    return false;
                }
                let prev = ring[idx[(k2 + idx.len() - 1) % idx.len()]];
                let next = ring[idx[(k2 + 1) % idx.len()]];
                let reflex = plan::perp_dot(
                    plan::sub(ring[i], prev),
                    plan::sub(next, ring[i]),
                ) <= 0.0;
                reflex && strictly_inside(ring[i], a, b, c)
            });
            if blocked {
                continue;
            }
            tris.push([ia, ib, ic]);
            idx.remove(k);
            clipped = true;
            break;
        }
        if !clipped {
            return None;
        }
    }
    tris.push([idx[0], idx[1], idx[2]]);
    Some(tris)
}

/// Inside with no touching. A point on an edge does not block an ear — see
/// the bridge-vertex note in [`ear_clip`].
fn strictly_inside(p: P2, a: P2, b: P2, c: P2) -> bool {
    let d0 = plan::perp_dot(plan::sub(b, a), plan::sub(p, a));
    let d1 = plan::perp_dot(plan::sub(c, b), plan::sub(p, b));
    let d2 = plan::perp_dot(plan::sub(a, c), plan::sub(p, c));
    (d0 > 0.0 && d1 > 0.0 && d2 > 0.0) || (d0 < 0.0 && d1 < 0.0 && d2 < 0.0)
}

// ---------------------------------------------------------------------------
// hulls
// ---------------------------------------------------------------------------

#[cfg(feature = "csg")]
fn hull(points: &[[f32; 3]]) -> Result<Mesh, MeshError> {
    let m = mogen_geom::hull_mesh(points);
    if m.indices.is_empty() {
        return Err(MeshError::EmptyHull);
    }
    Ok(m)
}

#[cfg(not(feature = "csg"))]
fn hull(_points: &[[f32; 3]]) -> Result<Mesh, MeshError> {
    Err(MeshError::NoCsg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogen_geom::is_closed_manifold;

    fn square(s: f32) -> Vec<P2> {
        vec![[-s, -s], [s, -s], [s, s], [-s, s]]
    }

    /// Every edge used exactly twice, once in each direction. Stricter than
    /// [`is_closed_manifold`] on unwelded input, so run it on welded meshes.
    fn closed(mesh: &Mesh) -> bool {
        is_closed_manifold(&mogen_geom::weld_vertices(mesh, 1e-5))
    }

    fn volume(mesh: &Mesh) -> f32 {
        mesh.indices
            .chunks_exact(3)
            .map(|t| {
                let p = |k: usize| Vec3::from(mesh.positions[t[k] as usize]);
                p(0).dot(p(1).cross(p(2))) / 6.0
            })
            .sum()
    }

    #[test]
    fn a_box_is_closed_and_holds_its_volume() {
        let m = prism(&square(1.0), &[], 0.0, 3.0).unwrap();
        assert!(closed(&m));
        assert!((volume(&m) - 12.0).abs() < 1e-4, "{}", volume(&m));
    }

    #[test]
    fn the_outward_normal_points_out() {
        // A positive signed volume is the whole of the winding contract: get
        // it wrong and every CSG operation downstream sees an inverted solid.
        let m = prism(&square(1.0), &[], 0.0, 3.0).unwrap();
        assert!(volume(&m) > 0.0, "solid is inside out");
    }

    #[test]
    fn a_clockwise_ring_is_corrected_rather_than_trusted() {
        let mut cw = square(1.0);
        cw.reverse();
        let m = prism(&cw, &[], 0.0, 3.0).unwrap();
        assert!(closed(&m));
        assert!((volume(&m) - 12.0).abs() < 1e-4);
    }

    #[test]
    fn a_hole_removes_volume_and_stays_closed() {
        let m = prism(&square(2.0), &[square(1.0)], 0.0, 2.0).unwrap();
        assert!(closed(&m), "bridged ring left the solid open");
        // 4×4 minus 2×2, two metres tall.
        assert!((volume(&m) - (16.0 - 4.0) * 2.0).abs() < 1e-3, "{}", volume(&m));
    }

    #[test]
    fn an_l_shaped_ring_triangulates() {
        // Concave, so the fan a convex-only builder would use is wrong here.
        let l = vec![
            [0.0, 0.0],
            [3.0, 0.0],
            [3.0, 1.0],
            [1.0, 1.0],
            [1.0, 3.0],
            [0.0, 3.0],
        ];
        let m = prism(&l, &[], 0.0, 1.0).unwrap();
        assert!(closed(&m));
        assert!((volume(&m) - 5.0).abs() < 1e-4, "{}", volume(&m));
    }

    #[test]
    fn a_self_intersecting_ring_is_refused_not_rendered() {
        // The failure the whole module exists to avoid: `extrude_mesh` would
        // return a capless tube here and say nothing.
        let bowtie = vec![[0.0, 0.0], [2.0, 2.0], [2.0, 0.0], [0.0, 2.0]];
        let shape = Shape::Prism {
            poly: super::super::super::ir::Polygon { outer: bowtie, holes: Vec::new() },
            base: 0.0,
            top: 1.0,
        };
        assert!(matches!(
            shape_mesh(&shape),
            Err(MeshError::Shape(ShapeError::BadRing(_)))
        ));
    }

    #[test]
    fn a_zero_height_prism_is_refused() {
        let shape = Shape::Prism {
            poly: super::super::super::ir::Polygon { outer: square(1.0), holes: Vec::new() },
            base: 2.0,
            top: 2.0,
        };
        assert!(matches!(
            shape_mesh(&shape),
            Err(MeshError::Shape(ShapeError::ZeroHeight))
        ));
    }

    #[test]
    fn a_thin_wall_panel_survives_being_thin() {
        // 100 mm thick, 2.4 m tall, 40 mm wide — the sliver a pier beside a
        // narrow window turns into. Nothing here should collapse it.
        let panel = vec![[0.0, 0.0], [0.04, 0.0], [0.04, 0.1], [0.0, 0.1]];
        let m = prism(&panel, &[], 0.0, 2.4).unwrap();
        assert!(closed(&m));
        assert!((volume(&m) - 0.04 * 0.1 * 2.4).abs() < 1e-6, "{}", volume(&m));
    }

    #[test]
    fn every_vertex_carries_a_normal_and_a_uv() {
        // The exporter reads these three arrays in lockstep; a length mismatch
        // is a corrupt GLB rather than a visible bug.
        let m = prism(&square(2.0), &[square(1.0)], 0.0, 2.0).unwrap();
        assert_eq!(m.positions.len(), m.normals.len());
        assert_eq!(m.positions.len(), m.uvs.len());
    }
}
