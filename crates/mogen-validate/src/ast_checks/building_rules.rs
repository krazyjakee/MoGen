//! Validation rules specific to `building`, `room_type`, and `adjacency`
//! nodes. Pulled out of `rules.rs` to keep that file from growing past the
//! per-file size cap as more building features land in later tranches.

use mogen_core::Diagnostic;
use mogen_dsl::ast::{Node, Value};

use super::schema::as_string_or_ident;

/// Allowed `room_type.kind` values.
const ROOM_TYPE_KINDS: &[&str] = &[
    "public", "private", "service", "utility", "secure", "staff_only",
];

/// Allowed `building.style` values. After Tranche 4 every entry here also
/// has a lowering implementation, so `BUILDING_STYLES_IMPLEMENTED` mirrors
/// this list. The two are kept separate so a future tranche introducing a
/// new style can land the grammar acceptance and the lowering in separate
/// PRs again.
const BUILDING_STYLES: &[&str] = &[
    "grid", "apartment-block", "office-core", "hotel-corridor",
    "radial", "organic", "maze",
];

const BUILDING_ROOFS: &[&str] = &[
    "flat", "pitched", "gabled", "hipped", "mansard", "shed",
];

/// Layout styles whose lowering is implemented. Authoring a style outside
/// this set is rejected so an unimplemented style doesn't silently fall
/// through to a different algorithm at build time.
const BUILDING_STYLES_IMPLEMENTED: &[&str] = &[
    "grid",
    "apartment-block",
    "hotel-corridor",
    "office-core",
    "radial",
    "organic",
    "maze",
];

