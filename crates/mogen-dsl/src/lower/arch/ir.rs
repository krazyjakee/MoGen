//! The architectural IR: what a building *is*, before any geometry exists.
//!
//! A wall is a centreline, not a box. A slab is a polygon, not a rectangle. A
//! roof is a type and a pitch, not a pile of wedges. Describing a building this
//! way costs nothing extra for a plain rectangular house, but it can also
//! express an L-shaped room, a wall at 30°, or a curved bay — none of which the
//! `building` generator's `Rect2` model can represent.
//!
//! Two producers fill this in: the Pascal-editor importer (now) and the
//! `building` generator (later). One solver consumes it. That split is the
//! whole point — geometry maths lives on this side, never in a producer.
//!
//! # Conventions
//!
//! - Plan coordinates are `[x, z]` in metres, world space. **Not** `[x, y]`:
//!   the second component is the ground-plane depth axis, since we are +Y up.
//! - Angles are **radians**. (Pascal stores radians; our DSL surface is
//!   degrees. Convert at the producer boundary, once — except roof pitch, which
//!   they store in degrees and which stays degrees until `roof.rs`.)
//! - Ids are indices into the matching `Vec`, which `validate::ids_are_dense`
//!   enforces. That removes every reason to reach for a `HashMap` in the
//!   solver, and hash iteration order is the classic way determinism dies.

/// A point in the ground plane: `[x, z]`, metres, world space.
pub(crate) type P2 = [f32; 2];

/// Storey ordinal. `0` is ground, negatives are basements. Not an index —
/// levels are looked up by this value.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct LevelId(pub i32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct WallId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct SlabId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct CeilingId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct RoofId(pub u32);

/// A material by name, resolved against the scene graph at emit time. Names
/// rather than indices so a producer needn't know the material table.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct MatRef(pub String);

/// One storey. `height` is **floor-to-floor** — confirmed against
/// pascalorg/editor, whose schema says so twice. Getting this wrong delaminates
/// every storey above ground by one slab thickness.
#[derive(Clone, Debug)]
pub(crate) struct Level {
    pub id: LevelId,
    pub name: Option<String>,
    pub height: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OpeningKind {
    Door,
    Window,
    /// A bare hole — no leaf, no frame.
    Passage,
    /// A recess that does not pass through.
    Niche,
}

/// A rectangular hole in a wall, positioned in the wall's own frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Opening {
    pub kind: OpeningKind,
    /// Distance of the opening's centre from `Wall::start`, measured **along
    /// the centreline** — arc length when the wall curves, not chord distance.
    pub along: f32,
    /// Height of the opening's bottom edge above the wall's base.
    ///
    /// Their schema stores the **centre** height instead (`position[1]`, with
    /// the cutout deriving `bottom = position[1] - height/2`), so the importer
    /// converts. Sill is the architectural quantity — a window sill is a fixed
    /// height off the floor whatever the window's size — and keeping the IR in
    /// those terms means a producer changing an opening's height does not
    /// silently move it.
    pub sill: f32,
    pub width: f32,
    pub height: f32,
}

/// A wall as a centreline plus a thickness, which is what makes mitred corners
/// and curvature expressible at all.
#[derive(Clone, Debug)]
pub(crate) struct Wall {
    pub id: WallId,
    pub level: LevelId,
    /// Centreline start. Thickness is distributed ±`thickness/2` about it.
    pub start: P2,
    pub end: P2,
    pub thickness: f32,
    /// `None` means the wall is **plane-bound**: its top sits at the storey
    /// plane, i.e. `Level::height` above the level's floor plane. Its base
    /// still rests on whatever slab it stands on, so a slab makes a plane-bound
    /// wall *shorter*, never taller, and the resolved height is
    /// `level.height - slab.elevation` (see `height.rs`).
    ///
    /// Confirmed against their `wall-top.ts`: `if (wall.height == null) return
    /// storeyHeight`. The tempting alternative — stopping at the underside of
    /// the slab above — would leave a `slab.thickness` gap at every storey
    /// division, open to the sky.
    ///
    /// `Some` is an explicit height: a parapet, a half wall.
    pub height: Option<f32>,
    /// Sagitta: the perpendicular offset of the arc's midpoint from the chord.
    /// `None` or ~0 is straight.
    ///
    /// **Positive bulges to the *right* of `start → end`.** Verified against
    /// their `getWallArcData`, which places the arc centre on the `+normal`
    /// side, putting the bulge opposite. Guessing this wrong mirrors every
    /// curved wall while leaving the geometry perfectly valid — i.e. silently.
    pub curve_offset: Option<f32>,
    pub openings: Vec<Opening>,
    pub material: Option<MatRef>,
}

/// A closed outer ring with optional holes. Rings are not required to repeat
/// their first point; winding is normalised by the solver.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Polygon {
    pub outer: Vec<P2>,
    pub holes: Vec<Vec<P2>>,
}

/// A floor plate. The solid spans `[elevation - thickness, elevation]`, so
/// `elevation` is the walking surface.
#[derive(Clone, Debug)]
pub(crate) struct Slab {
    pub id: SlabId,
    pub level: LevelId,
    pub poly: Polygon,
    /// Top face, relative to the level's floor plane.
    pub elevation: f32,
    pub thickness: f32,
    pub material: Option<MatRef>,
}

