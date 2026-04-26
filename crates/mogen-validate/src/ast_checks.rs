use std::collections::{HashMap, HashSet};

use mogen_core::Diagnostic;
use mogen_dsl::ast::{Node, Value};

pub const KNOWN_KINDS: &[&str] = &[
    "scene", "group", "solid", "material", "connector", "attach", "mirror", "array",
    "stack", "grid",
    "box", "plane", "quad", "cylinder", "cone", "sphere", "capsule", "torus",
    "prism", "pyramid", "disc", "icosphere", "rounded_box",
    "wedge", "frustum", "tube", "hemisphere", "half_cylinder", "torus_arc", "ellipsoid",
    "superellipsoid", "curved_plane", "lathe", "spline_tube", "leaf_card",
    "slab", "post", "panel", "wall",
    "branch",
    "module", "use",
    "union", "difference", "intersect",
    "joint", "clip", "track",
    "spin", "open_close", "wave", "flap", "idle",
    "skeleton", "bone",
    "lod_scale",
];

/// Attribute names accepted on any geometry/group-like node (primitives,
/// `group`, `solid`, `stack`, `grid`, `mirror`, `array`, CSG, `use`, `module`).
/// Covers transforms, material binding, role/tags metadata, and the placement
/// shortcuts (`x`/`y`/`z`, `anchor`, `from`/`to` corners, sibling relations).
pub const GEOMETRY_COMMON_ATTRS: &[&str] = &[
    "pos", "rot", "scale", "role", "tags", "mat", "skin",
    // Per-component shortcuts.
    "x", "y", "z", "rx", "ry", "rz", "w", "h", "d",
    // Placement ergonomics.
    "anchor", "from", "to", "gap",
    "above", "below", "left_of", "right_of", "in_front_of", "behind",
];

/// Transform-only subset. Used by `skeleton` (places the whole rig) and `bone`
/// (parent-relative offset) — they carry positions but none of the placement
/// shortcuts or material bindings.
pub const TRANSFORM_COMMON_ATTRS: &[&str] = &[
    "pos", "rot", "scale",
    "x", "y", "z", "rx", "ry", "rz",
];

/// Per-kind common-attribute bucket. Kinds that are neither geometry nor
/// transform-bearing (materials, connectors, joins, animation tracks and
/// templates) accept ONLY their kind-specific allowlist — no implicit
/// `pos=`/`from=`/`anchor=`. This is what stops mistakes like `from=[1,0,1]`
/// on `open_close` from being silently accepted.
pub fn common_attrs_for_kind(kind: &str) -> &'static [&'static str] {
    match kind {
        "skeleton" | "bone" => TRANSFORM_COMMON_ATTRS,
        "material" | "connector" | "attach"
        | "joint" | "clip" | "track"
        | "spin" | "open_close" | "wave" | "flap" | "idle"
        | "lod_scale" => &[],
        _ => GEOMETRY_COMMON_ATTRS,
    }
}

pub fn validate_ast(ast: &[Node]) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let materials = collect_material_names(ast, &mut diags);
    let modules = collect_module_names(ast, &mut diags);
    for n in ast {
        walk(n, &materials, &modules, &mut diags);
    }
    diags
}

fn collect_material_names(ast: &[Node], diags: &mut Vec<Diagnostic>) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut names = HashSet::new();
    let mut visit = |n: &Node, diags: &mut Vec<Diagnostic>| {
        if n.kind != "material" {
            return;
        }
        match &n.name {
            None => {
                diags.push(
                    Diagnostic::error("E0201", "material declaration requires a name")
                        .with_span(n.span),
                );
            }
            Some(name) => {
                if seen.contains_key(name) {
                    diags.push(
                        Diagnostic::warning(
                            "W0202",
                            format!("duplicate material name \"{name}\""),
                        )
                        .with_span(n.span),
                    );
                } else {
                    seen.insert(name.clone(), 0);
                    names.insert(name.clone());
                }
            }
        }
    };
    for n in ast {
        visit(n, diags);
        if n.kind == "scene" {
            for c in &n.children {
                visit(c, diags);
            }
        }
    }
    names
}

