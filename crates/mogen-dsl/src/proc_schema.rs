//! Public parameter schema for the procedural generators (`branch`,
//! `building`, `cave`, `terrain`, `dungeon`).
//!
//! Each generator is, from the editor's point of view, a flat list of named
//! attributes with a type, a default, an edit range, and a group. This module
//! describes that list once so the Studio inspector can render every
//! generator's sidebar through a single generic widget loop instead of a
//! hand-written arm per kind — and any future generator gets a consistent
//! sidebar for free just by adding a [`ProcSchema`] here.
//!
//! The schema is the **UI source of truth**: the defaults shown when an attr is
//! absent, the clamp ranges for the drag widgets, and (critically) the enum
//! option lists, which would otherwise drift from the tokens the lowering
//! actually accepts. Lowering keeps its *own* defaults — `branch`'s are
//! form-dependent, so a single static default here couldn't drive both — so the
//! defaults below are the static (decurrent / `grid` / `flat` …) values the
//! sidebar shows, and a drift-guard test asserts every enum option here still
//! lowers without error.

/// One selectable value for an [`ParamKind::Enum`] attribute, plus the hover
/// help shown beside it in the combo box.
pub struct EnumOption {
    pub value: &'static str,
    pub help: &'static str,
}

/// The widget + value type for one parameter. Ranges drive the drag widgets;
/// scalars are intentionally unbounded above (matching the inspector's existing
/// behaviour) and only carry a drag `speed`.
pub enum ParamKind {
    /// Floating-point value rendered as an unbounded DragValue.
    Scalar { default: f32, speed: f32 },
    /// Integer value rendered as a DragValue clamped to `[min, max]`.
    Int { default: i32, min: i32, max: i32 },
    /// 0/1 boolean rendered as a checkbox (the DSL has no native bool; the
    /// lowering reads `n.abs() > 0.5`).
    Bool { default: bool },
    /// String-valued attribute rendered as a combo box, written back quoted.
    Enum { default: &'static str, options: &'static [EnumOption] },
    /// Fixed-length numeric vector (e.g. cave `size=[w, h, d]`). `defaults`
    /// supplies the per-component value shown when the attr is absent; its
    /// length is the vector's arity.
    Vec { defaults: &'static [f32], speed: f32 },
}

/// Which sidebar section a parameter belongs to. `Main` params render first
/// with no subheader; each other group renders under its own labelled row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ParamGroup {
    Main,
    Collider,
    Debug,
}

impl ParamGroup {
    /// Subheader label shown above a group's rows. `Main` has none.
    pub fn header(self) -> Option<&'static str> {
        match self {
            ParamGroup::Main => None,
            ParamGroup::Collider => Some("Collider"),
            ParamGroup::Debug => Some("Debug"),
        }
    }
}

/// One editable attribute of a procedural generator.
pub struct ParamSpec {
    pub attr: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    pub group: ParamGroup,
    /// Optional hover help shown on the row label.
    pub help: Option<&'static str>,
}

/// The full editable-parameter list for one generator kind.
pub struct ProcSchema {
    pub kind: &'static str,
    pub params: &'static [ParamSpec],
}

/// The schema for a procedural generator node kind, or `None` if `kind` is not
/// a procedural generator (e.g. a primitive, which the inspector renders from
/// its own hard-coded table).
pub fn schema_for(kind: &str) -> Option<&'static ProcSchema> {
    match kind {
        "branch" => Some(&BRANCH),
        "building" => Some(&BUILDING),
        "cave" => Some(&CAVE),
        "terrain" => Some(&TERRAIN),
        "dungeon" => Some(&DUNGEON),
        _ => None,
    }
}

/// Every procedural schema, for callers that want to enumerate them (tests,
/// docs tooling).
pub fn all() -> &'static [&'static ProcSchema] {
    static ALL: [&ProcSchema; 5] = [&BRANCH, &BUILDING, &CAVE, &TERRAIN, &DUNGEON];
    &ALL
}

use ParamGroup::{Collider, Debug, Main};

const fn scalar(attr: &'static str, label: &'static str, default: f32, speed: f32) -> ParamSpec {
    ParamSpec { attr, label, kind: ParamKind::Scalar { default, speed }, group: Main, help: None }
}

const fn int(
    attr: &'static str,
    label: &'static str,
    default: i32,
    min: i32,
    max: i32,
) -> ParamSpec {
    ParamSpec { attr, label, kind: ParamKind::Int { default, min, max }, group: Main, help: None }
}

