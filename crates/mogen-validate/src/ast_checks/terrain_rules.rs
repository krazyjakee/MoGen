//! Validation rules specific to `terrain` and its `hole` / `road` carving
//! children. Diagnostic codes live in the E15xx / W15xx range so they don't
//! collide with the cave (E12xx) or building (E11xx) rules.

use mogen_core::Diagnostic;
use mogen_dsl::ast::{Node, Value};

use super::schema::as_string_or_ident;

/// Allowed `hole.cap` values.
const HOLE_CAPS: &[&str] = &["open", "floor"];

pub(super) fn check_terrain(n: &Node, diags: &mut Vec<Diagnostic>) {
    // Only carving directives are accepted as children — geometry parented under
    // a terrain wrapper would be dropped by the generator.
    for c in &n.children {
        if c.kind != "hole" && c.kind != "road" {
            diags.push(
                Diagnostic::error(
                    "E1501",
                    format!(
                        "`terrain` body accepts only `hole` and `road` declarations; got `{}`",
                        c.kind
                    ),
                )
                .with_span(c.span),
            );
        }
    }
}

pub(super) fn check_hole(n: &Node, diags: &mut Vec<Diagnostic>) {
    if !n.children.is_empty() {
        diags.push(
            Diagnostic::error(
                "E1502",
                "`hole` does not accept a body block — use attrs only",
            )
            .with_span(n.span),
        );
    }

    // Exactly one footprint shape: `radius=` (circle) or `size=` (rect).
    let has_radius = n.attr("radius").is_some();
    let has_size = n.attr("size").is_some();
    match (has_radius, has_size) {
        (false, false) => diags.push(
            Diagnostic::error(
                "E1503",
                "`hole` requires a footprint — give `radius=` (circle) or `size=[w, d]` (rect)",
            )
            .with_span(n.span),
        ),
        (true, true) => diags.push(
            Diagnostic::error(
                "E1504",
                "`hole` takes either `radius=` or `size=`, not both",
            )
            .with_span(n.span),
        ),
        _ => {}
    }

    if let Some(Value::Number(r)) = n.attr("radius") {
        if *r <= 0.0 {
            diags.push(
                Diagnostic::error("E1505", format!("`hole.radius` must be > 0 (got {r})"))
                    .with_span(n.span),
            );
        }
    }
    if let Some(Value::Number(dep)) = n.attr("depth") {
        if *dep < 0.0 {
            diags.push(
                Diagnostic::error("E1506", format!("`hole.depth` must be ≥ 0 (got {dep})"))
                    .with_span(n.span),
            );
        }
    }
    if let Some(cap) = n.attr("cap").and_then(as_string_or_ident) {
        if !HOLE_CAPS.contains(&cap) {
            diags.push(
                Diagnostic::error(
                    "E1507",
                    format!(
                        "unknown hole cap \"{cap}\" (expected one of: {})",
                        HOLE_CAPS.join(", ")
                    ),
                )
                .with_span(n.span),
            );
        }
    }
}

pub(super) fn check_road(n: &Node, diags: &mut Vec<Diagnostic>) {
    if !n.children.is_empty() {
        diags.push(
            Diagnostic::error(
                "E1508",
                "`road` does not accept a body block — use attrs only",
            )
            .with_span(n.span),
        );
    }

    // `path` is required and must describe at least two waypoints.
    let pts = n.attr("path").map(point_count);
    match pts {
        None => diags.push(
            Diagnostic::error(
                "E1509",
                "`road` requires `path=[[x, z], [x, z], …]` (at least two waypoints)",
            )
            .with_span(n.span),
        ),
        Some(c) if c < 2 => diags.push(
            Diagnostic::error(
                "E1510",
                format!("`road.path` needs at least two waypoints (got {c})"),
            )
            .with_span(n.span),
        ),
        _ => {}
    }

    if let Some(Value::Number(w)) = n.attr("width") {
        if *w <= 0.0 {
            diags.push(
                Diagnostic::error("E1511", format!("`road.width` must be > 0 (got {w})"))
                    .with_span(n.span),
            );
        }
    }
    if let Some(Value::Number(s)) = n.attr("shoulder") {
        if *s < 0.0 {
            diags.push(
                Diagnostic::error("E1512", format!("`road.shoulder` must be ≥ 0 (got {s})"))
                    .with_span(n.span),
            );
        }
    }
}

/// Count the XZ waypoints in a `path` value, accepting nested pairs or a flat
/// even-length list.
fn point_count(v: &Value) -> usize {
    match v {
        Value::ListPair(p) => p.len(),
        Value::List(l) => l.len() / 2,
        _ => 0,
    }
}
