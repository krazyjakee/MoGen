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

/// Allowed `building.style` values. Tranche 1 only implements `grid` and
/// `apartment-block` — the others parse without error so authors can prepare
/// scenes ahead of later tranches, but `check_building` flags them with a
/// pending-tranche warning so they don't silently fall through to `grid`.
const BUILDING_STYLES: &[&str] = &[
    "grid", "apartment-block", "office-core", "hotel-corridor",
    "radial", "organic", "maze",
];

const BUILDING_ROOFS: &[&str] = &[
    "flat", "pitched", "gabled", "hipped", "mansard", "shed",
];

/// Layout styles whose lowering is implemented. Authoring a style outside
/// this set is rejected so an unimplemented style doesn't silently fall
/// through to a different algorithm at build time. Grow this list as
/// each tranche lands a new style.
const BUILDING_STYLES_IMPLEMENTED: &[&str] = &[
    "grid",
    "apartment-block",
    "hotel-corridor",
    "office-core",
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
        } else if name != "flat" {
            diags.push(
                Diagnostic::error(
                    "E1111",
                    format!(
                        "roof=\"{name}\" arrives in a future tranche — see docs/building.md \
                         (T1 supports: flat)"
                    ),
                )
                .with_span(n.span),
            );
        }
    }

    // Bounded counts that must be positive.
    check_min(n, "rooms", 1.0, "E1104", diags);
    check_min(n, "floors_above", 1.0, "E1104", diags);
    check_min(n, "entrances", 1.0, "E1104", diags);
    check_min_strict_positive(n, "floor_area", "E1104", diags);
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
