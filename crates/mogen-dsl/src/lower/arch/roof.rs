//! Roofs: seven named shapes, one construction.
//!
//! Hip, gable, shed, gambrel, dutch, mansard and flat all reduce to a **stack
//! of tiers**, where a tier is the convex hull of a lower rectangle and an
//! upper one at a greater height. The upper rectangle is allowed to be
//! degenerate: collapse one of its extents and it is a ridge line, collapse
//! both and it is an apex point.
//!
//! That single primitive covers every case:
//!
//! | Type    | Tiers | Upper of the top tier      |
//! |---------|-------|----------------------------|
//! | Flat    | 1     | the same rectangle (a slab)|
//! | Shed    | 1     | one edge raised            |
//! | Gable   | 1     | a full-length ridge        |
//! | Hip     | 1     | an inset ridge, or an apex |
//! | Gambrel | 2     | ridge, over a shallow break|
//! | Mansard | 2     | ridge, over a steep skirt  |
//! | Dutch   | 2     | ridge, over a hip          |
//!
//! Gambrel, mansard and dutch need two tiers precisely because their profiles
//! are **concave** — that is what a gambrel's kink *is* — and a hull cannot be
//! concave. Splitting at the break gives two convex pieces whose union is the
//! roof. Getting this wrong is not subtle: a single hull over a gambrel's
//! points quietly returns the shape with the kink smoothed away, which is a
//! plain gable.
//!
//! # Why hulls
//!
//! A loft through the same sections degenerates whenever a section collapses —
//! a hipped roof's ridge, a pyramid's apex — and `extrude`/loft failures are
//! silent, producing a mesh with no caps. A hull over the corner points is
//! closed by construction. The cost is Manifold, and the requirement that each
//! tier be convex.
//!
//! # Frames
//!
//! Everything here is built in the segment's **local** frame: origin at the
//! segment centre, X across the width, Z across the depth, Y up from the
//! eaves. The segment's world position and rotation ride on
//! [`resolved::Placement`] so an author can turn a roof without every
//! coordinate in the file changing.

use super::consts::{CONNECTIVITY_SLOP, MIN_PANEL};
use super::curve;
use super::ir::{ArchModel, RoofSegment, RoofType};
use super::junction;
use super::resolved::{Placement, Role, Shape, Solid, P3};
use super::height;

/// One segment's worth of roof, plus what had to be corrected to place it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RoofSolids {
    pub solids: Vec<Solid>,
    /// How far the eave was lifted to clear the walls under it, if at all.
    /// The caller turns this into a warning; the geometry is already correct.
    pub raised: Option<f32>,
}

/// A rectangle centred on the local origin, given as half-extents. A zero
/// half-extent is a legitimate degenerate case: a ridge, or an apex.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    hw: f32,
    hd: f32,
}

impl Rect {
    /// The four corners at height `y`, in a fixed order. Degenerate rectangles
    /// repeat points, which a hull absorbs without complaint.
    fn corners(&self, y: f32) -> [P3; 4] {
        [
            [-self.hw, y, -self.hd],
            [self.hw, y, -self.hd],
            [self.hw, y, self.hd],
            [-self.hw, y, self.hd],
        ]
    }

    fn inset(&self, dx: f32, dz: f32) -> Rect {
        Rect { hw: (self.hw - dx).max(0.0), hd: (self.hd - dz).max(0.0) }
    }
}

/// One convex slice of a roof.
#[derive(Clone, Copy, Debug)]
struct Tier {
    lower: Rect,
    lower_y: f32,
    upper: Rect,
    upper_y: f32,
}

impl Tier {
    fn hull(&self) -> Shape {
        let mut points = Vec::with_capacity(8);
        points.extend(self.lower.corners(self.lower_y));
        points.extend(self.upper.corners(self.upper_y));
        Shape::Hull { points }
    }
}

