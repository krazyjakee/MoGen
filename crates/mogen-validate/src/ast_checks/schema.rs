//! Static schema tables that drive AST validation: which node kinds exist,
//! which attributes each kind accepts, and what value type each attribute
//! expects. Kept separate from the walking logic so the tables read as a
//! reference manual.

use mogen_dsl::ast::Value;

pub const KNOWN_KINDS: &[&str] = &[
    "scene", "group", "solid", "material", "connector", "attach", "conform", "mirror", "array",
    "stack", "grid",
    "meta",
    "box", "plane", "quad", "cylinder", "cone", "sphere", "capsule", "torus",
    "prism", "pyramid", "disc", "icosphere", "rounded_box", "chamfered_box", "inset_box",
    "wedge", "frustum", "tube", "hemisphere", "half_cylinder", "torus_arc", "ellipsoid", "heightfield", "bezier_patch", "metaball", "blob",
    "superellipsoid", "curved_plane", "lathe", "spline_tube", "spline_ribbon", "coil", "leaf_card", "mesh",
    "extrude", "sweep", "loft",
    "slab", "post", "panel", "wall",
    "branch",
    "building", "room_type", "adjacency",
    "cave", "feature",
    "decal",
    "module", "use", "import",
    "union", "difference", "intersect",
    "joint", "clip", "track",
    "spin", "open_close", "wave", "flap", "idle",
    "skeleton", "bone",
    "lod_scale",
    "if", "else", "for",
    "light",
];

/// Attribute names accepted on any geometry/group-like node (primitives,
/// `group`, `solid`, `stack`, `grid`, `mirror`, `array`, CSG, `use`, `module`).
/// Covers transforms, material binding, role/tags metadata, and the placement
/// shortcuts (`x`/`y`/`z`, `anchor`, `from`/`to` corners, sibling relations).
pub const GEOMETRY_COMMON_ATTRS: &[&str] = &[
    "pos", "rot", "scale", "role", "tags", "mat", "skin", "bind",
    // Per-component shortcuts.
    "x", "y", "z", "rx", "ry", "rz", "w", "h", "d",
    // Placement ergonomics.
    "anchor", "from", "to", "gap",
    "above", "below", "left_of", "right_of", "in_front_of", "behind",
    // Collider request: `collider="aabb"` is the only accepted value in v1;
    // the lowering pass derives the box from the node's subtree mesh extents.
    "collider",
    // Shadow opt-out: `cast_shadow=0` excludes this node (and its subtree) from
    // the realtime shadow pre-pass and from the exported glTF shadow hint.
    // Default is true, so authors only ever write the attribute to disable it.
    "cast_shadow",
    // Deformation modifiers — composable variety knobs that work on any
    // primitive. Apply between primitive construction and anchor shift, so
    // the deformed mesh is what attach/connector logic sees. Stochastic
    // modifiers (`noise`, `jitter`) are seeded by `seed`. The matching
    // `*_range=[a,b]` attrs gate each deformation to a normalised slice
    // along its length axis (smoothstep ramp from `a` to `b`).
    "seed", "noise", "jitter", "bend_x", "bend_y", "bend_z", "twist_y",
    "taper", "droop", "faceted",
    // Periodic-wave deformer (sinusoidal displacement along the vertex
    // normal). Composes with the other modifiers; see deform.rs::wave.
    "wave", "wave_frequency", "wave_axis", "wave_phase",
    "noise_range", "jitter_range",
    "bend_x_range", "bend_y_range", "bend_z_range", "twist_y_range",
    "taper_range", "droop_range", "wave_range",
    // Per-node LOD multiplier — compounds with the file-global `lod_scale`
    // for the duration of this node and its subtree (see lod.rs guards).
    "lod",
    // Loop subdivision post-pass count (clamped to [0, 3]). Honoured on any
    // mesh-producing kind: leaf primitives, `union`/`difference`/`intersect`,
    // and `blob`. No-op on group/scene/replicator nodes.
    "subdivide",
    // Blob-child operator: `op="subtract"` (or `"sub"`/`"carve"`) inside a
    // `blob {}` body carves a smooth cavity instead of adding mass. Ignored
    // outside a `blob` container — kept in the geometry-common list so the
    // LLM can write `sphere(op=subtract)` as a blob child without tripping
    // the per-kind allowlist on `sphere`.
    "op",
];