pub(super) fn check_building(n: &Node, diags: &mut Vec<Diagnostic>) {
    // Children must be `room_type` or `adjacency` only. Everything else gets
    // a clear rejection so authors can't smuggle geometry into the wrapper
    // (it would be dropped by the lowering pass anyway).
    for c in &n.children {
        if !matches!(c.kind.as_str(), "room_type" | "adjacency") {
            diags.push(
                Diagnostic::error(
                    "E1101",
                    format!(
                        "`building` body accepts only `room_type` and `adjacency` declarations; got `{}`",
                        c.kind
                    ),
                )
                .with_span(c.span),
            );
        }
    }

    if let Some(name) = n.attr("style").and_then(as_string_or_ident) {
        if !BUILDING_STYLES.contains(&name) {
            diags.push(
                Diagnostic::error(
                    "E1102",
                    format!(
                        "unknown building style \"{name}\" (expected one of: {})",
                        BUILDING_STYLES.join(", ")
                    ),
                )
                .with_span(n.span),
            );
        } else if !BUILDING_STYLES_IMPLEMENTED.contains(&name) {
            diags.push(
                Diagnostic::error(
                    "E1110",
                    format!(
                        "building style \"{name}\" is reserved for a future tranche \
                         — see docs/building.md (currently supported: {})",
                        BUILDING_STYLES_IMPLEMENTED.join(", ")
                    ),
                )
                .with_span(n.span),
            );
        }
    }

    if let Some(name) = n.attr("roof").and_then(as_string_or_ident) {
        if !BUILDING_ROOFS.contains(&name) {
            diags.push(
                Diagnostic::error(
                    "E1103",
                    format!(
                        "unknown roof \"{name}\" (expected one of: {})",
                        BUILDING_ROOFS.join(", ")
                    ),
                )
                .with_span(n.span),
            );
        }
        // (E1111 retired in Tranche 4: every BUILDING_ROOFS entry is now
        // implemented. Code reserved — don't recycle it for a different
        // condition.)
    }

    // Bounded counts that must be positive.
    check_min(n, "rooms", 1.0, "E1104", diags);
    check_min(n, "floors_above", 1.0, "E1104", diags);
    check_min(n, "entrances", 1.0, "E1104", diags);
    check_min_strict_positive(n, "floor_area", "E1104", diags);
    check_min_strict_positive(n, "cellar_area", "E1104", diags);
    check_min_strict_positive(n, "ceiling_height", "E1104", diags);
    check_min_strict_positive(n, "door_w", "E1104", diags);
    check_min_strict_positive(n, "door_h", "E1104", diags);
    check_min_strict_positive(n, "window_w", "E1104", diags);
    check_min_strict_positive(n, "window_h", "E1104", diags);
    check_min_strict_positive(n, "wall_thickness", "E1104", diags);
    check_min_strict_positive(n, "ceiling_thickness", "E1104", diags);

    // Non-negative counts.
    for key in ["floors_below", "windows", "skylights", "elevators", "staircases"] {
        if let Some(Value::Number(v)) = n.attr(key) {
            if *v < 0.0 {
                diags.push(
                    Diagnostic::error(
                        "E1105",
                        format!("`building.{key}` must be ≥ 0 (got {v})"),
                    )
                    .with_span(n.span),
                );
            }
        }
    }

    // Multi-storey buildings require at least one staircase so the upper
    // floors are reachable. Elevators alone don't satisfy this in T2 — the
    // cab geometry doesn't double as a service stair. Emitted as W1113 so
    // the build still succeeds with isolated upper floors if the author
    // explicitly chose `staircases=0`.
    let floors_above = n.attr_number("floors_above").unwrap_or(1.0).max(1.0);
    let floors_below = n.attr_number("floors_below").unwrap_or(0.0).max(0.0);
    let staircases = n.attr_number("staircases").unwrap_or(0.0).max(0.0);
    let elevators = n.attr_number("elevators").unwrap_or(0.0).max(0.0);
    let skylights = n.attr_number("skylights").unwrap_or(0.0).max(0.0);
    if floors_above + floors_below > 1.0 && staircases < 1.0 {
        diags.push(
            Diagnostic::warning(
                "W1113",
                format!(
                    "multi-storey building (floors_above={floors_above}, floors_below={floors_below}) \
                     has no staircase — upper floors will be visually disconnected. \
                     Add `staircases=1` (or more) to link them."
                ),
            )
            .with_span(n.span),
        );
    }

    // Skylights only cut through flat ceilings in T4. A non-flat roof would
    // need CSG-cutting a sloped wedge mesh, which is out of scope for this
    // tranche — the skylight planner is short-circuited and the modules
    // simply don't get emitted, so warn the author rather than silently
    // dropping the request.
    let roof_name = n
        .attr("roof")
        .and_then(as_string_or_ident)
        .unwrap_or("flat");
    if skylights > 0.0 && roof_name != "flat" {
        diags.push(
            Diagnostic::warning(
                "W1114",
                format!(
                    "skylights are only carved through flat roofs in T4 \
                     — `skylights={skylights}` will be ignored under roof=\"{roof_name}\". \
                     Set `roof=\"flat\"` to use skylights."
                ),
            )
            .with_span(n.span),
        );
    }

    // Cellar circulation overflow guard. The east circulation column is
    // planned once against the above-ground footprint so stairs/elevators
    // line up vertically; if `cellar_area` shrinks the basement footprint
    // below the column's reach, the lowering pass will abort. Catch it
    // here with a clearer warning whose span points at the building node.
    if let Some(cellar) = n.attr_number("cellar_area").filter(|v| *v > 0.0) {
        let floor_area = n.attr_number("floor_area").unwrap_or(120.0).max(4.0);
        let circulation_cells = staircases + elevators;
        // Heuristic mirrors `floor_dims()`'s SQRT_2 aspect: shorter side of
        // the cellar must clear the 2 m circulation column plus 2 m of
        // breathing room for at least one room.
        let cellar_short_side = (cellar / std::f32::consts::SQRT_2).sqrt();
        if circulation_cells > 0.0 && cellar_short_side < 4.0 {
            diags.push(
                Diagnostic::warning(
                    "W1115",
                    format!(
                        "cellar_area={cellar} m² is too small to fit the vertical-circulation column \
                         (stair/elevator) — basement lowering will fail. Either grow \
                         `cellar_area` or drop `staircases`/`elevators` for this building."
                    ),
                )
                .with_span(n.span),
            );
        }
        if cellar > floor_area {
            diags.push(
                Diagnostic::warning(
                    "W1116",
                    format!(
                        "cellar_area={cellar} m² exceeds floor_area={floor_area} m² \
                         — the basement would stick out under the above-ground footprint. \
                         The lowering pass will clamp the basement to floor_area."
                    ),
                )
                .with_span(n.span),
            );
        }
    }

    // Room-type / adjacency name cross-checks.
    let mut declared_types: Vec<&str> = Vec::new();
    for c in &n.children {
        if c.kind == "room_type" {
            if let Some(name) = c.name.as_deref() {
                declared_types.push(name);
            }
        }
    }
    if declared_types.is_empty() {
        diags.push(
            Diagnostic::error(
                "E1106",
                "`building` requires at least one `room_type \"<name>\" (...)` declaration",
            )
            .with_span(n.span),
        );
    }
    for c in &n.children {
        if c.kind != "adjacency" {
            continue;
        }
        if let Some(adj_name) = c.name.as_deref() {
            if !declared_types.iter().any(|t| *t == adj_name) {
                diags.push(
                    Diagnostic::error(
                        "E1107",
                        format!(
                            "`adjacency \"{adj_name}\"` does not match any declared `room_type` in this building"
                        ),
                    )
                    .with_span(c.span),
                );
            }
        }
        for key in ["adjacent_to", "away_from"] {
            if let Some(Value::ListString(items)) = c.attr(key) {
                for item in items {
                    if !declared_types.iter().any(|t| *t == item.as_str()) {
                        diags.push(
                            Diagnostic::error(
                                "E1108",
                                format!(
                                    "`adjacency.{key}` references unknown room type \"{item}\""
                                ),
                            )
                            .with_span(c.span),
                        );
                    }
                }
            }
        }
    }
}