static BRANCH: ProcSchema = ProcSchema {
    kind: "branch",
    params: &[
        ParamSpec {
            attr: "form",
            label: "Form",
            kind: ParamKind::Enum {
                default: "decurrent",
                options: &[
                    EnumOption { value: "decurrent", help: "Spreading, rounded crown (oak-like)" },
                    EnumOption { value: "excurrent", help: "Single dominant leader (conifer-like)" },
                    EnumOption { value: "weeping", help: "Drooping branches (willow-like)" },
                    EnumOption { value: "shrub", help: "Multi-stem cluster from the base" },
                    EnumOption { value: "palm", help: "Bare trunk with a crown of fronds" },
                ],
            },
            group: Main,
            help: None,
        },
        scalar("length", "Length", 1.0, 0.02),
        scalar("radius", "Radius", 0.05, 0.005),
        int("depth", "Depth", 4, 1, 8),
        int("splits", "Splits", 2, 1, 8),
        scalar("length_falloff", "Length falloff", 0.7, 0.01),
        scalar("radius_falloff", "Radius falloff", 0.6, 0.01),
        scalar("branch_angle", "Branch angle\u{00b0}", 35.0, 1.0),
        scalar("roll", "Roll\u{00b0}", 137.5, 1.0),
        scalar("tropism", "Tropism", 0.0, 0.02),
        scalar("bend", "Bend\u{00b0}", 10.0, 0.5),
        scalar("leader_bias", "Leader bias", 0.0, 0.02),
        int("multi_stem", "Multi-stem", 1, 1, 8),
        int("segments", "Segments", 8, 3, 64),
        int("samples", "Samples", 4, 1, 16),
        int("seed", "Seed", 1, 1, 1_000_000),
        scalar("jitter", "Jitter", 0.2, 0.02),
        ParamSpec {
            attr: "leaves",
            label: "Leaves",
            kind: ParamKind::Bool { default: true },
            group: Main,
            help: None,
        },
        scalar("leaf_size", "Leaf size", 0.35, 0.01),
        scalar("leaf_aspect", "Leaf aspect", 1.0, 0.05),
        int("leaf_cards", "Leaf cards", 2, 1, 8),
    ],
};

static BUILDING: ProcSchema = ProcSchema {
    kind: "building",
    params: &[
        int("seed", "Seed", 1, 1, 1_000_000),
        ParamSpec {
            attr: "style",
            label: "Style",
            kind: ParamKind::Enum {
                default: "grid",
                options: &[
                    EnumOption { value: "grid", help: "Rectangular rooms packed on a grid" },
                    EnumOption { value: "apartment-block", help: "Stacked residential units" },
                    EnumOption { value: "hotel-corridor", help: "Rooms either side of a corridor" },
                    EnumOption { value: "office-core", help: "Rooms around a central service core" },
                    EnumOption { value: "radial", help: "Rooms fanning out from a centre" },
                    EnumOption { value: "organic", help: "Irregular, non-grid room packing" },
                    EnumOption { value: "maze", help: "Dense maze-like room warren" },
                ],
            },
            group: Main,
            help: None,
        },
        ParamSpec {
            attr: "roof",
            label: "Roof",
            kind: ParamKind::Enum {
                default: "flat",
                options: &[
                    EnumOption { value: "flat", help: "Flat slab roof" },
                    EnumOption { value: "gabled", help: "Two slopes meeting at a ridge" },
                    EnumOption { value: "pitched", help: "Synonym of gabled (axis-aligned)" },
                    EnumOption { value: "hipped", help: "Slopes on all four sides" },
                    EnumOption { value: "mansard", help: "Double-slope (steep lower) roof" },
                    EnumOption { value: "shed", help: "Single slope (lean-to)" },
                ],
            },
            group: Main,
            help: None,
        },
        scalar("floor_area", "Floor area m\u{00b2}", 120.0, 1.0),
        int("rooms", "Rooms", 4, 1, 256),
        int("floors_above", "Floors above", 1, 1, 64),
        int("floors_below", "Floors below", 0, 0, 32),
        int("windows", "Windows", 0, 0, 1024),
        int("skylights", "Skylights", 0, 0, 256),
        scalar("ceiling_height", "Ceiling height", 2.6, 0.05),
        scalar("door_w", "Door W", 0.9, 0.02),
        scalar("door_h", "Door H", 2.1, 0.02),
        scalar("window_w", "Window W", 1.2, 0.02),
        scalar("window_h", "Window H", 1.4, 0.02),
        scalar("wall_thickness", "Wall thickness", 0.12, 0.005),
        scalar("ceiling_thickness", "Ceiling thickness", 0.2, 0.005),
        int("entrances", "Entrances", 1, 1, 64),
        int("elevators", "Elevators", 0, 0, 32),
        int("staircases", "Staircases", 0, 0, 32),
        ParamSpec {
            attr: "furnish",
            label: "Furnish (POI)",
            kind: ParamKind::Bool { default: true },
            group: Main,
            help: Some(
                "Drop transform-only furniture POI markers into each room \
                 (bed, desk, stove, …). The markers carry no geometry; turn \
                 off to suppress the furnishing pass entirely.",
            ),
        },
        ParamSpec {
            attr: "debug_hide_roof",
            label: "Hide roof",
            kind: ParamKind::Bool { default: false },
            group: Debug,
            help: Some(
                "Drop the top-storey ceiling slab (and its skylights) so a \
                 flat-roof building can be inspected from above.",
            ),
        },
        ParamSpec {
            attr: "debug_render_floor",
            label: "Isolate storey",
            kind: ParamKind::Int { default: 0, min: -32, max: 64 },
            group: Debug,
            help: Some(
                "Render only this signed storey index, with no ceiling and no \
                 vertical circulation, to peek inside one floor.",
            ),
        },
        ParamSpec {
            attr: "debug_show_poi",
            label: "Show POI markers",
            kind: ParamKind::Bool { default: false },
            group: Debug,
            help: Some(
                "Give every furnishing POI marker a small bright sphere so the \
                 otherwise geometry-free markers are visible in the preview.",
            ),
        },
    ],
};

