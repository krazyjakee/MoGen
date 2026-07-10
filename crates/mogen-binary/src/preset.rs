//! The shared, versioned dictionary — MOGB's answer to "gzip has to learn the
//! vocabulary; we ship it."
//!
//! Every string here is assigned a stable index (its position in [`PRESET`])
//! that is *implied by the format version* and never written into a `.mogb`
//! file. Node kinds and common attribute keys dominate real `.mog` files, so a
//! reference into this table is 1–2 bytes with no per-file string-table cost.
//!
//! Strings that are **not** here (user material names, tags, module names,
//! prompts) fall through to the per-file string table — so this list never has
//! to be exhaustive, only *common*. Adding entries is safe but changes indices,
//! which is why touching this list requires bumping [`crate::VERSION`].
//!
//! The list is deliberately a superset drawn from the validator's `KNOWN_KINDS`
//! and the geometry/transform attribute allowlists, plus a handful of very
//! common enum *values*. Duplicates are harmless: the interner keeps the first
//! index for any repeated string.

/// Ordered preset dictionary. Index = position. Do not reorder or remove
/// entries without bumping [`crate::VERSION`] — appending is the only
/// backward-compatible edit within a version bump.
pub const PRESET: &[&str] = &[
    // ── node kinds ─────────────────────────────────────────────────────────
    "scene", "group", "solid", "material", "connector", "attach", "conform",
    "mirror", "array", "stack", "grid", "meta",
    "box", "plane", "quad", "cylinder", "cone", "sphere", "capsule", "torus",
    "prism", "pyramid", "disc", "icosphere", "rounded_box", "chamfered_box",
    "inset_box", "wedge", "frustum", "tube", "hemisphere", "half_cylinder",
    "torus_arc", "ellipsoid", "heightfield", "bezier_patch", "metaball", "blob",
    "superellipsoid", "curved_plane", "lathe", "spline_tube", "spline_ribbon",
    "coil", "leaf_card", "mesh", "extrude", "sweep", "loft", "hull", "poly",
    "slab", "post", "panel", "wall", "branch",
    "building", "room_type", "adjacency",
    "cave", "feature", "terrain", "hole", "road", "dungeon", "decal",
    "module", "use", "import",
    "union", "difference", "intersect",
    "joint", "clip", "track",
    "spin", "open_close", "wave", "flap", "idle",
    "skeleton", "bone", "lod_scale",
    "if", "else", "for", "light",

    // ── transforms & placement (geometry-common) ───────────────────────────
    "pos", "rot", "scale", "role", "tags", "mat", "skin", "bind",
    "x", "y", "z", "rx", "ry", "rz", "w", "h", "d",
    "anchor", "from", "to", "gap",
    "above", "below", "left_of", "right_of", "in_front_of", "behind",
    "collider", "cast_shadow",

    // ── deformation modifiers ──────────────────────────────────────────────
    "seed", "noise", "jitter", "bend_x", "bend_y", "bend_z", "twist_y",
    "taper", "droop", "faceted",
    "wave_frequency", "wave_axis", "wave_phase",
    "noise_range", "jitter_range", "bend_x_range", "bend_y_range",
    "bend_z_range", "twist_y_range", "taper_range", "droop_range", "wave_range",
    "lod", "subdivide", "op",

    // ── common primitive/geometry attributes ───────────────────────────────
    "size", "radius", "height", "width", "depth", "segments", "segments_u",
    "segments_v", "rings", "radii", "major", "minor", "major_segments",
    "minor_segments", "caps", "cap_ends", "cap", "top", "bottom", "taper",
    "twist", "profile", "path", "points", "samples", "resolution", "axis",
    "smooth", "solid", "inset", "chamfer", "roundness", "thickness", "sweep",
    "arc", "start", "count", "spacing", "step", "pivot", "center", "offset",
    "blend", "amplitude", "frequency", "persistence", "octaves", "lift",
    "jitter", "faces", "uv_mode", "uv_scale", "uv_offset", "uv_swap",

    // ── material attributes ────────────────────────────────────────────────
    "color", "roughness", "metallic", "emissive", "emissive_strength",
    "transmission", "alpha", "alpha_mode", "texture", "gradient", "double_sided",

    // ── procedural-generator attributes (cave/building/terrain/dungeon) ─────
    "levels", "floors_above", "floors_below", "floor_area", "floor_thickness",
    "ceiling_height", "ceiling_thickness", "wall_thickness", "roof", "style",
    "mat_style", "rooms", "room_min", "room_max", "min_size", "max_size",
    "min_area", "max_area", "corridor_width", "chambers", "chamber_min",
    "chamber_max", "chamber_flatten", "entrances", "stairs", "staircases",
    "elevators", "skylights", "windows", "window_w", "window_h", "door_w",
    "door_h", "columns", "density", "loops", "keys", "level_gap", "level_links",
    "adjacent_to", "away_from", "prop_spots", "debug_show_poi",
    "debug_render_floor", "debug_hide_roof", "cellar_area", "office", "logo",
    "heightfield", "frequency", "amplitude", "octaves", "lakes", "pools",
    "ground", "grid", "cell", "margin", "max_slope", "curved", "flat",
    "floating", "target", "range", "limits", "easing", "seconds", "hz",
    "shoulder", "head", "leg", "body", "rig", "kind", "type", "form",
    "mogen_version", "prompt", "conform",

    // ── very common enum *values* (idents / short strings) ──────────────────
    "true", "false", "none", "on", "off",
    "linear", "vertical", "radial", "stops",
    "aabb", "subtract", "sub", "carve", "wood", "metal", "stone",
    "flat_top", "gable", "hip", "dome",
];
