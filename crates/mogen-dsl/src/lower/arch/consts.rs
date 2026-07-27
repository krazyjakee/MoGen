//! Every tolerance the architectural solver uses, in one place.
//!
//! Two call sites disagreeing on a tolerance is how cracks appear: if a wall
//! tessellates its arc at 24 segments and the junction filler assumes 32, the
//! filler's hull misses the wall's actual corner points. Keeping the numbers
//! here makes that class of bug impossible to introduce by accident.

/// Junction snapping resolution. Wall endpoints are quantised to this grid
/// before anything else runs, so two walls meeting at a corner read *bit
/// identical* endpoints and independently compute the same mitre point.
///
/// Matches the 1 mm grid pascalorg/editor uses for the same purpose.
pub(super) const SNAP_MM: f32 = 1000.0;

/// Past this multiple of a wall's half-thickness, a mitre point is treated as
/// a runaway spike and the joint falls back to a square butt.
///
/// As the angle between two walls approaches zero the mitre point runs to
/// infinity; without a cap the wall renders as an infinite spike and its plan
/// polygon self-intersects, which is a hole waiting to happen. Same value as
/// their `MITER_LIMIT`.
pub(super) const MITER_LIMIT: f32 = 10.0;

/// Segment count for sampling a curved wall's centreline.
///
/// Their renderer uses the same count for wall *surfaces*. (They separately use
/// adaptive deviation-based subdivision for room detection, which is a
/// different job with a different tolerance — don't conflate them.)
pub(super) const ARC_SEGMENTS: usize = 24;

/// Below this, a float is treated as zero when reasoning about curvature and
/// direction. Their `CURVE_EPSILON`.
pub(super) const CURVE_EPSILON: f32 = 1e-6;

/// Two directions whose 2D cross product falls below this are parallel, so no
/// mitre point exists and the joint butts instead.
pub(super) const COLLINEAR_EPS: f32 = 1e-4;

/// Walls shorter than this are dropped rather than emitted as slivers.
pub(super) const MIN_WALL_H: f32 = 0.05;

/// Wall panels (the pieces either side of an opening) thinner than this are
/// dropped — a sub-centimetre pier is visual noise and a degenerate-geometry
/// risk, not architecture.
pub(super) const MIN_PANEL: f32 = 0.02;

/// Ceilings carry no thickness in the IR, but a zero-thickness sheet cannot be
/// watertight, so the solver gives them this.
pub(super) const CEILING_SHELL_THICKNESS: f32 = 0.02;

/// Slack allowed when checking that two solids actually meet (roof sitting on
/// wall tops, slab rim overlapping a wall footprint).
pub(super) const CONNECTIVITY_SLOP: f32 = 0.002;
