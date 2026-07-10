//! Per-kind constraint checks that run on top of the generic attribute
//! schema. These cover required attributes, value-domain checks (e.g.
//! `light kind` must be one of three strings), and structural rules
//! (e.g. `clip` children must be `track`s, `bone` children must be `bone`s).

use mogen_core::Diagnostic;
use mogen_dsl::ast::{Node, Value};

use super::schema::as_string_or_ident;

pub(super) fn check_anim_required(n: &Node, diags: &mut Vec<Diagnostic>) {
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
        "physics" => {
            if n.name.is_none() {
                diags.push(
                    Diagnostic::error(
                        "E0214",
                        "physics declaration requires a name, e.g. `physics \"oak\" (...)`",
                    )
                    .with_span(n.span),
                );
            }
            if let Some(Value::Number(w)) = n.attr("weight") {
                if *w <= 0.0 {
                    diags.push(
                        Diagnostic::warning(
                            "W0211",
                            format!("weight {w} kg/m³ is not positive — a body needs a positive weight to be simulated"),
                        )
                        .with_span(n.span),
                    );
                }
            }
            if let Some(Value::Number(f)) = n.attr("friction") {
                if *f < 0.0 {
                    diags.push(
                        Diagnostic::warning(
                            "W0212",
                            format!("friction {f} is negative — clamped to 0 by most engines"),
                        )
                        .with_span(n.span),
                    );
                }
            }
            if let Some(Value::Number(b)) = n.attr("bounce") {
                if !(0.0..=1.0).contains(b) {
                    diags.push(
                        Diagnostic::warning(
                            "W0213",
                            format!("bounce {b} is outside the [0,1] range (0 = dead thud, 1 = superball)"),
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
                if !matches!(c.kind.as_str(), "bone" | "connector") {
                    diags.push(
                        Diagnostic::error(
                            "E0505",
                            format!(
                                "bone children must be `bone` or `connector` nodes, got `{}`",
                                c.kind
                            ),
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
        "branch" => {
            if let Some(name) = n.attr("form").and_then(as_string_or_ident) {
                if !matches!(
                    name,
                    "decurrent" | "excurrent" | "weeping" | "shrub" | "palm"
                ) {
                    diags.push(
                        Diagnostic::error(
                            "E0210",
                            format!(
                                "branch form must be \"decurrent\", \"excurrent\", \"weeping\", \"shrub\", or \"palm\"; got \"{name}\""
                            ),
                        )
                        .with_span(n.span),
                    );
                }
            }
        }
        "building" => super::building_rules::check_building(n, diags),
        "room_type" => super::building_rules::check_room_type(n, diags),
        "adjacency" => super::building_rules::check_adjacency(n, diags),
        "cave" => super::cave_rules::check_cave(n, diags),
        "feature" => super::cave_rules::check_feature(n, diags),
        "terrain" => super::terrain_rules::check_terrain(n, diags),
        "hole" => super::terrain_rules::check_hole(n, diags),
        "road" => super::terrain_rules::check_road(n, diags),
        "dungeon" => super::dungeon_rules::check_dungeon(n, diags),
        "light" => {
            let kind = n.attr("kind").and_then(as_string_or_ident);
            match kind {
                None => diags.push(
                    Diagnostic::error(
                        "E0801",
                        "`light` requires kind=directional|point|spot",
                    )
                    .with_span(n.span),
                ),
                Some(name) if !matches!(name, "directional" | "point" | "spot") => {
                    diags.push(
                        Diagnostic::error(
                            "E0802",
                            format!(
                                "unknown light kind \"{name}\" (expected directional|point|spot)"
                            ),
                        )
                        .with_span(n.span),
                    );
                }
                _ => {}
            }
            if let Some(Value::Number(i)) = n.attr("intensity") {
                if *i < 0.0 {
                    diags.push(
                        Diagnostic::warning(
                            "W0803",
                            format!("light intensity {i} is negative — clamped to 0 by most renderers"),
                        )
                        .with_span(n.span),
                    );
                }
            }
            if let Some(Value::Number(r)) = n.attr("range") {
                if *r <= 0.0 {
                    diags.push(
                        Diagnostic::error(
                            "E0804",
                            format!("light `range` must be > 0, got {r}"),
                        )
                        .with_span(n.span),
                    );
                }
                if matches!(kind, Some("directional")) {
                    diags.push(
                        Diagnostic::warning(
                            "W0805",
                            "`range` is ignored on directional lights",
                        )
                        .with_span(n.span),
                    );
                }
            }
            let inner = match n.attr("inner_cone") {
                Some(Value::Number(v)) => Some(*v),
                _ => None,
            };
            let outer = match n.attr("outer_cone") {
                Some(Value::Number(v)) => Some(*v),
                _ => None,
            };
            for (label, val) in [("inner_cone", inner), ("outer_cone", outer)] {
                if let Some(v) = val {
                    if !(0.0..=90.0).contains(&v) {
                        diags.push(
                            Diagnostic::error(
                                "E0806",
                                format!("light `{label}` {v}° must be in [0, 90]"),
                            )
                            .with_span(n.span),
                        );
                    }
                    if !matches!(kind, Some("spot")) {
                        diags.push(
                            Diagnostic::warning(
                                "W0807",
                                format!("`{label}` is only used by spot lights"),
                            )
                            .with_span(n.span),
                        );
                    }
                }
            }
            if let (Some(i), Some(o)) = (inner, outer) {
                if i > o {
                    diags.push(
                        Diagnostic::error(
                            "E0808",
                            format!(
                                "light `inner_cone` ({i}°) must be ≤ `outer_cone` ({o}°)"
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
        "decal" => {
            // Decals own their synthesized material — `mat=` would silently
            // shadow the decal's transparent / double-sided / fit-UV setup.
            if n.attr("mat").is_some() {
                diags.push(
                    Diagnostic::error(
                        "E0901",
                        "`decal` does not accept `mat=` — decals own their material; \
                         author `tint=`/`roughness=` directly on the decal instead",
                    )
                    .with_span(n.span),
                );
            }
            // Both image= and prompt= present: image wins, prompt is dead text.
            if n.attr("image").is_some() && n.attr("prompt").is_some() {
                diags.push(
                    Diagnostic::warning(
                        "W0902",
                        "`decal` has both `image=` and `prompt=` — `image=` wins; \
                         the `prompt=` is unused",
                    )
                    .with_span(n.span),
                );
            }
            // size= must be a 2-element list of positive numbers.
            if let Some(v) = n.attr("size") {
                let arity = match v {
                    Value::List(xs) => Some(xs.len()),
                    Value::ListExpr(xs) => Some(xs.len()),
                    _ => None,
                };
                if let Some(a) = arity {
                    if a != 2 {
                        diags.push(
                            Diagnostic::error(
                                "E0903",
                                format!("`decal` size must be a 2-element list [w, h], got {a}"),
                            )
                            .with_span(n.span),
                        );
                    }
                }
                if let Value::List(xs) = v {
                    if xs.iter().any(|n| !n.is_finite() || *n <= 0.0) {
                        diags.push(
                            Diagnostic::error(
                                "E0903",
                                "`decal` size components must be finite and > 0",
                            )
                            .with_span(n.span),
                        );
                    }
                }
            }
            // Curved-surface shortcut: `on=<target>` synthesizes a `conform`
            // patch internally. `at=` is required so we know which connector
            // on the target acts as the anchor; `up=`/`lift=` are optional.
            // Without `on=`, those three attributes are inert noise — reject
            // them so the author doesn't think they're doing something.
            let has_on = n.attr("on").is_some();
            let has_at = n.attr("at").is_some();
            if has_on && !has_at {
                diags.push(
                    Diagnostic::error(
                        "E0904",
                        "`decal` with `on=\"<target>\"` requires `at=\"<connector>\"` \
                         — the patch needs an anchor connector on the target",
                    )
                    .with_span(n.span),
                );
            }
            if !has_on {
                for inert in ["at", "up", "lift"] {
                    if n.attr(inert).is_some() {
                        diags.push(
                            Diagnostic::error(
                                "E0905",
                                format!(
                                    "`decal` `{inert}=` only applies with `on=\"<target>\"` \
                                     (curved-surface shortcut); drop it or add `on=`"
                                ),
                            )
                            .with_span(n.span),
                        );
                    }
                }
            }
        }
        _ => {}
    }
    check_deform_attrs(n, diags);
}

/// Validate the deformation modifier attrs that any geometry primitive accepts.
/// Catches out-of-range stochastic dials so the lowering pass can trust the
/// inputs.
fn check_deform_attrs(n: &Node, diags: &mut Vec<Diagnostic>) {
    for attr in ["noise", "jitter"] {
        if let Some(Value::Number(v)) = n.attr(attr) {
            if !(0.0..=1.0).contains(v) {
                diags.push(
                    Diagnostic::warning(
                        "W1002",
                        format!("`{attr}` is {v}; values are clamped to [0, 1]"),
                    )
                    .with_span(n.span),
                );
            }
        }
    }
    if let Some(Value::Number(t)) = n.attr("taper") {
        if *t < 0.0 {
            diags.push(
                Diagnostic::warning(
                    "W1003",
                    format!("`taper` is {t}; values are clamped to [0, ∞) (1.0 = no change)"),
                )
                .with_span(n.span),
            );
        }
    }
}
