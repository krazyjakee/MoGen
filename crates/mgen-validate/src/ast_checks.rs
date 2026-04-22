use std::collections::{HashMap, HashSet};

use mgen_core::Diagnostic;
use mgen_dsl::ast::{Node, Value};

const KNOWN_KINDS: &[&str] = &[
    "scene", "group", "material", "connector", "attach", "mirror", "array",
    "box", "plane", "quad", "cylinder", "cone", "sphere", "capsule", "torus",
    "prism", "pyramid", "disc", "icosphere", "rounded_box",
    "wedge", "frustum", "tube", "hemisphere", "half_cylinder", "torus_arc", "ellipsoid",
    "superellipsoid", "curved_plane", "lathe", "spline_tube",
    "module", "use",
    "union", "difference", "intersect",
    "joint", "clip", "track",
    "spin", "open_close", "wave", "flap", "idle",
    "skeleton", "bone",
];

/// Attribute names that every kind is allowed to carry (transforms + metadata).
const COMMON_ATTRS: &[&str] = &["pos", "rot", "scale", "role", "tags", "mat", "skin"];

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
    for (k, v) in &n.attrs {
        if !allowed.contains(&k.as_str()) && !COMMON_ATTRS.contains(&k.as_str()) {
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

fn attrs_for_kind(kind: &str) -> &'static [&'static str] {
    match kind {
        "box" | "plane" | "quad" | "prism" => &["size"],
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
        "material" => &[
            "color", "alpha", "metallic", "roughness",
            "alpha_mode", "alpha_cutoff",
            "emissive", "emissive_strength",
            "transmission",
            "double_sided",
        ],
        "connector" => &["at", "dir", "tag", "radius"],
        "mirror" => &["axis"],
        "array" => &["count", "around", "start_angle"],
        "joint" => &["type", "axis", "limits", "pivot"],
        "clip" => &["seconds"],
        "track" => &["prop", "from", "to"],
        "spin" => &["target", "axis", "rpm"],
        "open_close" => &["target", "axis", "angle", "seconds"],
        "wave" | "flap" => &["target", "axis", "amplitude", "hz"],
        "idle" => &["target", "amplitude", "hz"],
        "skeleton" => &[],
        "bone" => &["envelope"],
        "attach" => &["parent", "child", "socket", "plug", "offset", "twist"],
        _ => &[],
    }
}

fn attr_type(kind: &str, attr: &str) -> Option<&'static str> {
    let t = match (kind, attr) {
        (_, "pos") | (_, "rot") => "vec3",
        (_, "scale") => "number or vec3",
        ("box", "size")
        | ("plane", "size")
        | ("quad", "size")
        | ("prism", "size")
        | ("rounded_box", "size")
        | ("wedge", "size")
        | ("ellipsoid", "size")
        | ("superellipsoid", "size") => "vec3",
        ("curved_plane", "size") => "list",
        ("frustum", "bottom") | ("frustum", "top") => "list",
        ("lathe", "profile") => "list",
        ("spline_tube", "points") => "list",
        ("spline_tube", "radii") => "list",
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
        | ("material", "alpha")
        | ("material", "metallic")
        | ("material", "roughness")
        | ("material", "alpha_cutoff")
        | ("material", "emissive_strength")
        | ("material", "transmission")
        | ("material", "double_sided") => "number",
        ("material", "color") | ("material", "emissive") => "vec3",
        ("material", "alpha_mode") => "string",
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
        ("track", "from") | ("track", "to") => "number",
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
        (Value::String(_), "string") => true,
        (Value::Ident(_), "string") => true,
        (Value::List(_) | Value::ListExpr(_) | Value::ListVec3(_) | Value::ListPair(_), "list") => true,
        // Deferred expressions: accept as their natural type; evaluation errors
        // (unbound params, etc.) are reported during module expansion.
        (Value::Expr(_), "number") => true,
        (Value::Expr(_), "number or vec3") => true,
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
            if n.attr("to").is_none() {
                diags.push(
                    Diagnostic::error("E0414", "track requires `to=` scalar")
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