/// Build the solids for one roof segment.
///
/// Returns an empty vector for a degenerate segment rather than emitting a
/// shape that will fail downstream — the caller reports it as a warning. Every
/// shape returned has already passed [`Shape::check`].
pub(super) fn solids(model: &ArchModel, seg: &RoofSegment) -> RoofSolids {
    let Ok(plane) = height::level_plane_y(model, seg.level).ok_or(()) else {
        return RoofSolids { solids: Vec::new(), raised: None };
    };

    // Where the producer asked for the eave, and where the walls will actually
    // let it sit. Only ever raised: a roof floating above its walls is a knee
    // wall someone meant, but a roof *below* them is a row of walls poking out
    // through the slopes, and nothing downstream can tell that from a design.
    let wanted = plane + seg.wall_height;
    let eave = match supported_top(model, seg) {
        Some(top) if top > wanted => top,
        _ => wanted,
    };
    let raised = (eave > wanted).then_some(eave - wanted);

    // The eave footprint is the segment grown by its overhang on every side.
    let hw = 0.5 * seg.width + seg.overhang;
    let hd = 0.5 * seg.depth + seg.overhang;
    if hw <= MIN_PANEL || hd <= MIN_PANEL {
        return RoofSolids { solids: Vec::new(), raised: None };
    }
    let base = Rect { hw, hd };

    // The pitch applies across the SHORT span — that is what makes a long
    // narrow roof read as a roof rather than a wedge, and it is why the ridge
    // runs along the long axis without anyone having to say so.
    let half_run = hw.min(hd);
    let rise = half_run * seg.pitch_deg.clamp(0.0, 85.0).to_radians().tan();

    let tiers = match seg.roof_type {
        RoofType::Flat => vec![Tier {
            lower: base,
            lower_y: -seg.params.deck_thickness,
            upper: base,
            upper_y: 0.0,
        }],
        RoofType::Shed => {
            return RoofSolids { solids: shed(seg, base, rise, eave), raised }
        }
        RoofType::Gable => vec![Tier { lower: base, lower_y: 0.0, upper: ridge(base, 0.0), upper_y: rise }],
        RoofType::Hip => {
            vec![Tier { lower: base, lower_y: 0.0, upper: ridge(base, half_run), upper_y: rise }]
        }
        RoofType::Gambrel => two_tier(
            base,
            rise,
            half_run,
            seg.params.gambrel_lower_width,
            seg.params.gambrel_lower_height,
        ),
        RoofType::Mansard => two_tier(
            base,
            rise,
            half_run,
            seg.params.mansard_steep_width,
            seg.params.mansard_steep_height,
        ),
        RoofType::Dutch => dutch(base, rise, half_run, seg),
    };

    RoofSolids { solids: finish(seg, tiers, eave), raised }
}

/// The highest top among the walls this segment covers, or `None` if it
/// covers none.
///
/// "Covers" is the whole of a wall's centreline lying inside the eave
/// rectangle, and the *whole* is what makes it discriminating. A roof drawn to
/// its building's footprint has every perimeter centreline running along its
/// edge, so it holds them all up. A porch roof tucked against a two-storey
/// wall does not: that wall runs on past the porch, so the porch keeps the
/// height it was given instead of jumping to the house's eaves.
///
/// Testing the centreline rather than the footprint is deliberate. A wall's
/// faces sit half a thickness either side of it, so a roof sized exactly to
/// its walls — the ordinary case — never quite contains them, and a footprint
/// test would hold up nothing at all.
fn supported_top(model: &ArchModel, seg: &RoofSegment) -> Option<f32> {
    let (sin, cos) = seg.rotation.sin_cos();
    let hw = 0.5 * seg.width + seg.overhang + CONNECTIVITY_SLOP;
    let hd = 0.5 * seg.depth + seg.overhang + CONNECTIVITY_SLOP;

    model
        .walls
        .iter()
        .filter(|w| {
            let (s, e) = junction::ends(w);
            curve::sample_centreline(s, e, w.curve_offset).iter().all(|p| {
                // Undo the segment's placement: `ry` maps local (x, z) to
                // world (x·cos + z·sin, −x·sin + z·cos), so this is its
                // inverse.
                let (dx, dz) = (p[0] - seg.centre[0], p[1] - seg.centre[1]);
                let (lx, lz) = (dx * cos - dz * sin, dx * sin + dz * cos);
                lx.abs() <= hw && lz.abs() <= hd
            })
        })
        .filter_map(|w| height::wall_span(model, w).ok())
        .map(|span| span.top)
        .reduce(f32::max)
}