/// Transform-only subset. Used by `skeleton` (places the whole rig) and `bone`
/// (parent-relative offset) — they carry positions but none of the placement
/// shortcuts or material bindings.
pub const TRANSFORM_COMMON_ATTRS: &[&str] = &[
    "pos", "rot", "scale",
    "x", "y", "z", "rx", "ry", "rz",
];

/// Subset for `light`: transforms (placement + rotation drive direction) plus
/// `role` and `tags` for metadata. No material binding, no placement shortcuts
/// — a light has no AABB to anchor against.
pub const LIGHT_COMMON_ATTRS: &[&str] = &[
    "pos", "rot", "scale",
    "x", "y", "z", "rx", "ry", "rz",
    "role", "tags",
];

/// Subset for `decal`: transforms and placement helpers, plus `role`/`tags`.
/// No `mat`/`skin`/`bind` — a decal owns its synthesized material outright,
/// and accepting `mat=` would let an author silently swap in a material that
/// lacks the alpha/double-sided settings the decal pipeline depends on.
pub const DECAL_COMMON_ATTRS: &[&str] = &[
    "pos", "rot", "scale", "role", "tags",
    "x", "y", "z", "rx", "ry", "rz", "w", "h",
    "anchor", "from", "to", "gap",
    "above", "below", "left_of", "right_of", "in_front_of", "behind",
];

/// Per-kind common-attribute bucket. Kinds that are neither geometry nor
/// transform-bearing (materials, connectors, joins, animation tracks and
/// templates) accept ONLY their kind-specific allowlist — no implicit
/// `pos=`/`from=`/`anchor=`. This is what stops mistakes like `from=[1,0,1]`
/// on `open_close` from being silently accepted.
pub fn common_attrs_for_kind(kind: &str) -> &'static [&'static str] {
    match kind {
        "skeleton" | "bone" => TRANSFORM_COMMON_ATTRS,
        "light" => LIGHT_COMMON_ATTRS,
        "decal" => DECAL_COMMON_ATTRS,
        "material" | "connector" | "attach"
        | "joint" | "clip" | "track"
        | "spin" | "open_close" | "wave" | "flap" | "idle"
        | "lod_scale" | "meta"
        // `room_type` and `adjacency` are pure metadata children of `building`
        // — they carry no transforms, materials, or placement helpers; the
        // generator owns where their geometry lands. `feature` plays the same
        // role for `cave` (it tunes one decoration kind).
        | "room_type" | "adjacency" | "feature"
        // Control-flow constructs are pre-expansion only — they don't
        // accept transforms or material binding, just their control attrs.
        | "if" | "else" | "for" => &[],
        _ => GEOMETRY_COMMON_ATTRS,
    }
}

