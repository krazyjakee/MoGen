//! The solver's output: shapes with no opinion about how they get rendered.
//!
//! Two sinks consume this. One writes `.mog` source text (phase 1: the Pascal
//! importer produces an editable file). One builds `SceneGraph` meshes
//! (phase 2: the `building` generator emits directly). Anything either sink
//! would have to *compute* belongs on this side of the boundary — the sinks
//! translate, they do not solve.
//!
//! # Only two shapes
//!
//! [`Shape::Prism`] — a plan polygon extruded straight up — covers walls, wall
//! panels, slabs, ceilings, flat roofs and junction patches. [`Shape::Hull`]
//! covers every sloped roof. There is deliberately no third option and no
//! open-surface variant: a sink cannot be handed a shape it might leave
//! unclosed, because there is no such shape to hand it.
//!
//! Hulls rather than lofted profiles for roofs is a considered trade. A loft
//! degenerates when a section collapses to a point or a line — a hipped roof's
//! apex, a gambrel's ridge — and the failure mode is a capless mesh. A hull
//! over the same corner points is closed by construction. The cost is a
//! dependency on Manifold via `hull_mesh`, and the constraint that every roof
//! must be convex; the concave ones (Dutch, Mansard, Gambrel) are therefore
//! composed from several convex tiers rather than one hull.

use super::ir::{LevelId, MatRef, Polygon};
use super::plan;

/// A point in 3D world space: `[x, y, z]`, metres, +Y up.
pub(super) type P3 = [f32; 3];

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Shape {
    /// A plan polygon swept vertically. `base` and `top` are absolute world Y.
    Prism { poly: Polygon, base: f32, top: f32 },
    /// The convex hull of a point set. Must be genuinely three-dimensional —
    /// see [`ShapeError::Coplanar`].
    Hull { points: Vec<P3> },
}

/// What a solid *is*, for `role=` in the DSL and `node.extras` in the glTF.
///
/// Only the distinctions the IR can actually justify today. In particular
/// there is no exterior/interior wall split: neither the Pascal model nor
/// [`super::ir::Wall`] carries that flag, and inventing one here that no
/// producer sets would be a lie the sinks then propagate. Phase 2 adds it when
/// the `building` generator — which does know — becomes the second producer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Role {
    Wall,
    /// A patch over a corner whose mitre fell back to a butt joint.
    WallJoint,
    Slab,
    Ceiling,
    Roof,
    /// The triangular wall closing the end of a gabled roof.
    GableWall,
}

