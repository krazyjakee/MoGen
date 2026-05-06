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
    "prism", "pyramid", "disc", "icosphere", "rounded_box",
    "wedge", "frustum", "tube", "hemisphere", "half_cylinder", "torus_arc", "ellipsoid",
    "superellipsoid", "curved_plane", "lathe", "spline_tube", "spline_ribbon", "leaf_card", "mesh",
    "slab", "post", "panel", "wall",
    "branch",
    "decal",
    "module", "use", "import",
    "union", "difference", "intersect",
    "joint", "clip", "track",
    "spin", "open_close", "wave", "flap", "idle",
    "skeleton", "bone",
    "lod_scale",
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
    // modifiers (`noise`, `jitter`) are seeded by `seed`.
    "seed", "noise", "jitter", "bend_x", "bend_y", "bend_z", "twist_y",
    "taper", "droop", "faceted",
    // Per-node LOD multiplier — compounds with the file-global `lod_scale`
    // for the duration of this node and its subtree (see lod.rs guards).
    "lod",
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
        | "lod_scale" | "meta" => &[],
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
        "leaf_card" => &["size", "cards"],
        "mesh" => &["src"],
        "branch" => &[
            "length", "radius", "depth", "splits", "length_falloff", "radius_falloff",
            "branch_angle", "roll", "tropism", "bend", "segments", "samples", "seed",
            "jitter", "leaves", "leaf_size", "leaf_cards", "leaf_aspect", "leaf_mat",
            "form", "leader_bias", "multi_stem",
        ],
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
            "prompt",
        ],
        "connector" => &["at", "dir", "tag", "radius"],
        "mirror" => &["axis"],
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
        | (_, "faceted")
        | (_, "cast_shadow")
        | (_, "lod") => "number",
        ("box", "size")
        | ("plane", "size")
        | ("quad", "size")
        | ("prism", "size")
        | ("rounded_box", "size")
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
        ("lathe", "profile") => "list",
        ("spline_tube", "points") => "list",
        ("spline_tube", "radii") => "list",
        ("spline_ribbon", "points") => "list",
        ("spline_ribbon", "widths") => "list",
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
        ("material", "color") | ("material", "emissive") => "vec3",
        ("material", "alpha_mode") | ("material", "uv_mode") | ("material", "shader") => "string",
        ("material", "uv_scale") => "number or vec2",
        ("material", "base_color_texture")
        | ("material", "metallic_roughness_texture")
        | ("material", "normal_texture")
        | ("material", "occlusion_texture")
        | ("material", "emissive_texture")
        | ("material", "prompt") => "string",
        ("connector", "at") | ("connector", "dir") => "vec3",
        ("connector", "tag") => "string",
        ("connector", "radius") => "number",
        ("mirror", "axis") => "string",
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
        (Value::ListString(_), "list of string") => true,
        (Value::String(_) | Value::Ident(_), "list of string") => true,
        // Deferred expressions: accept as their natural type; evaluation errors
        // (unbound params, etc.) are reported during module expansion.
        (Value::Expr(_), "number") => true,
        (Value::Expr(_), "number or vec3") => true,
        (Value::Expr(_), "number or vec2") => true,
        (Value::Vec3Expr(_), "vec3") => true,
        (Value::Vec3Expr(_), "number or vec3") => true,
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
    }
}

pub(super) fn as_string_or_ident(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) | Value::Ident(s) => Some(s.as_str()),
        _ => None,
    }
}