static CAVE: ProcSchema = ProcSchema {
    kind: "cave",
    params: &[
        int("seed", "Seed", 1, 1, 1_000_000),
        ParamSpec {
            attr: "size",
            label: "Size",
            kind: ParamKind::Vec { defaults: &[24.0, 10.0, 24.0], speed: 0.1 },
            group: Main,
            help: Some("Outer rock-block dimensions [width, height, depth] in metres."),
        },
        int("chambers", "Chambers", 6, 1, 64),
        int("levels", "Levels", 2, 1, 16),
        scalar("chamber_min", "Chamber min", 2.5, 0.05),
        scalar("chamber_max", "Chamber max", 5.0, 0.05),
        scalar("spacing", "Spacing", 2.0, 0.05),
        scalar("overlap", "Overlap", 0.35, 0.02),
        scalar("chamber_flatten", "Chamber flatten", 0.6, 0.02),
        scalar("level_gap", "Level gap", 1.5, 0.05),
        int("level_links", "Level links", 1, 0, 16),
        scalar("passage_radius", "Passage radius", 1.1, 0.02),
        int("loops", "Loops", 1, 0, 64),
        scalar("max_slope", "Max slope\u{00b0}", 45.0, 1.0),
        scalar("roughness", "Roughness", 0.35, 0.02),
        scalar("blend", "Blend", 1.5, 0.05),
        scalar("margin", "Margin", 2.0, 0.05),
        ParamSpec {
            attr: "resolution",
            label: "Resolution",
            kind: ParamKind::Int { default: 96, min: 32, max: 224 },
            group: Main,
            help: Some("Voxel-grid resolution (samples along the longest axis)."),
        },
        int("entrances", "Entrances", 1, 0, 16),
        int("mushrooms", "Mushrooms", 0, 0, 256),
        ParamSpec {
            attr: "colliders",
            label: "Surfaces",
            kind: ParamKind::Enum {
                default: "all",
                options: &[
                    EnumOption { value: "all", help: "Rock shell + every solid decoration" },
                    EnumOption {
                        value: "shell",
                        help: "Only the outer rock shell; decorations walk-through",
                    },
                    EnumOption { value: "none", help: "No colliders on any cave geometry" },
                ],
            },
            group: Collider,
            help: None,
        },
        ParamSpec {
            attr: "water_collider",
            label: "Water is solid",
            kind: ParamKind::Bool { default: false },
            group: Collider,
            help: Some(
                "Give pools and lakes a trimesh collider so a player stands on \
                 the surface. Off by default so water is wadeable.",
            ),
        },
        ParamSpec {
            attr: "debug_show_poi",
            label: "Show POI markers",
            kind: ParamKind::Bool { default: false },
            group: Debug,
            help: Some(
                "Give every point-of-interest marker (dead-end chambers, column \
                 bases, ladder anchors, mushroom spots) a small bright sphere so \
                 the otherwise-empty markers are visible in the preview.",
            ),
        },
        ParamSpec {
            attr: "debug_hide_shell",
            label: "Hide outer shell",
            kind: ParamKind::Bool { default: false },
            group: Debug,
            help: Some(
                "Slice the front (+Z) half of the rock shell away so the \
                 chambers are visible in cross-section.",
            ),
        },
    ],
};