/// The ridge of a hipped or gabled roof: the base rectangle with its **short**
/// axis collapsed, and its long axis pulled in by `inset`.
///
/// `inset = 0` leaves the ridge running the full length — a gable. `inset =
/// half_run` pulls each end in by the same distance the slope travels, so all
/// four faces sit at the same pitch — a hip. On a square footprint that pulls
/// the ridge to a single point, and the hip becomes a pyramid, which is
/// correct and needs no special case.
fn ridge(base: Rect, inset: f32) -> Rect {
    if base.hw >= base.hd {
        Rect { hw: (base.hw - inset).max(0.0), hd: 0.0 }
    } else {
        Rect { hw: 0.0, hd: (base.hd - inset).max(0.0) }
    }
}

/// Gambrel and mansard: a break partway up, then a shallower run to the ridge.
///
/// `width_ratio` is where the break sits across the half-run and
/// `height_ratio` how much of the rise the lower tier takes. A gambrel breaks
/// halfway across and takes most of the height (steep bottom, shallow top); a
/// mansard breaks close to the eaves and takes even more (a near-vertical
/// skirt under a shallow cap).
fn two_tier(base: Rect, rise: f32, half_run: f32, width_ratio: f32, height_ratio: f32) -> Vec<Tier> {
    let wr = width_ratio.clamp(0.05, 0.95);
    let hr = height_ratio.clamp(0.05, 0.95);
    let break_inset = half_run * wr;
    let break_y = rise * hr;
    let break_rect = base.inset(break_inset, break_inset);

    vec![
        Tier { lower: base, lower_y: 0.0, upper: break_rect, upper_y: break_y },
        Tier {
            lower: break_rect,
            lower_y: break_y,
            upper: ridge(break_rect, 0.0),
            upper_y: rise,
        },
    ]
}

/// Dutch: a hip that stops partway up, with a small gable (the gablet) above.
///
/// Structurally the same two-tier stack as a gambrel, but the lower tier insets
/// on *both* axes (it is a hip) while the upper runs out to a full ridge (it is
/// a gable). That combination is the whole point of the shape.
fn dutch(base: Rect, rise: f32, half_run: f32, seg: &RoofSegment) -> Vec<Tier> {
    let wr = seg.params.dutch_hip_width.clamp(0.05, 0.95);
    let hr = seg.params.dutch_hip_height.clamp(0.05, 0.95);
    let hip_y = rise * hr;
    let hip_rect = base.inset(half_run * wr, half_run * wr);

    vec![
        Tier { lower: base, lower_y: 0.0, upper: hip_rect, upper_y: hip_y },
        Tier { lower: hip_rect, lower_y: hip_y, upper: ridge(hip_rect, 0.0), upper_y: rise },
    ]
}

/// Shed: one plane, low on −Z and high on +Z.
///
/// Not expressible as a `Tier`, whose upper rectangle is always centred. Built
/// directly as the hull of the eave rectangle and the two raised corners along
/// +Z.
fn shed(seg: &RoofSegment, base: Rect, rise: f32, eave: f32) -> Vec<Solid> {
    let points = vec![
        [-base.hw, 0.0, -base.hd],
        [base.hw, 0.0, -base.hd],
        [base.hw, 0.0, base.hd],
        [-base.hw, 0.0, base.hd],
        // The high edge, and the underside beneath it, so the solid has depth
        // rather than being a single sloped sheet.
        [-base.hw, rise, base.hd],
        [base.hw, rise, base.hd],
        [-base.hw, -seg.params.deck_thickness, -base.hd],
        [base.hw, -seg.params.deck_thickness, -base.hd],
    ];
    finish_shapes(seg, vec![Shape::Hull { points }], eave)
}

fn finish(seg: &RoofSegment, tiers: Vec<Tier>, eave: f32) -> Vec<Solid> {
    finish_shapes(seg, tiers.iter().map(Tier::hull).collect(), eave)
}