pub fn attrs_for_kind(kind: &str) -> &'static [&'static str] {
    match kind {
        "meta" => &[
            "name", "version", "mogen_version", "description", "tags",
            "seed", "thinking", "prompt", "style",
            // Stamped by MoGen Studio's Publish dialog after a successful
            // upload; subsequent publishes use them to republish into the
            // same MoGHub model instead of allocating a new slug.
            "moghub_model_id", "moghub_slug", "moghub_version",
        ],
        "box" | "plane" | "quad" | "prism" => &["size"],
        "slab" | "post" | "panel" => &["size"],
        "wall" => &["size", "holes"],
        "stack" => &["axis", "align", "pack"],
        "solid" => &["cleanup"],
        "grid" => &["count", "step", "center"],
        "cylinder" | "cone" => &["radius", "height", "segments"],
        "sphere" => &["radius", "rings", "segments"],
        "capsule" => &["radius", "height", "rings", "segments"],
        "torus" => &["major", "minor", "major_segments", "minor_segments"],
        "pyramid" => &["radius", "height", "sides"],
        "disc" => &["radius", "segments"],
        "icosphere" => &["radius", "subdivisions"],
        "rounded_box" => &["size", "radius", "segments"],
        "chamfered_box" => &["size", "radius"],
        "inset_box" => &["size", "face", "amount", "depth"],
        "wedge" => &["size"],
        "frustum" => &["bottom", "top", "height"],
        "tube" => &["outer", "inner", "height", "segments"],
        "hemisphere" => &["radius", "rings", "segments"],
        "half_cylinder" => &["radius", "height", "segments"],
        "torus_arc" => &["major", "minor", "arc", "major_segments", "minor_segments"],
        "ellipsoid" => &["size", "rings", "segments"],
        "superellipsoid" => &["size", "ew", "ns", "rings", "segments"],
        "curved_plane" => &["size", "bend_u", "bend_v", "segments_u", "segments_v"],
        "lathe" => &["profile", "segments", "cap_ends"],
        "spline_tube" => &[
            "points", "radius", "radii", "segments", "samples", "cap_ends",
        ],
        "spline_ribbon" => &["points", "width", "widths", "samples", "twist"],
        "coil" => &[
            "radius", "height", "turns", "profile_radius",
            "segments", "samples", "cap_ends", "handedness",
        ],
        "heightfield" => &[
            "size", "segments_u", "segments_v",
            "amplitude", "octaves", "frequency", "persistence", "seed",
        ],
        "bezier_patch" => &["points", "segments_u", "segments_v"],
        "metaball" => &["points", "radius", "radii", "blend", "rings", "segments"],
        // True SDF + surface-nets container. Children are implicit-field
        // primitives (sphere, ellipsoid, box, rounded_box, capsule, cylinder,
        // torus) combined with smooth-min, with optional `op=subtract`
        // children for smooth cavities. Distinct code path from `metaball`
        // (which only smooths sphere clusters) and `union(smooth=k)`
        // (vertex-fillet approximation of mesh CSG).
        "blob" => &["blend", "resolution"],
        "leaf_card" => &["size", "cards"],
        "extrude" => &["points", "hole", "height", "taper", "twist", "caps"],
        "sweep" => &[
            "profile", "path", "samples", "twist", "roll", "scale_along", "caps",
        ],
        "loft" => &["points", "heights", "samples", "caps"],
        "mesh" => &["src"],
        "branch" => &[
            "length", "radius", "depth", "splits", "length_falloff", "radius_falloff",
            "branch_angle", "roll", "tropism", "bend", "segments", "samples", "seed",
            "jitter", "leaves", "leaf_size", "leaf_cards", "leaf_aspect", "leaf_mat",
            "form", "leader_bias", "multi_stem",
        ],
        "building" => &[
            "seed", "style", "mat_style", "floor_area", "cellar_area", "rooms",
            "floors_above", "floors_below", "windows", "skylights", "roof",
            "ceiling_height", "door_w", "door_h", "window_w", "window_h",
            "wall_thickness", "ceiling_thickness", "entrances",
            "external_door", "internal_door",
            "window_small", "window_medium", "window_large", "skylight",
            "elevators", "staircases",
            "debug_hide_roof", "debug_render_floor",
        ],
        "room_type" => &["kind", "density", "mat", "min_area", "max_area"],
        "adjacency" => &["adjacent_to", "away_from"],
        "cave" => &[
            "seed", "mat_style", "size", "chambers", "levels",
            "chamber_min", "chamber_max", "spacing", "overlap", "chamber_flatten",
            "level_gap", "level_links",
            "passage_radius", "loops", "max_slope", "roughness", "blend",
            "margin", "resolution", "entrances", "water_mat", "lod_scale",
            "rock_piles", "pools", "lakes", "stalagmites", "stalactites",
            "columns", "mushrooms",
            "colliders", "water_collider",
            "debug_hide_shell", "debug_show_poi",
        ],
        "feature" => &["kind", "count", "min_size", "max_size", "mat"],
        "decal" => &[
            "size", "prompt", "image", "tint", "roughness", "offset",
            // Curved-surface shortcut: synthesizes a `conform` patch under
            // the hood so authors can stick a transparent image onto a
            // curved target without writing a separate `conform` node.
            "on", "at", "up", "lift",
        ],
        "material" => &[
            "color", "alpha", "metallic", "roughness",
            "normal_strength", "occlusion_strength",
            "alpha_mode", "alpha_cutoff",
            "emissive", "emissive_strength",
            "transmission",
            "double_sided",
            "uv_mode", "uv_scale",
            "shader",
            "base_color_texture", "metallic_roughness_texture",
            "normal_texture", "occlusion_texture", "emissive_texture",
            "gradient",
            "prompt",
        ],
        "connector" => &["at", "dir", "tag", "radius"],
        "mirror" => &["axis", "flip_bind"],
        "array" => &["count", "around", "start_angle"],
        "joint" => &["type", "axis", "limits", "pivot"],
        "clip" => &["seconds"],
        "track" => &["prop", "axis", "from", "to", "keys", "easing"],
        "spin" => &["target", "axis", "rpm", "easing"],
        "open_close" => &["target", "axis", "angle", "seconds", "easing"],
        "wave" | "flap" => &["target", "axis", "amplitude", "hz", "easing"],
        "idle" => &["target", "amplitude", "hz", "easing"],
        "skeleton" => &[],
        "bone" => &["envelope"],
        "attach" => &["parent", "child", "socket", "plug", "offset", "twist"],
        "conform" => &[
            "target", "child",
            // path mode
            "from", "to", "along", "width", "height", "samples", "twist",
            // patch mode
            "at", "up",
            // shared
            "lift", "reparent",
            // Reserved for future modes — accepted by the validator and
            // rejected with a clear message at lowering time.
            "direction", "curve", "via",
        ],
        "lod_scale" => &["value"],
        "if" => &["cond"],
        "else" => &[],
        "for" => &["var", "from", "to", "step"],
        "light" => &["kind", "color", "intensity", "range", "inner_cone", "outer_cone", "dir"],
        // `smooth` blends limb-to-torso seams for organic shapes.
        // `difference`/`intersect` reject it via attr_type below.
        "union" => &["smooth"],
        _ => &[],
    }
}