static TERRAIN: ProcSchema = ProcSchema {
    kind: "terrain",
    params: &[
        int("seed", "Seed", 1, 1, 1_000_000),
        ParamSpec {
            attr: "size",
            label: "Size",
            kind: ParamKind::Vec { defaults: &[40.0, 6.0, 40.0], speed: 0.5 },
            group: Main,
            help: Some("Patch dimensions [width, amplitude(height), depth] in metres."),
        },
        ParamSpec {
            attr: "source",
            label: "Source",
            kind: ParamKind::Enum {
                default: "fbm",
                options: &[
                    EnumOption { value: "fbm", help: "Rolling hills and dunes (plain fBm)" },
                    EnumOption { value: "ridged", help: "Sharp mountain ridges and canyons" },
                    EnumOption { value: "billow", help: "Lumpy, rounded cloud-like mounds" },
                    EnumOption { value: "island", help: "Central landmass falling off to water at the edges" },
                    EnumOption { value: "voronoi", help: "Cellular crater rims and cracked basins" },
                ],
            },
            group: Main,
            help: None,
        },
        int("octaves", "Octaves", 4, 1, 8),
        scalar("frequency", "Frequency", 0.06, 0.005),
        scalar("persistence", "Persistence", 0.5, 0.02),
        ParamSpec {
            attr: "resolution",
            label: "Resolution",
            kind: ParamKind::Int { default: 128, min: 4, max: 1024 },
            group: Main,
            help: Some("Grid divisions per axis (rounded up to a multiple of chunks)."),
        },
        ParamSpec {
            attr: "chunks",
            label: "Chunks",
            kind: ParamKind::Int { default: 4, min: 1, max: 16 },
            group: Main,
            help: Some("Per-axis chunk count; each chunk is an independently culled mesh."),
        },
        ParamSpec {
            attr: "lod_levels",
            label: "LOD levels",
            kind: ParamKind::Int { default: 3, min: 1, max: 4 },
            group: Main,
            help: Some(
                "Baked LOD meshes per chunk; coarser ones swap in with distance \
                 to keep big terrains cheap. 1 = full detail only.",
            ),
        },
        ParamSpec {
            attr: "smooth",
            label: "Smooth",
            kind: ParamKind::Int { default: 0, min: 0, max: 32 },
            group: Main,
            help: Some("Box-blur passes over the height field (erodes jaggies)."),
        },
        ParamSpec {
            attr: "terrace",
            label: "Terrace steps",
            kind: ParamKind::Int { default: 0, min: 0, max: 64 },
            group: Main,
            help: Some("Quantise height into this many bands for a stepped look (0 = off)."),
        },
        ParamSpec {
            attr: "sea_level",
            label: "Sea level",
            kind: ParamKind::Scalar { default: 0.0, speed: 0.01 },
            group: Main,
            help: Some(
                "Normalised water height [0, 0.95). Adds a flat water plane at \
                 this height + shoreline POIs. Land keeps its real shape below \
                 the waterline — basins are not flattened. 0 = no water.",
            ),
        },
        int("peaks", "Peak POIs", 0, 0, 1024),
        int("flat_spots", "Flat-spot POIs", 0, 0, 1024),
        int("shore_points", "Shoreline POIs", 0, 0, 1024),
        ParamSpec {
            attr: "colliders",
            label: "Surfaces",
            kind: ParamKind::Enum {
                default: "all",
                options: &[
                    EnumOption { value: "all", help: "Every terrain chunk gets a trimesh collider" },
                    EnumOption { value: "none", help: "No colliders on terrain geometry" },
                ],
            },
            group: Collider,
            help: None,
        },
        ParamSpec {
            attr: "debug_show_poi",
            label: "Show POI markers",
            kind: ParamKind::Bool { default: false },
            group: Debug,
            help: Some(
                "Give every POI marker (peaks, flat spots, shoreline) a small \
                 bright sphere so the otherwise-empty markers are visible.",
            ),
        },
    ],
};