/// Wrap shapes as solids, dropping any that fail their own closure check.
///
/// A tier can legitimately come out degenerate — a zero-pitch hip has no rise,
/// so its hull is flat — and a flat hull is an empty mesh, not an error, which
/// is exactly the silent hole this whole layer exists to prevent. Dropping it
/// here means the roof loses a tier it could never have rendered anyway.
fn finish_shapes(seg: &RoofSegment, shapes: Vec<Shape>, eave: f32) -> Vec<Solid> {
    let placement = Placement {
        translation: [seg.centre[0], eave, seg.centre[1]],
        rotation: seg.rotation,
    };
    let multi = shapes.len() > 1;

    shapes
        .into_iter()
        .enumerate()
        .filter(|(_, s)| s.check().is_ok())
        .map(|(i, shape)| Solid {
            name: if multi {
                format!("roof_{}_tier{}", seg.id.0, i)
            } else {
                format!("roof_{}", seg.id.0)
            },
            role: Role::Roof,
            level: seg.level,
            shape,
            placement,
            material: seg.material.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::arch::ir::{
        Level, LevelId, ModelSource, RoofId, RoofParams, Wall, WallId,
    };

    const EPS: f32 = 1e-4;

    /// Most of these tests only care about the shapes, so they read the
    /// shapes. The eave-raising cases below call `super::solids` directly.
    fn solids(m: &ArchModel, seg: &RoofSegment) -> Vec<Solid> {
        super::solids(m, seg).solids
    }

    fn model() -> ArchModel {
        let mut m = ArchModel::new(ModelSource::PascalEditor);
        m.levels.push(Level { id: LevelId(0), name: None, height: 2.7 });
        m
    }

    fn segment(roof_type: RoofType, width: f32, depth: f32) -> RoofSegment {
        RoofSegment {
            id: RoofId(0),
            level: LevelId(0),
            centre: [0.0, 0.0],
            width,
            depth,
            rotation: 0.0,
            pitch_deg: 40.0,
            roof_type,
            overhang: 0.0,
            wall_height: 2.5,
            params: RoofParams::default(),
            material: None,
        }
    }

    fn points(s: &Solid) -> Vec<P3> {
        match &s.shape {
            Shape::Hull { points } => points.clone(),
            other => panic!("expected a hull, got {other:?}"),
        }
    }

    fn peak(s: &Solid) -> f32 {
        points(s).iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max)
    }

    fn every_shape_is_closed(solids: &[Solid]) {
        for s in solids {
            assert_eq!(s.shape.check(), Ok(()), "{} is not a closed solid", s.name);
        }
    }

    #[test]
    fn every_roof_type_produces_closed_solids() {
        // The blanket guarantee. A roof that reaches a sink unclosed is a hole
        // in the building, and Manifold reports degenerate hulls as an empty
        // mesh rather than an error.
        let m = model();
        for rt in [
            RoofType::Flat,
            RoofType::Shed,
            RoofType::Gable,
            RoofType::Hip,
            RoofType::Gambrel,
            RoofType::Mansard,
            RoofType::Dutch,
        ] {
            for (w, d) in [(8.0, 5.0), (5.0, 8.0), (6.0, 6.0)] {
                let out = solids(&m, &segment(rt, w, d));
                assert!(!out.is_empty(), "{rt:?} at {w}x{d} produced nothing");
                every_shape_is_closed(&out);
            }
        }
    }

    #[test]
    fn a_flat_roof_is_a_prism_of_its_deck_thickness() {
        let m = model();
        let out = solids(&m, &segment(RoofType::Flat, 8.0, 5.0));
        assert_eq!(out.len(), 1);
        let ys: Vec<f32> = points(&out[0]).iter().map(|p| p[1]).collect();
        let (lo, hi) = (
            ys.iter().copied().fold(f32::INFINITY, f32::min),
            ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        );
        assert!((hi - lo - RoofParams::default().deck_thickness).abs() < EPS);
    }

    #[test]
    fn a_gable_ridge_runs_the_full_length_of_the_long_axis() {
        let m = model();
        let out = solids(&m, &segment(RoofType::Gable, 8.0, 5.0));
        let top: Vec<P3> = points(&out[0]).into_iter().filter(|p| p[1] > EPS).collect();
        // The ridge spans the full 8 m width and has no depth.
        assert!(top.iter().all(|p| p[2].abs() < EPS), "ridge must be a line: {top:?}");
        let x_span = top.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
        assert!((x_span - 4.0).abs() < EPS, "got {x_span}");
    }

    #[test]
    fn a_hip_ridge_is_inset_by_the_slope_run() {
        let m = model();
        let out = solids(&m, &segment(RoofType::Hip, 8.0, 5.0));
        let top: Vec<P3> = points(&out[0]).into_iter().filter(|p| p[1] > EPS).collect();
        // Half-run is 2.5 (the short axis), so the 4 m half-width pulls in to
        // 1.5 — that inset is what makes all four faces share one pitch.
        let x_span = top.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
        assert!((x_span - 1.5).abs() < EPS, "got {x_span}");
    }

    #[test]
    fn a_hip_on_a_square_plan_becomes_a_pyramid() {
        let m = model();
        let out = solids(&m, &segment(RoofType::Hip, 6.0, 6.0));
        every_shape_is_closed(&out);
        let top: Vec<P3> = points(&out[0]).into_iter().filter(|p| p[1] > EPS).collect();
        assert!(
            top.iter().all(|p| p[0].abs() < EPS && p[2].abs() < EPS),
            "the ridge should collapse to an apex: {top:?}"
        );
    }

    #[test]
    fn the_ridge_follows_the_long_axis_whichever_it_is() {
        let m = model();
        let wide = solids(&m, &segment(RoofType::Gable, 8.0, 5.0));
        let deep = solids(&m, &segment(RoofType::Gable, 5.0, 8.0));

        let ridge_of = |s: &Solid| -> (f32, f32) {
            let top: Vec<P3> = points(s).into_iter().filter(|p| p[1] > EPS).collect();
            (
                top.iter().map(|p| p[0].abs()).fold(0.0, f32::max),
                top.iter().map(|p| p[2].abs()).fold(0.0, f32::max),
            )
        };
        assert_eq!(ridge_of(&wide[0]), (4.0, 0.0));
        assert_eq!(ridge_of(&deep[0]), (0.0, 4.0));
    }

    #[test]
    fn the_concave_roofs_are_split_into_two_convex_tiers() {
        // A single hull over a gambrel's points smooths the kink away and
        // leaves a plain gable -- valid geometry, wrong roof.
        let m = model();
        for rt in [RoofType::Gambrel, RoofType::Mansard, RoofType::Dutch] {
            let out = solids(&m, &segment(rt, 8.0, 5.0));
            assert_eq!(out.len(), 2, "{rt:?} needs two tiers, got {}", out.len());
            every_shape_is_closed(&out);
        }
    }

    #[test]
    fn a_gambrel_tiers_meet_without_a_gap() {
        // The upper tier's base must be exactly the lower tier's top, or the
        // roof has a slot around it at the break line.
        let m = model();
        let out = solids(&m, &segment(RoofType::Gambrel, 8.0, 5.0));
        let lower_top = peak(&out[0]);
        let upper_base = points(&out[1]).iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        assert!((lower_top - upper_base).abs() < EPS, "{lower_top} vs {upper_base}");
    }

    #[test]
    fn a_mansard_skirt_is_steeper_than_a_gambrel_break() {
        // The distinction the shared RoofParams used to erase: a mansard's
        // break sits close to the eaves and takes most of the rise, so its
        // lower face is nearly vertical.
        let m = model();
        let gambrel = solids(&m, &segment(RoofType::Gambrel, 8.0, 5.0));
        let mansard = solids(&m, &segment(RoofType::Mansard, 8.0, 5.0));
        assert!(
            peak(&mansard[0]) > peak(&gambrel[0]),
            "mansard skirt {} should out-rise gambrel break {}",
            peak(&mansard[0]),
            peak(&gambrel[0])
        );
    }

    #[test]
    fn a_dutch_gablet_sits_on_a_hip() {
        // Lower tier hips in on both axes; upper runs out to a full ridge.
        let m = model();
        let out = solids(&m, &segment(RoofType::Dutch, 8.0, 5.0));
        let hip_top: Vec<P3> =
            points(&out[0]).into_iter().filter(|p| p[1] > EPS).collect();
        assert!(
            hip_top.iter().all(|p| p[0].abs() < 4.0 - EPS && p[2].abs() < 2.5 - EPS),
            "the hip must inset on both axes: {hip_top:?}"
        );
        // The gablet's own ridge — its topmost points, not everything at or
        // above the break, which would sweep in the tier's base as well.
        let gablet = points(&out[1]);
        let top = peak(&out[1]);
        let ridge: Vec<P3> = gablet.into_iter().filter(|p| p[1] > top - EPS).collect();
        assert!(
            ridge.iter().all(|p| p[2].abs() < EPS),
            "the gablet ridge must be a line, got {ridge:?}"
        );
        assert!(
            ridge.iter().any(|p| p[0].abs() > EPS),
            "...running along the long axis, not collapsed to a point: {ridge:?}"
        );
    }

    #[test]
    fn pitch_sets_the_rise_across_the_short_span() {
        let m = model();
        let mut seg = segment(RoofType::Gable, 8.0, 5.0);
        seg.pitch_deg = 45.0;
        let out = solids(&m, &seg);
        // 45 degrees over a 2.5 m half-run gives a 2.5 m rise.
        assert!((peak(&out[0]) - 2.5).abs() < EPS, "got {}", peak(&out[0]));
    }

    #[test]
    fn a_flat_pitch_drops_the_sloped_tier_rather_than_emitting_a_sheet() {
        // Zero pitch makes the hull coplanar. Manifold would return an empty
        // mesh for it and say nothing.
        let m = model();
        let mut seg = segment(RoofType::Gable, 8.0, 5.0);
        seg.pitch_deg = 0.0;
        assert!(solids(&m, &seg).is_empty());
    }

    #[test]
    fn overhang_grows_the_eaves_on_every_side() {
        let m = model();
        let mut seg = segment(RoofType::Gable, 8.0, 5.0);
        seg.overhang = 0.4;
        let out = solids(&m, &seg);
        let eave: Vec<P3> = points(&out[0]).into_iter().filter(|p| p[1] < EPS).collect();
        let x = eave.iter().map(|p| p[0].abs()).fold(0.0, f32::max);
        let z = eave.iter().map(|p| p[2].abs()).fold(0.0, f32::max);
        assert!((x - 4.4).abs() < EPS && (z - 2.9).abs() < EPS, "{x}, {z}");
    }

    #[test]
    fn a_roof_sits_on_its_walls_at_its_levels_plane() {
        let mut m = model();
        m.levels.push(Level { id: LevelId(1), name: None, height: 2.7 });
        let mut seg = segment(RoofType::Gable, 8.0, 5.0);
        seg.level = LevelId(1);
        seg.centre = [3.0, -2.0];
        let out = solids(&m, &seg);
        // Level 1's plane is 2.7 up, plus a 2.5 m wall.
        assert_eq!(out[0].placement.translation, [3.0, 5.2, -2.0]);
    }

    #[test]
    fn rotation_rides_on_the_placement_not_the_points() {
        // Baking rotation into the hull would mean nudging a roof's angle
        // rewrites every coordinate in the emitted source.
        let m = model();
        let mut seg = segment(RoofType::Gable, 8.0, 5.0);
        seg.rotation = 0.7;
        let rotated = solids(&m, &seg);
        seg.rotation = 0.0;
        let square = solids(&m, &seg);

        assert_eq!(points(&rotated[0]), points(&square[0]), "points must not move");
        assert_eq!(rotated[0].placement.rotation, 0.7);
    }

    /// A room of `top`-tall walls centred on the origin, `w` × `d` between
    /// centrelines.
    fn walled(m: &mut ArchModel, w: f32, d: f32, top: f32) {
        let (hw, hd) = (0.5 * w, 0.5 * d);
        let c = [[-hw, -hd], [hw, -hd], [hw, hd], [-hw, hd]];
        for i in 0..4 {
            m.push_wall(Wall {
                id: WallId(0),
                level: LevelId(0),
                start: c[i],
                end: c[(i + 1) % 4],
                thickness: 0.3,
                height: Some(top),
                curve_offset: None,
                openings: Vec::new(),
                material: None,
            });
        }
    }

    fn eave(out: &RoofSolids) -> f32 {
        out.solids[0].placement.translation[1]
    }

    #[test]
    fn a_roof_that_would_sink_into_its_walls_is_lifted_onto_them() {
        // The defect this exists for: an eave below the walls it covers puts a
        // band of wall out through the slopes, and every one of those walls is
        // a perfectly valid closed solid, so nothing downstream can tell it
        // from a design.
        let mut m = model();
        walled(&mut m, 8.0, 5.0, 2.6);
        let mut seg = segment(RoofType::Gable, 8.0, 5.0);
        seg.wall_height = 0.0;

        let out = super::solids(&m, &seg);
        assert_eq!(eave(&out), 2.6, "the eave should rest on the wall tops");
        assert_eq!(out.raised, Some(2.6));
    }

    #[test]
    fn a_roof_already_clear_of_its_walls_is_left_where_it_was_put() {
        // Only ever raised. A roof above its walls is a knee wall someone
        // meant, and pulling it down would be the solver overruling a design.
        let mut m = model();
        walled(&mut m, 8.0, 5.0, 2.6);
        let mut seg = segment(RoofType::Gable, 8.0, 5.0);
        seg.wall_height = 3.4;

        let out = super::solids(&m, &seg);
        assert_eq!(eave(&out), 3.4);
        assert_eq!(out.raised, None);
    }

    #[test]
    fn a_porch_roof_is_not_hoisted_by_the_wall_it_abuts() {
        // A 2 m porch against a two-storey wall. The wall's centreline runs
        // along the porch's own edge, so anything testing *overlap* would lift
        // the porch to 6 m; requiring the whole centreline is what keeps it at
        // the height it was drawn.
        let mut m = model();
        walled(&mut m, 8.0, 5.0, 6.0);
        let mut seg = segment(RoofType::Shed, 3.0, 2.0);
        seg.centre = [0.0, -3.5];
        seg.wall_height = 2.4;

        let out = super::solids(&m, &seg);
        assert_eq!(eave(&out), 2.4);
        assert_eq!(out.raised, None);
    }

    #[test]
    fn the_lift_follows_a_rotated_segment() {
        // A segment turned a quarter turn covers a different set of walls, and
        // reading its extents without undoing the rotation swaps them.
        let mut m = model();
        // A long thin room on Z: only a segment turned to match spans it.
        walled(&mut m, 2.0, 12.0, 2.6);
        let mut seg = segment(RoofType::Gable, 3.0, 14.0);
        seg.wall_height = 0.0;
        assert_eq!(super::solids(&m, &seg).raised, Some(2.6));

        seg.rotation = std::f32::consts::FRAC_PI_2;
        assert_eq!(
            super::solids(&m, &seg).raised,
            None,
            "turned across the room, it covers no wall midpoint"
        );
    }

    #[test]
    fn a_degenerate_segment_produces_nothing() {
        let m = model();
        assert!(solids(&m, &segment(RoofType::Gable, 0.0, 5.0)).is_empty());
        assert!(solids(&m, &segment(RoofType::Hip, 8.0, 0.001)).is_empty());
    }

    #[test]
    fn a_roof_on_a_missing_level_produces_nothing() {
        let m = model();
        let mut seg = segment(RoofType::Gable, 8.0, 5.0);
        seg.level = LevelId(9);
        assert!(solids(&m, &seg).is_empty());
    }

    #[test]
    fn roofs_are_reproducible() {
        let m = model();
        for rt in [RoofType::Gable, RoofType::Hip, RoofType::Gambrel, RoofType::Dutch] {
            let seg = segment(rt, 7.0, 4.5);
            assert_eq!(solids(&m, &seg), solids(&m, &seg), "{rt:?}");
        }
    }
}
