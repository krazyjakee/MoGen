//! Validation rules specific to `cave` and its `feature` children. Mirrors the
//! shape of `building_rules.rs`: structural checks (allowed children, required
//! attrs) plus value-domain checks. Diagnostic codes live in the E12xx / W12xx
//! range so they don't collide with the building rules (E11xx).

use mogen_core::Diagnostic;
use mogen_dsl::ast::{Node, Value};

use super::schema::as_string_or_ident;

/// Allowed `feature.kind` values — one per decoration the cave can scatter.
const FEATURE_KINDS: &[&str] = &[
    "stalagmite",
    "stalactite",
    "rock_pile",
    "pool",
    "lake",
];

pub(super) fn check_cave(n: &Node, diags: &mut Vec<Diagnostic>) {
    // Only `feature` children are accepted — geometry smuggled into the wrapper
    // would be dropped by the generator anyway.
    for c in &n.children {
        if c.kind != "feature" {
            diags.push(
                Diagnostic::error(
                    "E1201",
                    format!(
                        "`cave` body accepts only `feature` declarations; got `{}`",
                        c.kind
                    ),
                )
                .with_span(c.span),
            );
        }
    }

    // Positive counts / dimensions.
    check_min(n, "chambers", 1.0, diags);
    check_min(n, "levels", 1.0, diags);
    check_strict_positive(n, "chamber_min", diags);
    check_strict_positive(n, "chamber_max", diags);
    check_strict_positive(n, "passage_radius", diags);
    check_strict_positive(n, "margin", diags);
    check_strict_positive(n, "resolution", diags);

    // Non-negative counts.
    for key in [
        "loops",
        "entrances",
        "rock_piles",
        "pools",
        "lakes",
        "stalagmites",
        "stalactites",
    ] {
        if let Some(Value::Number(v)) = n.attr(key) {
            if *v < 0.0 {
                diags.push(
                    Diagnostic::error("E1202", format!("`cave.{key}` must be ≥ 0 (got {v})"))
                        .with_span(n.span),
                );
            }
        }
    }

    // `size=[w, h, d]` must be a vec3 of positive numbers.
    if let Some(Value::Vec3(s)) = n.attr("size") {
        if s.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            diags.push(
                Diagnostic::error(
                    "E1203",
                    "`cave.size` components must be finite and > 0 ([width, height, depth])",
                )
                .with_span(n.span),
            );
        }
    }

    // Slope cap must be a sensible walkable angle.
    if let Some(Value::Number(s)) = n.attr("max_slope") {
        if *s <= 0.0 || *s >= 90.0 {
            diags.push(
                Diagnostic::error(
                    "E1204",
                    format!("`cave.max_slope` must be in (0, 90) degrees (got {s})"),
                )
                .with_span(n.span),
            );
        }
    }

    // chamber_min must not exceed chamber_max (the lowering pass swaps them,
    // but warn so the author's intent is clear).
    if let (Some(Value::Number(lo)), Some(Value::Number(hi))) =
        (n.attr("chamber_min"), n.attr("chamber_max"))
    {
        if lo > hi {
            diags.push(
                Diagnostic::warning(
                    "W1205",
                    format!(
                        "`cave.chamber_min` ({lo}) exceeds `chamber_max` ({hi}) — they will be swapped"
                    ),
                )
                .with_span(n.span),
            );
        }
    }

    // [0,1] dials.
    for key in ["roughness", "chamber_flatten"] {
        if let Some(Value::Number(v)) = n.attr(key) {
            if !(0.0..=1.0).contains(v) {
                diags.push(
                    Diagnostic::warning(
                        "W1206",
                        format!("`cave.{key}` is {v}; values are clamped to [0, 1]"),
                    )
                    .with_span(n.span),
                );
            }
        }
    }
}

pub(super) fn check_feature(n: &Node, diags: &mut Vec<Diagnostic>) {
    if !n.children.is_empty() {
        diags.push(
            Diagnostic::error(
                "E1210",
                "`feature` does not accept a body block — use attrs only",
            )
            .with_span(n.span),
        );
    }
    match n.attr("kind").and_then(as_string_or_ident) {
        None => diags.push(
            Diagnostic::error(
                "E1211",
                format!(
                    "`feature` requires `kind=` (one of: {})",
                    FEATURE_KINDS.join(", ")
                ),
            )
            .with_span(n.span),
        ),
        Some(k) if !FEATURE_KINDS.contains(&k) => diags.push(
            Diagnostic::error(
                "E1212",
                format!(
                    "unknown feature kind \"{k}\" (expected one of: {})",
                    FEATURE_KINDS.join(", ")
                ),
            )
            .with_span(n.span),
        ),
        _ => {}
    }
    if let Some(Value::Number(c)) = n.attr("count") {
        if *c < 0.0 {
            diags.push(
                Diagnostic::error("E1213", format!("`feature.count` must be ≥ 0 (got {c})"))
                    .with_span(n.span),
            );
        }
    }
    for key in ["min_size", "max_size"] {
        if let Some(Value::Number(v)) = n.attr(key) {
            if *v <= 0.0 {
                diags.push(
                    Diagnostic::error("E1214", format!("`feature.{key}` must be > 0 (got {v})"))
                        .with_span(n.span),
                );
            }
        }
    }
}

fn check_min(n: &Node, key: &str, min: f32, diags: &mut Vec<Diagnostic>) {
    if let Some(Value::Number(v)) = n.attr(key) {
        if *v < min {
            diags.push(
                Diagnostic::error("E1207", format!("`cave.{key}` must be ≥ {min} (got {v})"))
                    .with_span(n.span),
            );
        }
    }
}

fn check_strict_positive(n: &Node, key: &str, diags: &mut Vec<Diagnostic>) {
    if let Some(Value::Number(v)) = n.attr(key) {
        if *v <= 0.0 || !v.is_finite() {
            diags.push(
                Diagnostic::error("E1208", format!("`cave.{key}` must be > 0 (got {v})"))
                    .with_span(n.span),
            );
        }
    }
}