fn collect_module_names(ast: &[Node], diags: &mut Vec<Diagnostic>) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut names = HashSet::new();
    // Seed with stdlib modules so `use "humanoid_torso" (...)` validates
    // without the user needing to redeclare them. lower() merges these
    // into the live registry too — keep the two in sync.
    for name in mogen_dsl::stdlib_registry().names() {
        names.insert(name.clone());
    }
    for n in ast {
        if n.kind != "module" {
            continue;
        }
        match &n.name {
            None => {
                diags.push(
                    Diagnostic::error("E0301", "module declaration requires a name")
                        .with_span(n.span),
                );
            }
            Some(name) => {
                if seen.contains_key(name) {
                    diags.push(
                        Diagnostic::error(
                            "E0302",
                            format!("duplicate module declaration \"{name}\""),
                        )
                        .with_span(n.span),
                    );
                } else {
                    seen.insert(name.clone(), 0);
                    names.insert(name.clone());
                }
            }
        }
    }
    names
}

fn walk(
    n: &Node,
    materials: &HashSet<String>,
    modules: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    check_kind(n, diags);

    match n.kind.as_str() {
        // `module` and `use` carry user-defined attr names (params/args); skip the
        // closed attr vocabulary check. We still validate specific constraints below.
        "module" => {
            // Parameter defaults must be numeric scalars; the collector enforces this,
            // but we also reject e.g. `module "leg" (color="red")` here with a clearer
            // diagnostic than lowering would produce.
            for (k, v) in &n.attrs {
                if !matches!(v, Value::Number(_) | Value::Expr(_)) {
                    diags.push(
                        Diagnostic::error(
                            "E0303",
                            format!(
                                "module parameter `{}` default must be a number or expression",
                                k
                            ),
                        )
                        .with_span(n.span),
                    );
                }
            }
        }
        "use" => {
            if let Some(name) = &n.name {
                if !modules.contains(name) {
                    diags.push(
                        Diagnostic::error(
                            "E0304",
                            format!("unknown module \"{}\"", name),
                        )
                        .with_span(n.span),
                    );
                }
            } else {
                diags.push(
                    Diagnostic::error(
                        "E0305",
                        "`use` requires a module name, e.g. `use \"leg\" (...)`",
                    )
                    .with_span(n.span),
                );
            }
        }
        _ if KNOWN_KINDS.contains(&n.kind.as_str()) => {
            check_attrs(n, materials, diags);
            check_anim_required(n, diags);
        }
        _ => {}
    }

    for c in &n.children {
        walk(c, materials, modules, diags);
    }
}

fn check_kind(n: &Node, diags: &mut Vec<Diagnostic>) {
    if !KNOWN_KINDS.contains(&n.kind.as_str()) {
        diags.push(
            Diagnostic::error(
                "E0101",
                format!("unknown node kind \"{}\"", n.kind),
            )
            .with_span(n.kind_span),
        );
    }
}

fn check_attrs(n: &Node, materials: &HashSet<String>, diags: &mut Vec<Diagnostic>) {
    let allowed = attrs_for_kind(&n.kind);
    let common = common_attrs_for_kind(&n.kind);
    for (k, v) in &n.attrs {
        if !allowed.contains(&k.as_str()) && !common.contains(&k.as_str()) {
            diags.push(
                Diagnostic::warning(
                    "W0102",
                    format!("attribute \"{}\" is not used by `{}`", k, n.kind),
                )
                .with_span(n.span),
            );
            continue;
        }
        if let Some(expected) = attr_type(&n.kind, k) {
            if !value_matches(v, expected) {
                diags.push(
                    Diagnostic::error(
                        "E0103",
                        format!(
                            "attribute \"{}\" on `{}` expects {}, got {}",
                            k,
                            n.kind,
                            expected,
                            value_kind(v)
                        ),
                    )
                    .with_span(n.span),
                );
            }
        }
        if k == "mat" {
            if let Some(name) = as_string_or_ident(v) {
                if !materials.contains(name) {
                    diags.push(
                        Diagnostic::error(
                            "E0104",
                            format!("unknown material \"{}\"", name),
                        )
                        .with_span(n.span),
                    );
                }
            }
        }
    }
}

