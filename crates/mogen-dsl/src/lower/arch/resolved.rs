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

use super::ir::{LevelId, MatRef, Marker, OpeningKind, Polygon};
pub use super::ir::P3;
use super::plan;

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

/// Where a solid's shape sits. A shape's coordinates are expressed **in its
/// solid's placement frame**, so [`Placement::IDENTITY`] means world space.
///
/// Walls and slabs are already solved in world plan coordinates and use the
/// identity. Roofs do not: a roof segment carries a rotation, and baking that
/// into every hull point would mean nudging a roof's angle rewrites every
/// number in the emitted source. Building the shape in a local frame keeps the
/// rotation editable as a single attribute.
///
/// Only Y rotation, because that is the only rotation a building has. A roof
/// tilted about X is a different roof.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Placement {
    pub translation: P3,
    /// Radians about +Y.
    pub rotation: f32,
}

impl Placement {
    pub const IDENTITY: Placement = Placement { translation: [0.0; 3], rotation: 0.0 };

    pub fn is_identity(&self) -> bool {
        self.translation == [0.0; 3] && self.rotation == 0.0
    }
}

impl Default for Placement {
    fn default() -> Self {
        Self::IDENTITY
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
    pub placement: Placement,
    pub material: Option<MatRef>,
}

/// A door or window: placed by the solver, shaped by the sink.
///
/// Cutting the hole is geometry and lives here; deciding what a door *looks*
/// like is not. The text sink instantiates the stdlib `door_simple` /
/// `window_simple` modules so the imported file stays editable — swap the
/// module name and every door in the building changes — and a mesh sink is
/// free to build its own leaf. Either way both get the same answer to the only
/// question that needs solving: where is the doorway, and which way does it
/// face?
///
/// Without this, an opening is only ever an absence. That reads as a building
/// with no windows rather than a building with glass in them, and it throws
/// away the [`OpeningKind`] the producer took the trouble to supply.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpeningInstance {
    /// Derived from the wall and the opening's index within it, so adding a
    /// window to one wall does not rename the doors in another.
    pub name: String,
    pub kind: OpeningKind,
    pub level: LevelId,
    /// The centre of the threshold: mid-width, mid-thickness, at the sill —
    /// which is where both stdlib modules are anchored.
    pub position: P3,
    /// Radians about +Y, turning local +Z onto the wall's normal — square
    /// across the wall, so the leaf sits flush in its own reveal.
    ///
    /// *Which* normal is not knowable here: nothing in the IR says which side
    /// of a wall is outside, and inferring it from ring winding would be a
    /// guess that silently reverses for a producer that winds the other way.
    /// The stdlib door and window are near enough symmetric front-to-back that
    /// picking one side consistently reads correctly either way.
    pub rotation: f32,
    /// The **clipped** extents: what the cut actually left, not what the
    /// producer asked for.
    pub width: f32,
    pub height: f32,
}

/// A material the solids refer to by name.
///
/// The solver never invents these — it only carries [`MatRef`] names — so they
/// come from whichever producer built the model. Kept minimal on purpose: a
/// field here is a field every producer has to have an answer for, and the
/// place to add richness is the DSL's own `material`, not this.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaterialDecl {
    pub name: String,
    /// Linear RGB.
    pub color: Option<[f32; 3]>,
    pub metallic: Option<f32>,
    pub roughness: Option<f32>,
    /// Path to an albedo image, relative to the emitted file.
    pub texture: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ResolvedGeometry {
    pub solids: Vec<Solid>,
    /// Doors and windows, in the holes the panels left for them.
    pub openings: Vec<OpeningInstance>,
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
pub(crate) enum ShapeError {
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
            placement: Placement::IDENTITY,
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