pub(super) fn attr_type(kind: &str, attr: &str) -> Option<&'static str> {
    let t = match (kind, attr) {
        ("meta", "name")
        | ("meta", "version")
        | ("meta", "mogen_version")
        | ("meta", "description")
        | ("meta", "seed")
        | ("meta", "thinking")
        | ("meta", "prompt")
        | ("meta", "style")
        | ("meta", "moghub_model_id")
        | ("meta", "moghub_slug")
        | ("meta", "moghub_version") => "string",
        ("meta", "tags") => "list of string",
        (_, "pos") | (_, "rot") => "vec3",
        (_, "scale") => "number or vec3",
        // Per-axis shortcuts and placement helpers are allowed everywhere.
        (_, "x") | (_, "y") | (_, "z")
        | (_, "rx") | (_, "ry") | (_, "rz")
        | (_, "w") | (_, "h") | (_, "d")
        | (_, "gap") => "number",
        (_, "anchor") => "string",
        (_, "above") | (_, "below") | (_, "left_of") | (_, "right_of")
        | (_, "in_front_of") | (_, "behind") => "string",
        // `track` uses from/to as scalar keyframe values; `conform`'s
        // from/to are connector names (strings); every other kind treats
        // `from`/`to` as AABB-corner vec3 shortcuts.
        ("track", "from") | ("track", "to") => "number",
        ("conform", "from") | ("conform", "to") => "string",
        // `for.from`/`to`/`step` are scalar bounds; carve them out before
        // the generic vec3 fallback below.
        ("for", "from") | ("for", "to") | ("for", "step") => "number",
        (_, "from") | (_, "to") => "vec3",
        // Deformation modifier types. `seed` overlaps with the meta-block's
        // string seed (handled above), so this arm only applies to geometry
        // primitives where the seed is an integer.
        (_, "seed")
        | (_, "noise")
        | (_, "jitter")
        | (_, "bend_x")
        | (_, "bend_y")
        | (_, "bend_z")
        | (_, "twist_y")
        | (_, "taper")
        | (_, "droop")
        | (_, "wave")
        | (_, "wave_frequency")
        | (_, "wave_phase")
        | (_, "faceted")
        | (_, "cast_shadow")
        | (_, "lod")
        | (_, "subdivide") => "number",
        // Blob-child operator; valid values checked at lowering time.
        (_, "op") => "string",
        ("chamfered_box", "radius") => "number",
        ("inset_box", "face") => "string",
        (_, "wave_axis") => "string",
        ("inset_box", "amount") | ("inset_box", "depth") => "number",
        // 2-element `[start, end]` ranges along the deformation's length axis.
        (_, "bend_x_range")
        | (_, "bend_y_range")
        | (_, "bend_z_range")
        | (_, "twist_y_range")
        | (_, "taper_range")
        | (_, "droop_range")
        | (_, "noise_range")
        | (_, "jitter_range")
        | (_, "wave_range") => "list",
        ("box", "size")
        | ("plane", "size")
        | ("quad", "size")
        | ("prism", "size")
        | ("rounded_box", "size")
        | ("chamfered_box", "size")
        | ("inset_box", "size")
        | ("wedge", "size")
        | ("ellipsoid", "size")
        | ("superellipsoid", "size")
        | ("slab", "size")
        | ("post", "size")
        | ("panel", "size")
        | ("wall", "size") => "number or vec3",
        ("wall", "holes") => "list",
        ("stack", "axis") | ("stack", "align") | ("stack", "pack") => "string",
        ("solid", "cleanup") => "string",
        ("grid", "count") | ("grid", "step") => "number or vec3",
        ("grid", "center") => "number",
        ("curved_plane", "size") => "list",
        ("frustum", "bottom") | ("frustum", "top") => "list",
        ("heightfield", "size") => "list",
        ("bezier_patch", "points") => "list",
        ("metaball", "points") => "list",
        ("metaball", "radii") => "list of number",
        ("lathe", "profile") => "list",
        ("spline_tube", "points") => "list",
        ("spline_tube", "radii") => "list",
        ("spline_ribbon", "points") => "list",
        ("spline_ribbon", "widths") => "list",
        ("extrude", "points") => "list",
        ("extrude", "hole") => "list",
        ("extrude", "height") | ("extrude", "twist") => "number",
        ("extrude", "caps") => "number",
        ("sweep", "profile") => "list",
        ("sweep", "path") => "list",
        ("sweep", "samples") | ("sweep", "twist") | ("sweep", "caps") => "number",
        ("sweep", "roll") | ("sweep", "scale_along") => "list",
        ("loft", "points") => "list",
        ("loft", "heights") => "list of number",
        ("loft", "samples") | ("loft", "caps") => "number",
        ("leaf_card", "size") => "number or vec3",
        ("leaf_card", "cards") => "number",
        ("decal", "size") => "list",
        ("decal", "prompt") | ("decal", "image") => "string",
        ("decal", "tint") => "vec3",
        ("decal", "roughness") | ("decal", "offset") => "number",
        ("decal", "on") | ("decal", "at") | ("decal", "up") => "string",
        ("decal", "lift") => "number",
        ("mesh", "src") => "string",
        ("branch", "length")
        | ("branch", "radius")
        | ("branch", "depth")
        | ("branch", "splits")
        | ("branch", "length_falloff")
        | ("branch", "radius_falloff")
        | ("branch", "branch_angle")
        | ("branch", "roll")
        | ("branch", "tropism")
        | ("branch", "bend")
        | ("branch", "segments")
        | ("branch", "samples")
        | ("branch", "leaves")
        | ("branch", "leaf_size")
        | ("branch", "leaf_cards")
        | ("branch", "leaf_aspect")
        | ("branch", "leader_bias")
        | ("branch", "multi_stem") => "number",
        ("branch", "leaf_mat") | ("branch", "form") => "string",
        ("building", "floor_area")
        | ("building", "cellar_area")
        | ("building", "rooms")
        | ("building", "floors_above")
        | ("building", "floors_below")
        | ("building", "windows")
        | ("building", "skylights")
        | ("building", "ceiling_height")
        | ("building", "door_w")
        | ("building", "door_h")
        | ("building", "window_w")
        | ("building", "window_h")
        | ("building", "wall_thickness")
        | ("building", "ceiling_thickness")
        | ("building", "entrances")
        | ("building", "elevators")
        | ("building", "staircases")
        | ("building", "debug_hide_roof")
        | ("building", "debug_render_floor") => "number",
        ("building", "style")
        | ("building", "mat_style")
        | ("building", "roof")
        | ("building", "external_door")
        | ("building", "internal_door")
        | ("building", "window_small")
        | ("building", "window_medium")
        | ("building", "window_large")
        | ("building", "skylight") => "string",
        ("room_type", "kind") => "string",
        ("room_type", "density")
        | ("room_type", "min_area")
        | ("room_type", "max_area") => "number",
        ("room_type", "mat") => "string",
        ("adjacency", "adjacent_to") | ("adjacency", "away_from") => "list of string",
        ("cave", "size") => "vec3",
        ("cave", "chambers")
        | ("cave", "levels")
        | ("cave", "chamber_min")
        | ("cave", "chamber_max")
        | ("cave", "spacing")
        | ("cave", "overlap")
        | ("cave", "chamber_flatten")
        | ("cave", "level_gap")
        | ("cave", "level_links")
        | ("cave", "passage_radius")
        | ("cave", "loops")
        | ("cave", "max_slope")
        | ("cave", "roughness")
        | ("cave", "blend")
        | ("cave", "margin")
        | ("cave", "resolution")
        | ("cave", "entrances")
        | ("cave", "rock_piles")
        | ("cave", "pools")
        | ("cave", "lakes")
        | ("cave", "stalagmites")
        | ("cave", "stalactites")
        | ("cave", "columns")
        | ("cave", "mushrooms")
        | ("cave", "lod_scale")
        | ("cave", "water_collider")
        | ("cave", "debug_hide_shell")
        | ("cave", "debug_show_poi") => "number",
        ("cave", "mat_style") | ("cave", "water_mat") | ("cave", "colliders") => "string",
        ("feature", "kind") | ("feature", "mat") => "string",
        ("feature", "count")
        | ("feature", "min_size")
        | ("feature", "max_size") => "number",
        ("cylinder", "radius")
        | ("cylinder", "height")
        | ("cylinder", "segments")
        | ("cone", "radius")
        | ("cone", "height")
        | ("cone", "segments")
        | ("sphere", "radius")
        | ("sphere", "rings")
        | ("sphere", "segments")
        | ("capsule", "radius")
        | ("capsule", "height")
        | ("capsule", "rings")
        | ("capsule", "segments")
        | ("torus", "major")
        | ("torus", "minor")
        | ("torus", "major_segments")
        | ("torus", "minor_segments")
        | ("pyramid", "radius")
        | ("pyramid", "height")
        | ("pyramid", "sides")
        | ("disc", "radius")
        | ("disc", "segments")
        | ("icosphere", "radius")
        | ("icosphere", "subdivisions")
        | ("rounded_box", "radius")
        | ("rounded_box", "segments")
        | ("tube", "outer")
        | ("tube", "inner")
        | ("tube", "height")
        | ("tube", "segments")
        | ("hemisphere", "radius")
        | ("hemisphere", "rings")
        | ("hemisphere", "segments")
        | ("half_cylinder", "radius")
        | ("half_cylinder", "height")
        | ("half_cylinder", "segments")
        | ("torus_arc", "major")
        | ("torus_arc", "minor")
        | ("torus_arc", "arc")
        | ("torus_arc", "major_segments")
        | ("torus_arc", "minor_segments")
        | ("ellipsoid", "rings")
        | ("ellipsoid", "segments")
        | ("superellipsoid", "ew")
        | ("superellipsoid", "ns")
        | ("superellipsoid", "rings")
        | ("superellipsoid", "segments")
        | ("curved_plane", "bend_u")
        | ("curved_plane", "bend_v")
        | ("curved_plane", "segments_u")
        | ("curved_plane", "segments_v")
        | ("lathe", "segments")
        | ("lathe", "cap_ends")
        | ("spline_tube", "radius")
        | ("spline_tube", "segments")
        | ("spline_tube", "samples")
        | ("spline_tube", "cap_ends")
        | ("spline_ribbon", "width")
        | ("spline_ribbon", "samples")
        | ("spline_ribbon", "twist")
        | ("coil", "radius")
        | ("coil", "height")
        | ("coil", "turns")
        | ("coil", "profile_radius")
        | ("coil", "segments")
        | ("coil", "samples")
        | ("coil", "cap_ends")
        | ("heightfield", "segments_u")
        | ("heightfield", "segments_v")
        | ("heightfield", "amplitude")
        | ("heightfield", "octaves")
        | ("heightfield", "frequency")
        | ("heightfield", "persistence")
        | ("bezier_patch", "segments_u")
        | ("bezier_patch", "segments_v")
        | ("metaball", "radius")
        | ("metaball", "blend")
        | ("metaball", "rings")
        | ("metaball", "segments")
        | ("blob", "blend")
        | ("blob", "resolution")
        | ("frustum", "height")
        | ("union", "smooth")
        | ("material", "alpha")
        | ("material", "metallic")
        | ("material", "roughness")
        | ("material", "normal_strength")
        | ("material", "occlusion_strength")
        | ("material", "alpha_cutoff")
        | ("material", "emissive_strength")
        | ("material", "transmission")
        | ("material", "double_sided") => "number",
        ("coil", "handedness") => "string",
        ("material", "color") | ("material", "emissive") => "vec3",
        ("material", "alpha_mode") | ("material", "uv_mode") | ("material", "shader") => "string",
        ("material", "uv_scale") => "number or vec2",
        ("material", "base_color_texture")
        | ("material", "metallic_roughness_texture")
        | ("material", "normal_texture")
        | ("material", "occlusion_texture")
        | ("material", "emissive_texture")
        | ("material", "prompt") => "string",
        ("material", "gradient") => "gradient",
        ("connector", "at") | ("connector", "dir") => "vec3",
        ("connector", "tag") => "string",
        ("connector", "radius") => "number",
        ("mirror", "axis") => "string",
        ("mirror", "flip_bind") => "number",
        ("array", "count") | ("array", "start_angle") => "number",
        ("array", "around") => "string",
        ("joint", "type") | ("joint", "pivot") => "string",
        ("joint", "axis") => "vec3",
        ("joint", "limits") => "list",
        ("clip", "seconds") => "number",
        ("track", "prop") => "string",
        ("track", "axis") => "vec3",
        ("track", "keys") => "list",
        ("spin", "target") | ("open_close", "target") | ("wave", "target") | ("flap", "target")
        | ("idle", "target") => "string",
        ("spin", "axis") | ("open_close", "axis") | ("wave", "axis") | ("flap", "axis") => "vec3",
        ("spin", "rpm")
        | ("open_close", "angle")
        | ("open_close", "seconds")
        | ("wave", "amplitude")
        | ("wave", "hz")
        | ("flap", "amplitude")
        | ("flap", "hz")
        | ("idle", "amplitude")
        | ("idle", "hz") => "number",
        ("track", "easing")
        | ("spin", "easing")
        | ("open_close", "easing")
        | ("wave", "easing")
        | ("flap", "easing")
        | ("idle", "easing") => "string",
        ("bone", "envelope") => "number",
        ("attach", "parent") | ("attach", "child")
        | ("attach", "socket") | ("attach", "plug") => "string",
        ("attach", "offset") | ("attach", "twist") => "number",
        // `conform`'s `from`/`to` are handled above (string, before the
        // generic vec3 fallback). Remaining conform attrs:
        ("conform", "target") | ("conform", "child")
        | ("conform", "along") | ("conform", "width") | ("conform", "height")
        | ("conform", "at") | ("conform", "up")
        | ("conform", "curve") => "string",
        ("conform", "lift") | ("conform", "samples")
        | ("conform", "twist") | ("conform", "reparent") => "number",
        ("conform", "direction") => "vec3",
        // `via=[c1, c2]` reserved for future multi-segment paths.
        ("conform", "via") => "list",
        ("lod_scale", "value") => "number",
        // Module control flow attrs. `if.cond` is numeric (truthy ≠ 0);
        // `for.var` names the binding and accepts ident-or-string;
        // `for.from`/`to`/`step` are the bounds.
        ("if", "cond") => "number",
        ("for", "var") => "string",
        // (`for.from`/`to`/`step` handled above the generic vec3 arm.)
        ("light", "kind") => "string",
        ("light", "color") | ("light", "dir") => "vec3",
        ("light", "intensity")
        | ("light", "range")
        | ("light", "inner_cone")
        | ("light", "outer_cone") => "number",
        (_, "mat") | (_, "role") | (_, "skin") | (_, "bind") => "string",
        (_, "tags") => "string",
        (_, "collider") => "string",
        _ => return None,
    };
    Some(t)
}

