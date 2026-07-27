use anyhow::{anyhow, Context, Result};
use mogen_core::Span;
use pest::iterators::Pair;
use pest::Parser;
use pest_derive::Parser;

use crate::ast::{BinOp, Expr, FaceEntry, FaceUv, GradientDef, Node, Value};

fn span_of(p: &Pair<Rule>) -> Span {
    let s = p.as_span();
    Span::new(s.start(), s.end())
}

#[derive(Parser)]
#[grammar = "grammar.pest"]
struct DslParser;

pub fn parse(source: &str) -> Result<Vec<Node>> {
    // BOM tolerance lives in the grammar (`bom_? ` in the `file` rule),
    // not here — stripping the BOM in this function would shift all
    // returned `Span` byte offsets by 3 vs the source the caller renders
    // diagnostics against.
    let mut pairs = DslParser::parse(Rule::file, source).context("parse error")?;
    let file = pairs.next().ok_or_else(|| anyhow!("empty parse"))?;
    let mut nodes = Vec::new();
    for p in file.into_inner() {
        if p.as_rule() == Rule::node {
            nodes.push(build_node(p)?);
        }
    }
    Ok(nodes)
}

fn build_node(pair: Pair<Rule>) -> Result<Node> {
    let span = span_of(&pair);
    let mut kind = String::new();
    let mut kind_span = span;
    let mut name = None;
    let mut attrs = Vec::new();
    let mut children = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => {
                kind_span = span_of(&inner);
                kind = inner.as_str().to_string();
            }
            Rule::name => {
                let s = inner.into_inner().next().unwrap();
                name = Some(unquote(s.as_str()));
            }
            Rule::attr_list => {
                for a in inner.into_inner() {
                    if a.as_rule() == Rule::attr {
                        attrs.push(build_attr(a)?);
                    }
                }
            }
            Rule::block => {
                for c in inner.into_inner() {
                    if c.as_rule() == Rule::node {
                        children.push(build_node(c)?);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(Node { kind, name, attrs, children, span, kind_span, use_id: None, origin: None })
}

fn build_attr(pair: Pair<Rule>) -> Result<(String, Value)> {
    let mut it = pair.into_inner();
    let key = it.next().unwrap().as_str().to_string();
    let value_pair = it.next().unwrap();
    // Weight units are typed: a mass / weight-per-volume literal (`5kg`,
    // `700kg/m3`) only makes sense on a `weight=` attribute. Using one anywhere
    // else — `size=[5kg, 1, 1]`, a rotation, a count — is a dimensional
    // mistake, so reject it at parse time rather than silently treating the
    // kilograms as metres.
    if key != "weight" {
        if let Some(lit) = value_mass_literal(&value_pair) {
            return Err(anyhow!(
                "weight unit `{lit}` is only valid on a `weight=` attribute (found on `{key}=`)"
            ));
        }
    }
    let val_pair = value_pair.into_inner().next().unwrap();
    let value = match val_pair.as_rule() {
        Rule::expr => lift_expr(build_expr(val_pair)?),
        Rule::vec3 => {
            let exprs: Vec<Expr> = val_pair
                .into_inner()
                .filter(|p| p.as_rule() == Rule::expr)
                .map(build_expr)
                .collect::<Result<Vec<_>>>()?;
            if exprs.len() != 3 {
                return Err(anyhow!("vec3 must have 3 components, got {}", exprs.len()));
            }
            let consts: Vec<Option<f32>> = exprs.iter().map(|e| e.eval_const()).collect();
            if consts.iter().all(|c| c.is_some()) {
                Value::Vec3([
                    consts[0].unwrap(),
                    consts[1].unwrap(),
                    consts[2].unwrap(),
                ])
            } else {
                Value::Vec3Expr([exprs[0].clone(), exprs[1].clone(), exprs[2].clone()])
            }
        }
        Rule::list => build_list(val_pair)?,
        Rule::string => Value::String(unquote(val_pair.as_str())),
        Rule::ident => Value::Ident(val_pair.as_str().to_string()),
        Rule::gradient => Value::Gradient(build_gradient(val_pair)?),
        r => return Err(anyhow!("unexpected value rule {:?}", r)),
    };
    Ok((key, value))
}

fn build_gradient(pair: Pair<Rule>) -> Result<GradientDef> {
    let span = span_of(&pair);
    let mut kind = String::new();
    let mut attrs: Vec<(String, Value)> = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::gradient_kind => kind = inner.as_str().to_string(),
            Rule::attr_list => {
                for a in inner.into_inner() {
                    if a.as_rule() == Rule::attr {
                        attrs.push(build_attr(a)?);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(GradientDef { kind, attrs, span })
}

/// Build a [`FaceEntry`] from a `face(...)` list item. The first positional
/// argument is the material string; optional named args `uv_scale` / `uv_offset`
/// (2-number lists) and `uv_swap` (true/false) supply an authored UV transform.
/// A `face("mat")` with no UV args yields `uv: None` so it behaves exactly like
/// the bare `"mat"` string.
fn build_face_call(pair: Pair<Rule>) -> Result<FaceEntry> {
    let span = span_of(&pair);
    let mut mat: Option<String> = None;
    let mut scale = [1.0_f32, 1.0];
    let mut offset = [0.0_f32, 0.0];
    let mut swap = false;
    let mut any_uv = false;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::string => mat = Some(unquote(inner.as_str())),
            Rule::attr => {
                let (key, value) = build_attr(inner)?;
                match key.as_str() {
                    "uv_scale" => {
                        scale = face_uv_pair(&value, "uv_scale")?;
                        any_uv = true;
                    }
                    "uv_offset" => {
                        offset = face_uv_pair(&value, "uv_offset")?;
                        any_uv = true;
                    }
                    "uv_swap" => {
                        swap = face_uv_bool(&value, "uv_swap")?;
                        any_uv = true;
                    }
                    other => {
                        return Err(anyhow!(
                            "unknown face() field `{other}` (expected uv_scale, uv_offset, uv_swap)"
                        ))
                    }
                }
            }
            _ => {}
        }
    }
    let mat = mat.ok_or_else(|| anyhow!("face() requires a material string as its first argument"))?;
    let uv = if any_uv { Some(FaceUv { scale, offset, swap }) } else { None };
    Ok(FaceEntry { mat, uv, span })
}

/// Read a 2-number list value (`[sx, sy]`) for a `face()` UV field. Negatives
/// are allowed (mirroring); values are taken verbatim.
fn face_uv_pair(v: &Value, field: &str) -> Result<[f32; 2]> {
    match v {
        Value::List(l) if l.len() == 2 => Ok([l[0], l[1]]),
        _ => Err(anyhow!("face() `{field}` must be a 2-number list like [1, 1]")),
    }
}

/// Read a boolean value (`true`/`false`, or nonzero number) for `uv_swap`.
fn face_uv_bool(v: &Value, field: &str) -> Result<bool> {
    match v {
        Value::Ident(s) if s == "true" => Ok(true),
        Value::Ident(s) if s == "false" => Ok(false),
        Value::Number(n) => Ok(*n != 0.0),
        _ => Err(anyhow!("face() `{field}` must be true or false")),
    }
}

/// Collapse a deferred expression to a Value::Number if it is fully constant.
fn lift_expr(e: Expr) -> Value {
    match e.eval_const() {
        Some(n) => Value::Number(n),
        None => Value::Expr(e),
    }
}

/// Build a `Value` from a `list` pair. List items may be `expr`, `vec3`, or a
/// nested `list`. Homogeneous lists produce a compact `List`/`ListVec3`/
/// `ListPair`; mixed or non-constant lists fall back to `ListExpr`.
fn build_list(val_pair: Pair<Rule>) -> Result<Value> {
    #[derive(Debug)]
    enum Item {
        Expr(Expr),
        Vec3([f32; 3]),
        Pair([f32; 2]),
        Quad([f32; 4]),
        Str(String, Span),
        Face(FaceEntry),
    }

    let mut items: Vec<Item> = Vec::new();
    for it in val_pair.into_inner() {
        if it.as_rule() != Rule::list_item {
            continue;
        }
        let item_span = span_of(&it);
        let inner = it.into_inner().next().unwrap();
        match inner.as_rule() {
            Rule::expr => items.push(Item::Expr(build_expr(inner)?)),
            Rule::face_call => items.push(Item::Face(build_face_call(inner)?)),
            Rule::string => items.push(Item::Str(unquote(inner.as_str()), item_span)),
            Rule::vec3 => {
                let exprs: Vec<Expr> = inner
                    .into_inner()
                    .filter(|p| p.as_rule() == Rule::expr)
                    .map(build_expr)
                    .collect::<Result<Vec<_>>>()?;
                if exprs.len() != 3 {
                    return Err(anyhow!(
                        "vec3 item in list must have 3 components, got {}",
                        exprs.len()
                    ));
                }
                let consts: Vec<f32> = exprs
                    .iter()
                    .map(|e| {
                        e.eval_const()
                            .ok_or_else(|| anyhow!("nested vec3 items cannot use `$param`"))
                    })
                    .collect::<Result<_>>()?;
                items.push(Item::Vec3([consts[0], consts[1], consts[2]]));
            }
            Rule::list => {
                let exprs: Vec<Expr> = inner
                    .into_inner()
                    .filter(|p| p.as_rule() == Rule::list_item)
                    .map(|li| {
                        let core = li.into_inner().next().unwrap();
                        if core.as_rule() != Rule::expr {
                            return Err(anyhow!(
                                "nested list must contain only scalars (got {:?})",
                                core.as_rule()
                            ));
                        }
                        build_expr(core)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let consts: Vec<f32> = exprs
                    .iter()
                    .map(|e| {
                        e.eval_const()
                            .ok_or_else(|| anyhow!("nested list items cannot use `$param`"))
                    })
                    .collect::<Result<_>>()?;
                match consts.len() {
                    2 => items.push(Item::Pair([consts[0], consts[1]])),
                    3 => items.push(Item::Vec3([consts[0], consts[1], consts[2]])),
                    4 => items.push(Item::Quad([consts[0], consts[1], consts[2], consts[3]])),
                    n => {
                        return Err(anyhow!(
                            "nested list must have 2, 3, or 4 components, got {n}"
                        ))
                    }
                }
            }
            r => return Err(anyhow!("unexpected list_item rule {:?}", r)),
        }
    }

    let has_expr = items.iter().any(|i| matches!(i, Item::Expr(_)));
    let has_vec3 = items.iter().any(|i| matches!(i, Item::Vec3(_)));
    let has_pair = items.iter().any(|i| matches!(i, Item::Pair(_)));
    let has_quad = items.iter().any(|i| matches!(i, Item::Quad(_)));
    let has_str = items.iter().any(|i| matches!(i, Item::Str(..)));
    let has_face = items.iter().any(|i| matches!(i, Item::Face(_)));
    // A `face(...)` entry promotes the whole list to `FaceList`. Bare strings
    // may coexist (they become `uv: None` entries); numeric items may not.
    if has_face && (has_expr || has_vec3 || has_pair || has_quad) {
        return Err(anyhow!(
            "faces list may contain only material strings or face(...) entries — not numbers"
        ));
    }
    if has_face {
        return Ok(Value::FaceList(
            items
                .into_iter()
                .map(|i| match i {
                    Item::Face(fe) => fe,
                    Item::Str(mat, span) => FaceEntry { mat, uv: None, span },
                    _ => unreachable!(),
                })
                .collect(),
        ));
    }
    if has_str && (has_expr || has_vec3 || has_pair || has_quad) {
        return Err(anyhow!(
            "list items must be all strings or all numeric — mixing is not allowed"
        ));
    }
    if (has_vec3 || has_pair || has_quad) && has_expr {
        return Err(anyhow!(
            "list items must be all scalars or all nested sublists — not mixed"
        ));
    }
    let nested_kinds = [has_vec3, has_pair, has_quad].iter().filter(|b| **b).count();
    if nested_kinds > 1 {
        return Err(anyhow!(
            "list items must all be the same arity — mixing 2/3/4-element sublists is not allowed"
        ));
    }

    if has_str {
        return Ok(Value::ListString(
            items
                .into_iter()
                .map(|i| match i {
                    Item::Str(s, _) => s,
                    _ => unreachable!(),
                })
                .collect(),
        ));
    }

    if has_vec3 {
        Ok(Value::ListVec3(
            items
                .into_iter()
                .map(|i| match i {
                    Item::Vec3(v) => v,
                    _ => unreachable!(),
                })
                .collect(),
        ))
    } else if has_pair {
        Ok(Value::ListPair(
            items
                .into_iter()
                .map(|i| match i {
                    Item::Pair(v) => v,
                    _ => unreachable!(),
                })
                .collect(),
        ))
    } else if has_quad {
        Ok(Value::ListQuad(
            items
                .into_iter()
                .map(|i| match i {
                    Item::Quad(v) => v,
                    _ => unreachable!(),
                })
                .collect(),
        ))
    } else {
        let exprs: Vec<Expr> = items
            .into_iter()
            .map(|i| match i {
                Item::Expr(e) => e,
                _ => unreachable!(),
            })
            .collect();
        let consts: Vec<Option<f32>> = exprs.iter().map(|e| e.eval_const()).collect();
        if consts.iter().all(|c| c.is_some()) {
            Ok(Value::List(consts.into_iter().map(|c| c.unwrap()).collect()))
        } else {
            Ok(Value::ListExpr(exprs))
        }
    }
}

fn build_expr(pair: Pair<Rule>) -> Result<Expr> {
    debug_assert_eq!(pair.as_rule(), Rule::expr);
    let mut inner = pair.into_inner();
    let lhs = build_sum(inner.next().unwrap())?;
    if let Some(op_pair) = inner.next() {
        // The single optional comparison: `lhs <op> rhs`. The grammar
        // forbids chained comparisons (`a < b < c` won't parse) — keep
        // the C-vs-Python ambiguity out of v1.
        let op = parse_cmp_op(op_pair.as_str())?;
        let rhs = build_sum(inner.next().unwrap())?;
        return Ok(Expr::Bin(Box::new(lhs), op, Box::new(rhs)));
    }
    Ok(lhs)
}

fn build_sum(pair: Pair<Rule>) -> Result<Expr> {
    debug_assert_eq!(pair.as_rule(), Rule::sum);
    let mut inner = pair.into_inner();
    let mut lhs = build_term(inner.next().unwrap())?;
    while let Some(op_pair) = inner.next() {
        let op = parse_op(op_pair.as_str())?;
        let rhs = build_term(inner.next().unwrap())?;
        lhs = Expr::Bin(Box::new(lhs), op, Box::new(rhs));
    }
    Ok(lhs)
}

fn build_term(pair: Pair<Rule>) -> Result<Expr> {
    debug_assert_eq!(pair.as_rule(), Rule::term);
    let mut inner = pair.into_inner();
    let mut lhs = build_factor(inner.next().unwrap())?;
    while let Some(op_pair) = inner.next() {
        let op = parse_op(op_pair.as_str())?;
        let rhs = build_factor(inner.next().unwrap())?;
        lhs = Expr::Bin(Box::new(lhs), op, Box::new(rhs));
    }
    Ok(lhs)
}

fn build_factor(pair: Pair<Rule>) -> Result<Expr> {
    debug_assert_eq!(pair.as_rule(), Rule::factor);
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::number => Ok(Expr::Num(parse_number_literal(inner.as_str())?)),
        Rule::param_ref => Ok(Expr::Param(inner.as_str().trim_start_matches('$').to_string())),
        Rule::expr => build_expr(inner),
        r => Err(anyhow!("unexpected factor rule {:?}", r)),
    }
}

/// Conversion factor from a length-unit suffix to metres, the canonical base
/// unit. Returns `None` for an unrecognised suffix — the grammar only admits
/// the units listed here, so that path is purely defensive.
fn length_unit_to_metres(unit: &str) -> Option<f32> {
    Some(match unit {
        "mm" => 0.001,
        "cm" => 0.01,
        "m" => 1.0,
        "km" => 1000.0,
        "in" => 0.0254,
        "ft" => 0.3048,
        "yd" => 0.9144,
        _ => return None,
    })
}

/// Conversion factor from a mass-unit suffix to kilograms, the base unit for
/// weights. Returns `None` for an unrecognised suffix — the grammar only admits
/// the units listed here, so that path is purely defensive.
fn mass_unit_to_kg(unit: &str) -> Option<f32> {
    Some(match unit {
        "kg" => 1.0,
        "g" => 0.001,
        "t" => 1000.0, // metric tonne
        "lb" => 0.45359237,
        "oz" => 0.028349523,
        "st" => 6.35029318, // stone
        _ => return None,
    })
}

/// True if a numeric literal carries a mass or weight-per-volume unit suffix
/// (`5kg`, `700kg/m3`) rather than a length suffix or no suffix at all.
fn number_literal_is_mass(lit: &str) -> bool {
    match lit.find(|c: char| c.is_ascii_alphabetic()) {
        None => false,
        Some(i) => {
            let unit = &lit[i..];
            let core = unit
                .strip_suffix("/m3")
                .or_else(|| unit.strip_suffix("/m³"))
                .unwrap_or(unit);
            mass_unit_to_kg(core).is_some()
        }
    }
}

/// Walk a parsed `value` (or any sub-pair) and return the first mass-dimensioned
/// numeric literal it contains, if any. Reaches through vec3 / list / expression
/// nesting so `size=[1, 2kg, 3]` is caught. Used to reject weight units outside
/// `weight=`.
fn value_mass_literal(pair: &Pair<Rule>) -> Option<String> {
    if pair.as_rule() == Rule::number {
        return number_literal_is_mass(pair.as_str()).then(|| pair.as_str().to_string());
    }
    for inner in pair.clone().into_inner() {
        if let Some(l) = value_mass_literal(&inner) {
            return Some(l);
        }
    }
    None
}

/// Parse a numeric literal, applying any trailing unit suffix so the returned
/// value is always in the dimension's base unit. Length suffixes normalise to
/// metres (`18in` → `0.4572`); mass suffixes to kilograms (`5kg` → `5`,
/// `180lb` → `81.6…`); the per-volume form `700kg/m3` to kilograms per cubic
/// metre — numerically the bare mass factor, since kg/m³ is the base density
/// unit. Bare numbers pass through unchanged (`1.5` → `1.5`).
fn parse_number_literal(s: &str) -> Result<f32> {
    match s.find(|c: char| c.is_ascii_alphabetic()) {
        None => Ok(s.parse()?),
        Some(i) => {
            let (num, unit) = s.split_at(i);
            let value: f32 = num.parse()?;
            // A `…/m3` suffix marks a weight-per-volume literal; strip it and
            // reuse the mass factor (the base density unit is kg/m³).
            let core = unit
                .strip_suffix("/m3")
                .or_else(|| unit.strip_suffix("/m³"))
                .unwrap_or(unit);
            let factor = length_unit_to_metres(core)
                .or_else(|| mass_unit_to_kg(core))
                .ok_or_else(|| anyhow!("unknown unit `{unit}`"))?;
            Ok(value * factor)
        }
    }
}

fn parse_op(s: &str) -> Result<BinOp> {
    match s {
        "+" => Ok(BinOp::Add),
        "-" => Ok(BinOp::Sub),
        "*" => Ok(BinOp::Mul),
        "/" => Ok(BinOp::Div),
        other => Err(anyhow!("unknown operator {other}")),
    }
}

fn parse_cmp_op(s: &str) -> Result<BinOp> {
    match s {
        "<"  => Ok(BinOp::Lt),
        "<=" => Ok(BinOp::Le),
        ">"  => Ok(BinOp::Gt),
        ">=" => Ok(BinOp::Ge),
        "==" => Ok(BinOp::Eq),
        "!=" => Ok(BinOp::Ne),
        other => Err(anyhow!("unknown comparison operator {other}")),
    }
}

/// Strip the surrounding quotes and resolve backslash escapes.
///
/// Only `\"` and `\\` are special — everything else after a backslash is left
/// alone, backslash included. That keeps Windows-style texture paths
/// (`"tex\normal.png"`) working verbatim instead of silently turning `\n` into
/// a newline, and it means the only strings whose meaning changed when escapes
/// were introduced are those containing `\"` or `\\`, which no `.mog` in the
/// corpus did.
fn unquote(s: &str) -> String {
    let body = s.strip_prefix('"').unwrap_or(s);
    let body = body.strip_suffix('"').unwrap_or(body);

    if !body.contains('\\') {
        return body.to_string();
    }

    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            // Unknown escape: keep both characters as written.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{parse, parse_number_literal};
    use crate::ast::Value;

    /// Pull the single attribute value off the first node of a parsed source.
    fn first_attr(src: &str, key: &str) -> Value {
        let nodes = parse(src).expect("source should parse");
        nodes[0]
            .attr(key)
            .unwrap_or_else(|| panic!("missing attr `{key}`"))
            .clone()
    }

    #[test]
    fn length_units_normalise_to_metres() {
        // Each unit converts to its metre equivalent.
        assert!((parse_number_literal("1.5m").unwrap() - 1.5).abs() < 1e-6);
        assert!((parse_number_literal("90cm").unwrap() - 0.9).abs() < 1e-6);
        assert!((parse_number_literal("250mm").unwrap() - 0.25).abs() < 1e-6);
        assert!((parse_number_literal("2km").unwrap() - 2000.0).abs() < 1e-6);
        assert!((parse_number_literal("18in").unwrap() - 0.4572).abs() < 1e-6);
        assert!((parse_number_literal("2ft").unwrap() - 0.6096).abs() < 1e-6);
        assert!((parse_number_literal("1yd").unwrap() - 0.9144).abs() < 1e-6);
        // Bare numbers are unitless metres.
        assert!((parse_number_literal("0.45").unwrap() - 0.45).abs() < 1e-6);
        // Negative literals keep their sign.
        assert!((parse_number_literal("-3ft").unwrap() + 0.9144).abs() < 1e-6);
    }

    #[test]
    fn weight_units_normalise_to_kilograms() {
        // Metric masses.
        assert!((parse_number_literal("5kg").unwrap() - 5.0).abs() < 1e-6);
        assert!((parse_number_literal("250g").unwrap() - 0.25).abs() < 1e-6);
        assert!((parse_number_literal("2t").unwrap() - 2000.0).abs() < 1e-4);
        // Imperial masses.
        assert!((parse_number_literal("180lb").unwrap() - 81.6466).abs() < 1e-3);
        assert!((parse_number_literal("16oz").unwrap() - 0.453592).abs() < 1e-4);
        assert!((parse_number_literal("11st").unwrap() - 69.8532).abs() < 1e-2);
    }

    #[test]
    fn weight_per_cubic_metre_normalises_to_kg_per_m3() {
        // `kg/m3` is the base density unit: same numeric value.
        assert!((parse_number_literal("700kg/m3").unwrap() - 700.0).abs() < 1e-3);
        // A tonne per m³ is 1000 kg/m³ — the mass factor carries through.
        assert!((parse_number_literal("0.7t/m3").unwrap() - 700.0).abs() < 1e-2);
        // Unicode superscript form is accepted too.
        assert!((parse_number_literal("917kg/m³").unwrap() - 917.0).abs() < 1e-2);
    }

    #[test]
    fn weight_literals_parse_through_attrs() {
        match first_attr("physics (weight=700kg/m3)\n", "weight") {
            Value::Number(n) => assert!((n - 700.0).abs() < 1e-3),
            other => panic!("expected Number, got {other:?}"),
        }
        match first_attr("box (weight=5kg)\n", "weight") {
            Value::Number(n) => assert!((n - 5.0).abs() < 1e-6),
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[test]
    fn mass_units_rejected_outside_weight() {
        // A weight suffix on a length attribute is a dimensional mistake.
        let err = parse("box (size=[5kg, 1, 1])\n").expect_err("mass in size must fail");
        assert!(err.to_string().contains("weight unit `5kg`"), "got: {err}");
        // Even nested inside an expression.
        assert!(parse("box (height=2 + 1kg)\n").is_err());
        // But `weight=` accepts them (mass on a node, density in a physics block).
        assert!(parse("box (weight=5kg)\n").is_ok());
        assert!(parse("physics \"oak\" (weight=700kg/m3)\n").is_ok());
    }

    #[test]
    fn division_still_parses_when_not_a_density_unit() {
        // `6/2` must stay an expression, not get swallowed as a unit suffix.
        match first_attr("box (height=6/2)\n", "height") {
            Value::Number(n) => assert!((n - 3.0).abs() < 1e-6),
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[test]
    fn units_flow_through_scalars_vecs_and_exprs() {
        match first_attr("box (height=6in)\n", "height") {
            Value::Number(n) => assert!((n - 0.1524).abs() < 1e-6),
            other => panic!("expected Number, got {other:?}"),
        }
        // vec3 components each carry their own unit.
        match first_attr("box (size=[18in, 1ft, 50cm])\n", "size") {
            Value::Vec3(v) => {
                assert!((v[0] - 0.4572).abs() < 1e-6);
                assert!((v[1] - 0.3048).abs() < 1e-6);
                assert!((v[2] - 0.5).abs() < 1e-6);
            }
            other => panic!("expected Vec3, got {other:?}"),
        }
        // Units compose through arithmetic — imperial feet+inches.
        match first_attr("box (height=5ft + 6in)\n", "height") {
            Value::Number(n) => assert!((n - (1.524 + 0.1524)).abs() < 1e-5),
            other => panic!("expected Number, got {other:?}"),
        }
        // yd unit flows through a scalar attribute.
        match first_attr("box (depth=2yd)\n", "depth") {
            Value::Number(n) => assert!((n - 1.8288).abs() < 1e-6),
            other => panic!("expected Number, got {other:?}"),
        }
        // Negative unit literal: pos offset should carry its sign through.
        match first_attr("box (x=-3ft)\n", "x") {
            Value::Number(n) => assert!((n + 0.9144).abs() < 1e-6),
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[test]
    fn bare_numbers_unchanged() {
        match first_attr("box (size=[0.45, 0.04, 0.45])\n", "size") {
            Value::Vec3(v) => assert_eq!(v, [0.45, 0.04, 0.45]),
            other => panic!("expected Vec3, got {other:?}"),
        }
    }

    #[test]
    fn parses_source_with_leading_bom() {
        let src = "\u{feff}meta (name=\"x\")\n";
        let nodes = parse(src).expect("BOM-prefixed source should parse");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].kind, "meta");
        // Spans must reference the *original* source (with BOM still in
        // place) so that diagnostics rendered against `src` highlight the
        // correct text. A previous implementation stripped the BOM in
        // parse() and shifted every span 3 bytes early — the assertion
        // below rejects that regression.
        let s = nodes[0].kind_span;
        assert_eq!(&src[s.start..s.end], "meta");
    }
}
