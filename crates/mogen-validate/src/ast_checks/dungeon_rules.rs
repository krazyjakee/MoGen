//! Validation rules specific to `dungeon`. Mirrors the shape of
//! `cave_rules.rs`: structural checks (allowed children) plus value-domain
//! checks. Diagnostic codes live in the E16xx / W16xx range so they don't
//! collide with building (E11xx), cave (E12xx) or terrain (E15xx).

use mogen_core::Diagnostic;
use mogen_dsl::ast::{Node, Value};

use super::schema::as_string_or_ident;

/// Allowed `dungeon.colliders` values.
const COLLIDER_MODES: &[&str] = &["all", "none"];

pub(super) fn check_dungeon(n: &Node, diags: &mut Vec<Diagnostic>) {
    // A dungeon takes no body — its whole layout is generated from attrs.
    for c in &n.children {
        diags.push(
            Diagnostic::error(
                "E1601",
                format!("`dungeon` does not accept a body block; got `{}`", c.kind),
            )
            .with_span(c.span),
        );
    }

    // seed=0 is silently coerced to 1 at lowering time.
    if let Some(Value::Number(s)) = n.attr("seed") {
        if *s <= 0.0 {
            diags.push(
                Diagnostic::warning(
                    "W1609",
                    format!("`dungeon.seed` must be ≥ 1 (got {s}); will be treated as 1"),
                )
                .with_span(n.span),
            );
        }
    }

    // Strictly positive dimensions / structure.
    for key in ["cell", "wall_thickness", "floor_thickness"] {
        if let Some(Value::Number(v)) = n.attr(key) {
            if *v <= 0.0 || !v.is_finite() {
                diags.push(
                    Diagnostic::error("E1602", format!("`dungeon.{key}` must be > 0 (got {v})"))
                        .with_span(n.span),
                );
            }
        }
    }

    // Counts that must be ≥ 1.
    check_min(n, "levels", 1.0, diags);
    check_min(n, "rooms", 1.0, diags);
    check_min(n, "room_min", 1.0, diags);
    check_min(n, "room_max", 1.0, diags);
    check_min(n, "corridor_width", 1.0, diags);

    // Non-negative counts.
    for key in ["spacing", "loops", "stairs", "prop_spots"] {
        if let Some(Value::Number(v)) = n.attr(key) {
            if *v < 0.0 {
                diags.push(
                    Diagnostic::error("E1603", format!("`dungeon.{key}` must be ≥ 0 (got {v})"))
                        .with_span(n.span),
                );
            }
        }
    }

    // `entrances` is a scalar count or a per-floor array; every count must be ≥ 0.
    let entrance_counts: &[f32] = match n.attr("entrances") {
        Some(Value::Number(v)) => std::slice::from_ref(v),
        Some(Value::Vec3(a)) => a.as_slice(),
        Some(Value::List(v)) => v.as_slice(),
        _ => &[],
    };
    if entrance_counts.iter().any(|v| *v < 0.0) {
        diags.push(
            Diagnostic::error("E1603", "`dungeon.entrances` counts must each be ≥ 0".to_string())
                .with_span(n.span),
        );
    }

    // `size=[w, h, d]` must be a vec3 of positive numbers.
    if let Some(Value::Vec3(s)) = n.attr("size") {
        if s.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            diags.push(
                Diagnostic::error(
                    "E1604",
                    "`dungeon.size` components must be finite and > 0 ([width, height, depth])",
                )
                .with_span(n.span),
            );
        }
    }

    // room_min must not exceed room_max (lowering swaps them; warn for clarity).
    if let (Some(Value::Number(lo)), Some(Value::Number(hi))) =
        (n.attr("room_min"), n.attr("room_max"))
    {
        if lo > hi {
            diags.push(
                Diagnostic::warning(
                    "W1605",
                    format!(
                        "`dungeon.room_min` ({lo}) exceeds `room_max` ({hi}) — they will be swapped"
                    ),
                )
                .with_span(n.span),
            );
        }
    }

    // Mesh-quality scale is clamped to [0.1, 1.0] at lowering.
    if let Some(Value::Number(v)) = n.attr("lod_scale") {
        if *v <= 0.0 || *v > 1.0 {
            diags.push(
                Diagnostic::warning(
                    "W1606",
                    format!("`dungeon.lod_scale` is {v}; values are clamped to [0.1, 1.0]"),
                )
                .with_span(n.span),
            );
        }
    }

    // Collider mode must be one of the known selectors.
    if let Some(mode) = n.attr("colliders").and_then(as_string_or_ident) {
        if !COLLIDER_MODES.contains(&mode) {
            diags.push(
                Diagnostic::error(
                    "E1607",
                    format!(
                        "`dungeon.colliders` must be one of: {} (got \"{mode}\")",
                        COLLIDER_MODES.join(", ")
                    ),
                )
                .with_span(n.span),
            );
        }
    }

    // `collider="aabb"` (the geometry-common attr) is meaningless on a dungeon —
    // it manages its own per-surface trimesh colliders via `colliders=`.
    if n.attr("collider").is_some() {
        diags.push(
            Diagnostic::warning(
                "W1608",
                "`collider=` is ignored on `dungeon`; use `colliders=all|none` \
                 to control dungeon physics",
            )
            .with_span(n.span),
        );
    }
}

fn check_min(n: &Node, key: &str, min: f32, diags: &mut Vec<Diagnostic>) {
    if let Some(Value::Number(v)) = n.attr(key) {
        if *v < min {
            diags.push(
                Diagnostic::error("E1610", format!("`dungeon.{key}` must be ≥ {min} (got {v})"))
                    .with_span(n.span),
            );
        }
    }
}