pub(super) fn check_room_type(n: &Node, diags: &mut Vec<Diagnostic>) {
    if n.name.is_none() {
        diags.push(
            Diagnostic::error(
                "E1120",
                "`room_type` requires a quoted name, e.g. `room_type \"bedroom\" (...)`",
            )
            .with_span(n.span),
        );
    }
    if !n.children.is_empty() {
        diags.push(
            Diagnostic::error(
                "E1121",
                "`room_type` does not accept a body block — use attrs only",
            )
            .with_span(n.span),
        );
    }
    match n.attr("kind").and_then(as_string_or_ident) {
        None => diags.push(
            Diagnostic::error(
                "E1122",
                format!(
                    "`room_type` requires `kind=` (one of: {})",
                    ROOM_TYPE_KINDS.join(", ")
                ),
            )
            .with_span(n.span),
        ),
        Some(k) if !ROOM_TYPE_KINDS.contains(&k) => diags.push(
            Diagnostic::error(
                "E1123",
                format!(
                    "unknown room type kind \"{k}\" (expected one of: {})",
                    ROOM_TYPE_KINDS.join(", ")
                ),
            )
            .with_span(n.span),
        ),
        _ => {}
    }
    if let Some(Value::Number(d)) = n.attr("density") {
        if !(0.0..=10.0).contains(d) {
            diags.push(
                Diagnostic::error(
                    "E1124",
                    format!("`room_type.density` {d} must be in [0, 10]"),
                )
                .with_span(n.span),
            );
        }
    }
    for key in ["min_area", "max_area"] {
        if let Some(Value::Number(v)) = n.attr(key) {
            if *v <= 0.0 {
                diags.push(
                    Diagnostic::error(
                        "E1125",
                        format!("`room_type.{key}` {v} must be > 0"),
                    )
                    .with_span(n.span),
                );
            }
        }
    }
}

pub(super) fn check_adjacency(n: &Node, diags: &mut Vec<Diagnostic>) {
    if n.name.is_none() {
        diags.push(
            Diagnostic::error(
                "E1130",
                "`adjacency` requires a quoted name matching one of the building's `room_type` names",
            )
            .with_span(n.span),
        );
    }
    if !n.children.is_empty() {
        diags.push(
            Diagnostic::error(
                "E1131",
                "`adjacency` does not accept a body block — use attrs only",
            )
            .with_span(n.span),
        );
    }
    if n.attr("adjacent_to").is_none() && n.attr("away_from").is_none() {
        diags.push(
            Diagnostic::warning(
                "W1132",
                "`adjacency` declared without `adjacent_to=` or `away_from=` — has no effect",
            )
            .with_span(n.span),
        );
    }
}

fn check_min(n: &Node, key: &str, min: f32, code: &'static str, diags: &mut Vec<Diagnostic>) {
    if let Some(Value::Number(v)) = n.attr(key) {
        if *v < min {
            diags.push(
                Diagnostic::error(
                    code,
                    format!("`building.{key}` must be ≥ {min} (got {v})"),
                )
                .with_span(n.span),
            );
        }
    }
}

fn check_min_strict_positive(n: &Node, key: &str, code: &'static str, diags: &mut Vec<Diagnostic>) {
    if let Some(Value::Number(v)) = n.attr(key) {
        if *v <= 0.0 || !v.is_finite() {
            diags.push(
                Diagnostic::error(
                    code,
                    format!("`building.{key}` must be > 0 (got {v})"),
                )
                .with_span(n.span),
            );
        }
    }
}
