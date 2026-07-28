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

/// How far off a line a point may sit and still count as *on* it when deciding
/// whether two ring edges lie along one another.
///
/// Deliberately a distance rather than a cross product, so it means the same
/// thing on a 0.1 m nib as on a 40 m run: a raw cross product scales with both
/// edge lengths, which is how a test tuned on short edges silently stops firing
/// on long ones. 0.1 mm is far below any real wall feature and far above f32
/// noise at plan coordinates.
pub(super) const OVERLAP_EPS: f32 = 1e-4;

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

/// A slab must carry at least this much of a wall's length to count as
/// supporting it. Their `WALL_SLAB_MIN_OVERLAP`; stops a slab that merely
/// clips a wall's corner from deciding where the whole wall stands.
pub(super) const SLAB_MIN_OVERLAP: f32 = 0.05;

/// Fraction of a wall's length a slab tier must cover to win outright. Their
/// `WALL_SLAB_SUPPORT_MAJORITY`.
pub(super) const SLAB_SUPPORT_MAJORITY: f32 = 0.5;

/// Slabs whose elevations differ by less than this are one tier, so a wall
/// spanning several slabs poured at the same level elects them together. Their
/// `WALL_SLAB_ELEVATION_POOL_EPSILON`.
pub(super) const ELEVATION_POOL_EPS: f32 = 1e-4;

/// Spacing of the samples taken along a wall's centreline when measuring how
/// much of it a slab carries.
///
/// Ours, not theirs — they compute exact overlap intervals. Sampling is the
/// simplification, but it has to be a *resolution* rather than a fixed count:
/// with a fixed count each sample stands for `length / n` metres, so on a long
/// wall one stray sample can represent more than [`SLAB_MIN_OVERLAP`] and a
/// slab clipping the very end of a wall hoists the whole wall onto it. At 10 mm
/// a sample is always well under that threshold, whatever the wall's length.
pub(super) const SUPPORT_SAMPLE_STEP: f32 = 0.01;

/// Bounds on the sample count, so a 100 mm nib still gets a usable measurement
/// and a 200 m wall does not cost 20,000 point-in-polygon tests per slab.
pub(super) const SUPPORT_SAMPLES_MIN: usize = 9;
pub(super) const SUPPORT_SAMPLES_MAX: usize = 4096;