pub fn attrs_for_kind(kind: &str) -> &'static [&'static str] {
    match kind {
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
        "leaf_card" => &["size", "cards"],
        "branch" => &[
            "length", "radius", "depth", "splits", "length_falloff", "radius_falloff",
            "branch_angle", "roll", "tropism", "bend", "segments", "samples", "seed",
            "jitter", "leaves", "leaf_size", "leaf_cards", "leaf_mat",
        ],
        "material" => &[
            "color", "alpha", "metallic", "roughness",
            "normal_strength", "occlusion_strength",
            "alpha_mode", "alpha_cutoff",
            "emissive", "emissive_strength",
            "transmission",
            "double_sided",
            "uv_mode", "uv_scale",
            "base_color_texture", "metallic_roughness_texture",
            "normal_texture", "occlusion_texture", "emissive_texture",
        ],
        "connector" => &["at", "dir", "tag", "radius"],
        "mirror" => &["axis"],
        "array" => &["count", "around", "start_angle"],
        "joint" => &["type", "axis", "limits", "pivot"],
        "clip" => &["seconds"],
        "track" => &["prop", "axis", "from", "to", "keys"],
        "spin" => &["target", "axis", "rpm"],
        "open_close" => &["target", "axis", "angle", "seconds"],
        "wave" | "flap" => &["target", "axis", "amplitude", "hz"],
        "idle" => &["target", "amplitude", "hz"],
        "skeleton" => &[],
        "bone" => &["envelope"],
        "attach" => &["parent", "child", "socket", "plug", "offset", "twist"],
        "lod_scale" => &["value"],
        // `smooth` blends limb-to-torso seams for organic shapes.
        // `difference`/`intersect` reject it via attr_type below.
        "union" => &["smooth"],
        _ => &[],
    }
}

fn attr_type(kind: &str, attr: &str) -> Option<&'static str> {
    let t = match (kind, attr) {
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
        // `track` uses from/to as scalar keyframe values; every other kind
        // treats `from`/`to` as AABB-corner vec3 shortcuts.
        ("track", "from") | ("track", "to") => "number",
        (_, "from") | (_, "to") => "vec3",
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
        ("leaf_card", "size") => "number or vec3",
        ("leaf_card", "cards") => "number",
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
        | ("branch", "seed")
        | ("branch", "jitter")
        | ("branch", "leaves")
        | ("branch", "leaf_size")
        | ("branch", "leaf_cards") => "number",
        ("branch", "leaf_mat") => "string",
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
        ("material", "alpha_mode") | ("material", "uv_mode") => "string",
        ("material", "uv_scale") => "number or vec2",
        ("material", "base_color_texture")
        | ("material", "metallic_roughness_texture")
        | ("material", "normal_texture")
        | ("material", "occlusion_texture")
        | ("material", "emissive_texture") => "string",
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
        ("bone", "envelope") => "number",
        ("attach", "parent") | ("attach", "child")
        | ("attach", "socket") | ("attach", "plug") => "string",
        ("attach", "offset") | ("attach", "twist") => "number",
        ("lod_scale", "value") => "number",
        (_, "mat") | (_, "role") | (_, "skin") => "string",
        (_, "tags") => "string",
        _ => return None,
    };
    Some(t)
}

fn value_matches(v: &Value, expected: &str) -> bool {
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

fn value_kind(v: &Value) -> &'static str {
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
    }
}

fn as_string_or_ident(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) | Value::Ident(s) => Some(s.as_str()),
        _ => None,
    }
}