/// A ceiling surface. Carries no thickness — the solver gives it
/// [`consts::CEILING_SHELL_THICKNESS`] so it can be a closed solid.
#[derive(Clone, Debug)]
pub(crate) struct Ceiling {
    pub id: CeilingId,
    pub level: LevelId,
    pub poly: Polygon,
    /// `None` means the storey height.
    pub elevation: Option<f32>,
    pub material: Option<MatRef>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RoofType {
    Hip,
    Gable,
    Shed,
    Gambrel,
    Dutch,
    Mansard,
    Flat,
}

/// Shape parameters for the roof types that need more than a pitch.
///
/// A flat struct rather than per-variant enum payloads, so a producer can fill
/// it in unconditionally without matching on the type first.
///
/// Names and defaults are taken from pascalorg/editor's `roof-segment` schema
/// rather than invented. Gambrel and Mansard are **not** the same break — they
/// carry separate ratios there (0.5/0.6 against 0.15/0.7), and folding them
/// into one pair would turn every imported mansard into a gambrel while leaving
/// it a perfectly plausible roof.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RoofParams {
    /// Gambrel: fraction of the half-run at which the lower slope breaks.
    pub gambrel_lower_width: f32,
    /// Gambrel: fraction of total rise taken by the lower slope.
    pub gambrel_lower_height: f32,
    /// Mansard: fraction of the half-run taken by the steep lower face.
    pub mansard_steep_width: f32,
    /// Mansard: fraction of total rise taken by that steep face.
    pub mansard_steep_height: f32,
    /// Dutch: fraction of the half-run the hip covers before the gablet.
    pub dutch_hip_width: f32,
    /// Dutch: fraction of the rise where the hip stops and the gablet starts.
    pub dutch_hip_height: f32,
    /// Thickness of the roof deck — a flat roof's whole solid, and the slab
    /// under the covering on every other type.
    pub deck_thickness: f32,
}

impl Default for RoofParams {
    fn default() -> Self {
        Self {
            gambrel_lower_width: 0.5,
            gambrel_lower_height: 0.6,
            mansard_steep_width: 0.15,
            mansard_steep_height: 0.7,
            dutch_hip_width: 0.25,
            dutch_hip_height: 0.5,
            deck_thickness: 0.15,
        }
    }
}

/// One roof volume over an axis-aligned rectangle, rotated about +Y.
///
/// Non-rectangular roofs come from composing several segments, not from giving
/// a segment a polygon — which matches how their editor models it.
#[derive(Clone, Debug)]
pub(crate) struct RoofSegment {
    pub id: RoofId,
    pub level: LevelId,
    pub centre: P2,
    /// Extents *before* overhang.
    pub width: f32,
    pub depth: f32,
    /// Radians about +Y. Not baked into the geometry — it rides on the part's
    /// placement, so the emitted source stays byte-stable.
    pub rotation: f32,
    /// **Degrees**, converted once inside `roof.rs`. Stored as-is because
    /// that's how their schema stores it and how a user thinks about a roof.
    pub pitch_deg: f32,
    pub roof_type: RoofType,
    pub overhang: f32,
    /// Height of the supporting wall above the level floor; the roof starts here.
    pub wall_height: f32,
    pub params: RoofParams,
    pub material: Option<MatRef>,
}

/// Which producer built this model. Only used for diagnostics and provenance
/// tags — the solver treats both identically, which is the point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ModelSource {
    PascalEditor,
    BuildingGenerator,
}

/// A whole building, ready to solve.
#[derive(Clone, Debug)]
pub(crate) struct ArchModel {
    /// Sorted ascending by `LevelId`; checked by `validate`.
    pub levels: Vec<Level>,
    /// Index == `WallId.0`.
    pub walls: Vec<Wall>,
    /// Index == `SlabId.0`.
    pub slabs: Vec<Slab>,
    /// Index == `CeilingId.0`.
    pub ceilings: Vec<Ceiling>,
    /// Index == `RoofId.0`.
    pub roofs: Vec<RoofSegment>,
    pub source: ModelSource,
}

impl ArchModel {
    pub fn new(source: ModelSource) -> Self {
        Self {
            levels: Vec::new(),
            walls: Vec::new(),
            slabs: Vec::new(),
            ceilings: Vec::new(),
            roofs: Vec::new(),
            source,
        }
    }

    /// Append a wall, assigning its id from the current length.
    pub fn push_wall(&mut self, mut w: Wall) -> WallId {
        let id = WallId(self.walls.len() as u32);
        w.id = id;
        self.walls.push(w);
        id
    }

    pub fn push_slab(&mut self, mut s: Slab) -> SlabId {
        let id = SlabId(self.slabs.len() as u32);
        s.id = id;
        self.slabs.push(s);
        id
    }

    pub fn push_ceiling(&mut self, mut c: Ceiling) -> CeilingId {
        let id = CeilingId(self.ceilings.len() as u32);
        c.id = id;
        self.ceilings.push(c);
        id
    }

    pub fn push_roof(&mut self, mut r: RoofSegment) -> RoofId {
        let id = RoofId(self.roofs.len() as u32);
        r.id = id;
        self.roofs.push(r);
        id
    }

    pub fn level(&self, id: LevelId) -> Option<&Level> {
        self.levels.iter().find(|l| l.id == id)
    }
}
