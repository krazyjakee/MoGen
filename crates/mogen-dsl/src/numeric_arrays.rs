//! Semantic checks shared by AST validation and resolved primitive lowering.
use crate::ast::{Node, Value};
use mogen_core::Diagnostic;

pub fn validate(node: &Node) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let (arrays, points): (&[(&str, bool)], Option<&str>) = match node.kind.as_str() {
        "spline_tube" => (&[("radii", true)], Some("points")),
        "spline_ribbon" => (&[("widths", true)], Some("points")),
        "sweep" => (&[("roll", false), ("scale_along", true)], Some("path")),
        "metaball" => (&[("radii", true)], Some("points")),
        "loft" => (&[("heights", false)], None),
        _ => return diags,
    };
    let error = |code, message| {
        Diagnostic::error(code, format!("{} {:?}: {message}", node.kind, node.name))
            .with_span(node.span)
    };
    for &(key, positive) in arrays {
        let Some(value) = node.attr(key) else {
            continue;
        };
        let len = match value {
            Value::List(v) => v.len(),
            Value::Vec3(_) | Value::Vec3Expr(_) => 3,
            Value::ListExpr(v) => v.len(),
            _ => {
                diags.push(error("E0112", format!("{key} expects a scalar array; nested coordinate lists and bare scalars are not accepted.")));
                continue;
            }
        };
        let expected = points
            .and_then(|key| node.attr_list_vec3(key))
            .map(|p| p.len())
            .or_else(|| points.filter(|key| node.attr(key).is_none()).map(|_| 2));
        if len == 0
            || (key == "heights" && len < 2)
            || expected.is_some_and(|n| len != 1 && len != n)
        {
            let expected = expected
                .map(|n| format!("1 (constant) or {n} (one per control point)"))
                .unwrap_or_else(|| "at least 2 section heights".into());
            diags.push(error(
                "E0112",
                format!("{key} has {len} values; expected {expected}. Correct the array length."),
            ));
        }
        if let Some(values) = node.attr_list(key) {
            let bad = values
                .iter()
                .filter(|v| !v.is_finite() || (positive && **v < 0.0))
                .count();
            if bad != 0 {
                diags.push(error("E0113", format!("{key} has {bad} invalid values; expected {}. Correct the values before tessellation.",
                    if positive { "finite nonnegative numbers" } else { "finite numbers" })));
            }
        }
    }
    // Generic schema `list` must not allow a malformed point/profile stream
    // to disappear into a lowerer's fallback geometry.
    let coordinates: &[(&str, usize)] = match node.kind.as_str() {
        "sweep" => &[("path", 3), ("profile", 2)],
        "loft" => &[("points", 2)],
        _ => &[("points", 3)],
    };
    for &(key, dimension) in coordinates {
        if node.attr(key).is_none() {
            continue;
        }
        let valid = if dimension == 3 {
            node.attr_list_vec3(key).is_some()
        } else {
            node.attr_list_pair(key).is_some()
        };
        if !valid {
            diags.push(error("E0114", format!("{key} expects {dimension}-component coordinate rows (or a flat list divisible by {dimension}). Correct the coordinate dimensions.")));
        }
    }
    diags
}