pub(super) fn value_matches(v: &Value, expected: &str) -> bool {
    match (v, expected) {
        (Value::Number(_), "number") => true,
        (Value::Vec3(_), "vec3") => true,
        (Value::Number(_) | Value::Vec3(_), "number or vec3") => true,
        (Value::Number(_), "number or vec2") => true,
        (Value::List(v), "number or vec2") if v.len() == 2 => true,
        (Value::ListExpr(v), "number or vec2") if v.len() == 2 => true,
        (Value::String(_), "string") => true,
        (Value::Ident(_), "string") => true,
        (Value::List(_) | Value::ListExpr(_) | Value::ListVec3(_) | Value::ListPair(_) | Value::ListQuad(_), "list") => true,
        // `list of number` attrs accept Vec3 as a 3-number list (the
        // grammar prefers `vec3` over `list` for exactly 3 components, so
        // `loft.heights=[0, 1, 2]` enters the type system as a Vec3 even
        // though the schema declares it a list).
        (Value::List(_) | Value::ListExpr(_), "list of number") => true,
        (Value::Vec3(_) | Value::Vec3Expr(_), "list of number") => true,
        (Value::ListString(_), "list of string") => true,
        (Value::String(_) | Value::Ident(_), "list of string") => true,
        // Deferred expressions: accept as their natural type; evaluation errors
        // (unbound params, etc.) are reported during module expansion.
        (Value::Expr(_), "number") => true,
        (Value::Expr(_), "number or vec3") => true,
        (Value::Expr(_), "number or vec2") => true,
        (Value::Vec3Expr(_), "vec3") => true,
        (Value::Vec3Expr(_), "number or vec3") => true,
        (Value::Gradient(_), "gradient") => true,
        _ => false,
    }
}

pub(super) fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "number",
        Value::Vec3(_) => "vec3",
        Value::String(_) => "string",
        Value::Ident(_) => "ident",
        Value::Expr(_) => "expression",
        Value::Vec3Expr(_) => "vec3 expression",
        Value::List(_) => "list",
        Value::ListExpr(_) => "list expression",
        Value::ListVec3(_) => "list of vec3",
        Value::ListPair(_) => "list of pair",
        Value::ListQuad(_) => "list of quad",
        Value::ListString(_) => "list of string",
        Value::Gradient(_) => "gradient",
    }
}

pub(super) fn as_string_or_ident(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) | Value::Ident(s) => Some(s.as_str()),
        _ => None,
    }
}