static DUNGEON: ProcSchema = ProcSchema {
    kind: "dungeon",
    params: &[
        int("seed", "Seed", 1, 1, 1_000_000),
        ParamSpec {
            attr: "size",
            label: "Size",
            kind: ParamKind::Vec { defaults: &[48.0, 4.0, 48.0], speed: 0.5 },
            group: Main,
            help: Some(
                "Footprint + clearance [width, room height, depth] in metres. \
                 Width/depth set the grid footprint (\u{00f7} cell); room height \
                 is one level's floor-to-ceiling clearance.",
            ),
        },
        ParamSpec {
            attr: "cell",
            label: "Cell",
            kind: ParamKind::Scalar { default: 4.0, speed: 0.1 },
            group: Main,
            help: Some("Grid cell edge length in metres; rooms and corridors snap to this lattice."),
        },
        int("levels", "Levels", 1, 1, 16),
        int("stairs", "Stairs", 1, 0, 16),
        int("rooms", "Rooms", 6, 1, 64),
        int("room_min", "Room min", 2, 1, 64),
        int("room_max", "Room max", 5, 1, 64),
        ParamSpec {
            attr: "spacing",
            label: "Spacing",
            kind: ParamKind::Int { default: 1, min: 0, max: 16 },
            group: Main,
            help: Some("Minimum rock gap between rooms of the same level, in cells."),
        },
        ParamSpec {
            attr: "corridor_width",
            label: "Corridor width",
            kind: ParamKind::Int { default: 1, min: 1, max: 8 },
            group: Main,
            help: Some("Corridor width in cells."),
        },
        ParamSpec {
            attr: "loops",
            label: "Loops",
            kind: ParamKind::Int { default: 1, min: 0, max: 64 },
            group: Main,
            help: Some("Extra corridor connections beyond the spanning tree, per level."),
        },
        scalar("wall_thickness", "Wall thickness", 0.4, 0.005),
        scalar("floor_thickness", "Floor thickness", 0.4, 0.005),
        ParamSpec {
            attr: "ceilings",
            label: "Ceilings",
            kind: ParamKind::Bool { default: true },
            group: Main,
            help: Some(
                "Emit ceiling decks (each deck doubles as the next level's floor). \
                 Off = open-topped levels.",
            ),
        },
        int("prop_spots", "Prop spots", 0, 0, 1024),
        ParamSpec {
            attr: "colliders",
            label: "Surfaces",
            kind: ParamKind::Enum {
                default: "all",
                options: &[
                    EnumOption {
                        value: "all",
                        help: "Trimesh collider on every solid surface (decks, walls, steps)",
                    },
                    EnumOption { value: "none", help: "No colliders on any dungeon geometry" },
                ],
            },
            group: Collider,
            help: None,
        },
        ParamSpec {
            attr: "debug_hide_roof",
            label: "Hide roof",
            kind: ParamKind::Bool { default: false },
            group: Debug,
            help: Some("Omit the topmost deck so the rooms are visible from above in a preview."),
        },
        ParamSpec {
            attr: "debug_render_floor",
            label: "Isolate level",
            kind: ParamKind::Int { default: 0, min: 0, max: 64 },
            group: Debug,
            help: Some(
                "Render only this level index (0 = ground), with no ceiling and \
                 only the staircases that touch it, to peek inside one floor.",
            ),
        },
        ParamSpec {
            attr: "debug_show_poi",
            label: "Show POI markers",
            kind: ParamKind::Bool { default: false },
            group: Debug,
            help: Some(
                "Give every POI marker (spawn, treasure rooms, stair landings, prop \
                 spots) a small bright sphere so the otherwise-empty markers are \
                 visible in the preview.",
            ),
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lower, parse};

    /// Every enum option the sidebar offers must lower without error — this is
    /// the drift guard between the schema's option lists and the tokens the
    /// generators' `read_cfg` accept. If someone renames a lowering token
    /// (e.g. a building style) without updating the schema, the stale option
    /// fails to lower here.
    #[test]
    fn every_enum_option_lowers() {
        for schema in all() {
            for spec in schema.params {
                let ParamKind::Enum { options, .. } = &spec.kind else {
                    continue;
                };
                for opt in *options {
                    let src = building_min_src(schema.kind, spec.attr, opt.value);
                    let ast = parse(&src)
                        .unwrap_or_else(|e| panic!("parse failed for {src:?}: {e}"));
                    lower(&ast).unwrap_or_else(|e| {
                        panic!(
                            "schema enum option {}={:?} on `{}` failed to lower: {e}",
                            spec.attr, opt.value, schema.kind
                        )
                    });
                }
            }
        }
    }

    /// Build a minimal lowerable scene for `kind` with one enum attr set.
    fn building_min_src(kind: &str, attr: &str, value: &str) -> String {
        match kind {
            // `building` requires at least one room_type.
            "building" => format!(
                "{kind} \"t\" ({attr}=\"{value}\") {{ room_type \"r\" (kind=public) }}"
            ),
            _ => format!("{kind} \"t\" ({attr}=\"{value}\")"),
        }
    }
}