fn check_anim_required(n: &Node, diags: &mut Vec<Diagnostic>) {
    match n.kind.as_str() {
        "material" => {
            if let Some(name) = n.attr("alpha_mode").and_then(as_string_or_ident) {
                if !matches!(name, "opaque" | "mask" | "blend") {
                    diags.push(
                        Diagnostic::error(
                            "E0203",
                            format!(
                                "alpha_mode must be \"opaque\", \"mask\", or \"blend\"; got \"{name}\""
                            ),
                        )
                        .with_span(n.span),
                    );
                }
            }
            if let Some(name) = n.attr("uv_mode").and_then(as_string_or_ident) {
                if !matches!(name, "tile" | "fit") {
                    diags.push(
                        Diagnostic::error(
                            "E0206",
                            format!(
                                "uv_mode must be \"tile\" or \"fit\"; got \"{name}\""
                            ),
                        )
                        .with_span(n.span),
                    );
                }
            }
            if let Some(Value::Number(t)) = n.attr("transmission") {
                if !(0.0..=1.0).contains(t) {
                    diags.push(
                        Diagnostic::warning(
                            "W0204",
                            format!("transmission {t} is outside the [0,1] range"),
                        )
                        .with_span(n.span),
                    );
                }
            }
            if let Some(Value::Number(s)) = n.attr("emissive_strength") {
                if *s < 0.0 {
                    diags.push(
                        Diagnostic::warning(
                            "W0205",
                            format!("emissive_strength {s} is negative — clamped to 0 by most renderers"),
                        )
                        .with_span(n.span),
                    );
                }
            }
        }
        "joint" => {
            if n.name.is_none() {
                diags.push(
                    Diagnostic::error("E0401", "joint declaration requires a name")
                        .with_span(n.span),
                );
            }
            if n.attr("type").is_none() {
                diags.push(
                    Diagnostic::error("E0402", "joint requires type=hinge|slider|ball|rotor")
                        .with_span(n.span),
                );
            } else if let Some(name) = n.attr("type").and_then(as_string_or_ident) {
                if !matches!(name, "hinge" | "slider" | "ball" | "rotor") {
                    diags.push(
                        Diagnostic::error(
                            "E0402",
                            format!("unknown joint type `{name}`"),
                        )
                        .with_span(n.span),
                    );
                }
            }
            if n.attr("pivot").is_none() {
                diags.push(
                    Diagnostic::error("E0403", "joint requires pivot=\"<node_name>\"")
                        .with_span(n.span),
                );
            }
            if let Some(Value::List(v)) = n.attr("limits") {
                if v.len() != 2 {
                    diags.push(
                        Diagnostic::error(
                            "E0404",
                            format!("joint limits must be a 2-element list, got {}", v.len()),
                        )
                        .with_span(n.span),
                    );
                }
            }
        }
        "clip" => {
            if n.name.is_none() {
                diags.push(
                    Diagnostic::error("E0411", "clip declaration requires a name")
                        .with_span(n.span),
                );
            }
            for c in &n.children {
                if c.kind != "track" {
                    diags.push(
                        Diagnostic::error(
                            "E0412",
                            format!("clip children must be `track` nodes, got `{}`", c.kind),
                        )
                        .with_span(c.span),
                    );
                }
            }
        }
        "track" => {
            if n.name.is_none() {
                diags.push(
                    Diagnostic::error(
                        "E0413",
                        "track requires a target name (joint or node)",
                    )
                    .with_span(n.span),
                );
            }
            if n.attr("to").is_none() && n.attr("keys").is_none() {
                diags.push(
                    Diagnostic::error(
                        "E0414",
                        "track requires either `to=` scalar or `keys=[[t,v], ...]`",
                    )
                    .with_span(n.span),
                );
            }
        }
        "spin" | "open_close" | "wave" | "flap" | "idle" => {
            if n.attr("target").is_none() {
                diags.push(
                    Diagnostic::error(
                        "E0421",
                        format!("`{}` requires target=\"<name>\"", n.kind),
                    )
                    .with_span(n.span),
                );
            }
        }
        "skeleton" => {
            if n.name.is_none() {
                diags.push(
                    Diagnostic::error("E0501", "skeleton declaration requires a name")
                        .with_span(n.span),
                );
            }
            if n.children.is_empty() {
                diags.push(
                    Diagnostic::error("E0502", "skeleton must contain at least one bone")
                        .with_span(n.span),
                );
            }
            for c in &n.children {
                if c.kind != "bone" {
                    diags.push(
                        Diagnostic::error(
                            "E0503",
                            format!("skeleton children must be `bone` nodes, got `{}`", c.kind),
                        )
                        .with_span(c.span),
                    );
                }
            }
        }
        "bone" => {
            if n.name.is_none() {
                diags.push(
                    Diagnostic::error("E0504", "bone declaration requires a name")
                        .with_span(n.span),
                );
            }
            for c in &n.children {
                if c.kind != "bone" {
                    diags.push(
                        Diagnostic::error(
                            "E0505",
                            format!("bone children must be `bone` nodes, got `{}`", c.kind),
                        )
                        .with_span(c.span),
                    );
                }
            }
        }
        "solid" => {
            if let Some(name) = n.attr("cleanup").and_then(as_string_or_ident) {
                if !matches!(name, "coplanar" | "none") {
                    diags.push(
                        Diagnostic::error(
                            "E0701",
                            format!(
                                "solid cleanup must be \"coplanar\" or \"none\"; got \"{name}\""
                            ),
                        )
                        .with_span(n.span),
                    );
                }
            }
        }
        "attach" => {
            if n.attr("parent").and_then(as_string_or_ident).is_none() {
                diags.push(
                    Diagnostic::error("E0601", "attach requires parent=\"<node name>\"")
                        .with_span(n.span),
                );
            }
            if n.attr("child").and_then(as_string_or_ident).is_none() {
                diags.push(
                    Diagnostic::error("E0602", "attach requires child=\"<node name>\"")
                        .with_span(n.span),
                );
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod common_attr_scope_tests {
    use super::*;
    use mogen_core::Severity;

    fn diags_for(src: &str) -> Vec<Diagnostic> {
        let ast = mogen_dsl::parse(src).expect("parse");
        validate_ast(&ast)
    }

    fn has_unknown_attr(diags: &[Diagnostic], attr: &str, kind: &str) -> bool {
        let needle = format!("attribute \"{attr}\" is not used by `{kind}`");
        diags.iter().any(|d| d.code == "W0102" && d.message == needle)
    }

    #[test]
    fn placement_shortcuts_are_rejected_on_animation_templates() {
        // This is the original bug: `from=[1,0,1]` was silently accepted on
        // `open_close` because it lived in the old blanket COMMON_ATTRS.
        let src = r#"
            material "wood" (color=[0.5, 0.3, 0.1])
            scene { box "lid" (size=[1,0.1,1], mat="wood") }
            open_close "swing" (target="lid", from=[1,0,1], axis=[1,0,0], angle=90, seconds=1.0)
        "#;
        let diags = diags_for(src);
        assert!(
            has_unknown_attr(&diags, "from", "open_close"),
            "expected W0102 for from= on open_close, got {diags:?}"
        );
    }

    #[test]
    fn placement_shortcuts_are_rejected_on_attach_joint_material() {
        // Attach, joint, clip, track, material: no implicit transforms or
        // placement shortcuts — only their kind-specific allowlist.
        let src = r#"
            material "wood" (color=[0.5, 0.3, 0.1], pos=[0,0,0])
            scene {
              box "a" (size=[1,1,1], mat="wood")
              box "b" (size=[1,1,1], mat="wood")
            }
            attach (parent="a", child="b", pos=[0,0,0])
            joint "j" (type=hinge, pivot="a", anchor="top")
        "#;
        let diags = diags_for(src);
        assert!(has_unknown_attr(&diags, "pos", "material"));
        assert!(has_unknown_attr(&diags, "pos", "attach"));
        assert!(has_unknown_attr(&diags, "anchor", "joint"));
    }

    #[test]
    fn geometry_still_accepts_common_attrs() {
        // Regression guard: the split must NOT reject legitimate uses of
        // placement shortcuts on primitives.
        let src = r#"
            material "wood" (color=[0.5, 0.3, 0.1])
            scene {
              box "a" (size=[1,1,1], mat="wood", pos=[0,0,0], anchor="bottom", tags="floating")
              slab "b" (size=[1,0.1,1], mat="wood", above="a", gap=0.05)
            }
        "#;
        let diags = diags_for(src);
        let warnings: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Warning))
            .collect();
        assert!(
            warnings.is_empty(),
            "expected no attr warnings on valid geometry, got {warnings:?}"
        );
    }

    #[test]
    fn bones_still_accept_transform_attrs_but_not_placement() {
        // Bones legitimately use pos= (parent-relative offset). They must NOT
        // accept anchor/from/above etc — those are meaningless on joints.
        let ok_src = r#"
            scene {
              skeleton "rig" {
                bone "root" (pos=[0, 1, 0]) {
                  bone "child" (pos=[0, 0.5, 0], envelope=0.2)
                }
              }
            }
        "#;
        let diags = diags_for(ok_src);
        let warns: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Warning))
            .collect();
        assert!(warns.is_empty(), "valid bone attrs should not warn: {warns:?}");

        let bad_src = r#"
            scene {
              skeleton "rig" {
                bone "root" (pos=[0, 1, 0], anchor="bottom") {
                  bone "child" (pos=[0, 0.5, 0], tags="foo")
                }
              }
            }
        "#;
        let diags = diags_for(bad_src);
        assert!(has_unknown_attr(&diags, "anchor", "bone"));
        assert!(has_unknown_attr(&diags, "tags", "bone"));
    }

    #[test]
    fn track_from_to_still_accepted() {
        // `track` has its own `from`/`to` (scalar keyframe values) in its
        // kind-specific allowlist — the split must not break these.
        let src = r#"
            scene {
              group "door" { box "panel" (size=[1, 2, 0.1]) }
            }
            joint "h" (type=hinge, pivot="door", axis=[0,1,0])
            clip "swing" (seconds=1.0) {
              track "h" (from=0, to=90)
            }
        "#;
        let diags = diags_for(src);
        let warns: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Warning))
            .collect();
        assert!(warns.is_empty(), "track from/to must pass: {warns:?}");
    }
}