impl Role {
    /// The `role=` string. Matches the vocabulary the `building` generator
    /// already emits, so engine-side code keyed on these keeps working.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Wall => "wall",
            Role::WallJoint => "wall_joint",
            Role::Slab => "slab",
            Role::Ceiling => "ceiling",
            Role::Roof => "roof",
            Role::GableWall => "gable_wall",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Solid {
    /// Stable, derived from the source element — never a running counter, so
    /// adding one wall does not rename every wall after it.
    pub name: String,
    pub role: Role,
    pub level: LevelId,
    pub shape: Shape,
    pub material: Option<MatRef>,
}

/// A transform-only point of interest: a door slot, a furniture anchor, an
/// imported item. Carries no geometry — the engine populates it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Marker {
    pub name: String,
    /// Free-form, exported to `node.extras.role`.
    pub role: String,
    pub position: P3,
    /// Radians about +Y.
    pub rotation: f32,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ResolvedGeometry {
    pub solids: Vec<Solid>,
    pub markers: Vec<Marker>,
    /// Things a producer got away with but should hear about — an unknown node
    /// kind, a wall dropped as degenerate. Never fatal.
    pub warnings: Vec<String>,
}

/// Why a shape must not reach a sink.
///
/// Every one of these produces geometry that *looks* fine until it doesn't:
/// the triangulator drops caps without reporting, and Manifold's hull returns
/// an empty mesh for degenerate input. Catching them here is the difference
/// between a diagnostic and a hole someone finds in the engine three weeks
/// later.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum ShapeError {
    /// A prism with `top <= base`.
    ZeroHeight,
    /// Fewer than three points, or a ring that crosses itself. `extrude_mesh`
    /// swallows this and returns a capless tube.
    BadRing(&'static str),
    /// A hole that leaves the outer ring, so the "hole" is really a second
    /// disjoint shape.
    HoleOutsideOuter,
    /// Fewer than four points in a hull.
    HullTooSmall,
    /// Every hull point lies in one plane, so the hull has no volume.
    Coplanar,
}

impl Shape {
    /// Check a shape is a closed solid before any sink sees it.
    pub fn check(&self) -> Result<(), ShapeError> {
        match self {
            Shape::Prism { poly, base, top } => {
                if top <= base {
                    return Err(ShapeError::ZeroHeight);
                }
                if !plan::ring_is_simple(&poly.outer) {
                    return Err(ShapeError::BadRing("outer"));
                }
                for hole in &poly.holes {
                    if !plan::ring_is_simple(hole) {
                        return Err(ShapeError::BadRing("hole"));
                    }
                    if !hole.iter().all(|p| plan::point_in_ring(*p, &poly.outer)) {
                        return Err(ShapeError::HoleOutsideOuter);
                    }
                }
                Ok(())
            }
            Shape::Hull { points } => {
                if points.len() < 4 {
                    return Err(ShapeError::HullTooSmall);
                }
                if is_coplanar(points) {
                    return Err(ShapeError::Coplanar);
                }
                Ok(())
            }
        }
    }
}

/// Whether every point lies in a single plane.
///
/// Takes the first non-degenerate triangle as the reference plane and measures
/// everything else against it. The tolerance is absolute because the inputs are
/// metres of building: 0.1 mm out of plane is noise, and a roof thin enough for
/// that to be a real distinction is not a roof.
fn is_coplanar(points: &[P3]) -> bool {
    const FLAT: f32 = 1e-4;

    let a = points[0];
    // First point far enough from `a` to define a direction.
    let Some(b) = points.iter().find(|p| dist3(**p, a) > FLAT).copied() else {
        return true;
    };
    let u = sub3(b, a);

    // First point off that line gives the plane's normal.
    let mut normal = None;
    for p in points {
        let n = cross3(u, sub3(*p, a));
        if len3(n) > FLAT {
            normal = Some(scale3(n, 1.0 / len3(n)));
            break;
        }
    }
    let Some(n) = normal else { return true };

    points.iter().all(|p| dot3(n, sub3(*p, a)).abs() <= FLAT)
}

fn sub3(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale3(a: P3, k: f32) -> P3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn dot3(a: P3, b: P3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn len3(a: P3) -> f32 {
    dot3(a, a).sqrt()
}

fn dist3(a: P3, b: P3) -> f32 {
    len3(sub3(a, b))
}

impl ResolvedGeometry {
    /// Check every shape. Returns **all** the bad ones, because a producer that
    /// got one wall wrong has usually got a family of them wrong, and fixing
    /// them one error message at a time is miserable.
    pub fn check(&self) -> Vec<(String, ShapeError)> {
        self.solids
            .iter()
            .filter_map(|s| s.shape.check().err().map(|e| (s.name.clone(), e)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(size: f32) -> Vec<[f32; 2]> {
        vec![[0.0, 0.0], [size, 0.0], [size, size], [0.0, size]]
    }

    fn prism(poly: Polygon, base: f32, top: f32) -> Shape {
        Shape::Prism { poly, base, top }
    }

    #[test]
    fn a_plain_box_prism_is_accepted() {
        let s = prism(Polygon { outer: square(4.0), holes: vec![] }, 0.0, 2.5);
        assert_eq!(s.check(), Ok(()));
    }

    #[test]
    fn a_prism_with_no_height_is_rejected() {
        let poly = Polygon { outer: square(4.0), holes: vec![] };
        assert_eq!(prism(poly.clone(), 2.5, 2.5).check(), Err(ShapeError::ZeroHeight));
        assert_eq!(prism(poly, 2.5, 1.0).check(), Err(ShapeError::ZeroHeight));
    }

    #[test]
    fn a_self_crossing_outline_is_rejected_rather_than_silently_uncapped() {
        // The whole reason this check exists: `extrude_mesh` returns a mesh
        // with sides and no caps for this input, and reports nothing.
        let bowtie = vec![[0.0, 0.0], [1.0, 1.0], [1.0, 0.0], [0.0, 1.0]];
        let s = prism(Polygon { outer: bowtie, holes: vec![] }, 0.0, 1.0);
        assert_eq!(s.check(), Err(ShapeError::BadRing("outer")));
    }

    #[test]
    fn a_stairwell_hole_is_fine_but_an_escaped_one_is_not() {
        let inside = Polygon {
            outer: square(6.0),
            holes: vec![vec![[1.0, 1.0], [1.0, 3.0], [3.0, 3.0], [3.0, 1.0]]],
        };
        assert_eq!(prism(inside, 0.0, 0.2).check(), Ok(()));

        let escaped = Polygon {
            outer: square(6.0),
            holes: vec![vec![[7.0, 7.0], [7.0, 9.0], [9.0, 9.0], [9.0, 7.0]]],
        };
        assert_eq!(prism(escaped, 0.0, 0.2).check(), Err(ShapeError::HoleOutsideOuter));
    }

    #[test]
    fn a_tetrahedron_hull_is_accepted() {
        let s = Shape::Hull {
            points: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
        };
        assert_eq!(s.check(), Ok(()));
    }

    #[test]
    fn a_flat_hull_is_rejected() {
        // Manifold's hull returns an *empty mesh* for coplanar input rather
        // than erroring, so a roof whose points all landed on one plane would
        // simply not be there.
        let flat = Shape::Hull {
            points: vec![
                [0.0, 2.0, 0.0],
                [4.0, 2.0, 0.0],
                [4.0, 2.0, 3.0],
                [0.0, 2.0, 3.0],
                [2.0, 2.0, 1.5],
            ],
        };
        assert_eq!(flat.check(), Err(ShapeError::Coplanar));
    }

    #[test]
    fn a_hull_of_collinear_points_is_rejected() {
        let line = Shape::Hull {
            points: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [3.0, 0.0, 0.0]],
        };
        assert_eq!(line.check(), Err(ShapeError::Coplanar));
    }

    #[test]
    fn a_hull_needs_four_points() {
        let s = Shape::Hull { points: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] };
        assert_eq!(s.check(), Err(ShapeError::HullTooSmall));
    }

    #[test]
    fn a_roof_pitched_by_a_tenth_of_a_millimetre_counts_as_flat() {
        // The boundary of the coplanarity test, stated as a fact about
        // buildings: this is not a roof, it is a slab with a rounding error.
        let barely = Shape::Hull {
            points: vec![
                [0.0, 2.0, 0.0],
                [4.0, 2.0, 0.0],
                [4.0, 2.0, 3.0],
                [0.0, 2.0, 3.0],
                [2.0, 2.000_05, 1.5],
            ],
        };
        assert_eq!(barely.check(), Err(ShapeError::Coplanar));

        let mut real = barely.clone();
        if let Shape::Hull { points } = &mut real {
            points[4][1] = 3.0;
        }
        assert_eq!(real.check(), Ok(()));
    }

    #[test]
    fn every_bad_shape_is_reported_not_just_the_first() {
        let bad = |name: &str, shape: Shape| Solid {
            name: name.into(),
            role: Role::Wall,
            level: LevelId(0),
            shape,
            material: None,
        };
        let g = ResolvedGeometry {
            solids: vec![
                bad("ok", prism(Polygon { outer: square(1.0), holes: vec![] }, 0.0, 1.0)),
                bad("flat", prism(Polygon { outer: square(1.0), holes: vec![] }, 1.0, 1.0)),
                bad("thin", Shape::Hull { points: vec![[0.0; 3], [1.0, 0.0, 0.0]] }),
            ],
            ..Default::default()
        };
        assert_eq!(
            g.check(),
            vec![
                ("flat".into(), ShapeError::ZeroHeight),
                ("thin".into(), ShapeError::HullTooSmall),
            ]
        );
    }

    #[test]
    fn role_strings_match_the_generators_existing_vocabulary() {
        // Engine-side code keys on these, so they are an interface, not labels.
        assert_eq!(Role::Wall.as_str(), "wall");
        assert_eq!(Role::Roof.as_str(), "roof");
        assert_eq!(Role::GableWall.as_str(), "gable_wall");
    }
}
