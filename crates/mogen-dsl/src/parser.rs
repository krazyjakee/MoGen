use anyhow::{anyhow, Context, Result};
use mogen_core::Span;
use pest::iterators::Pair;
use pest::Parser;
use pest_derive::Parser;

use crate::ast::{BinOp, Expr, Node, Value};

fn span_of(p: &Pair<Rule>) -> Span {
    let s = p.as_span();
    Span::new(s.start(), s.end())
}

#[derive(Parser)]
#[grammar = "grammar.pest"]
struct DslParser;

pub fn parse(source: &str) -> Result<Vec<Node>> {
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
    let val_pair = it.next().unwrap().into_inner().next().unwrap();
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
        r => return Err(anyhow!("unexpected value rule {:?}", r)),
    };
    Ok((key, value))
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
        Str(String),
    }

    let mut items: Vec<Item> = Vec::new();
    for it in val_pair.into_inner() {
        if it.as_rule() != Rule::list_item {
            continue;
        }
        let inner = it.into_inner().next().unwrap();
        match inner.as_rule() {
            Rule::expr => items.push(Item::Expr(build_expr(inner)?)),
            Rule::string => items.push(Item::Str(unquote(inner.as_str()))),
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
    let has_str = items.iter().any(|i| matches!(i, Item::Str(_)));
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
                    Item::Str(s) => s,
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
        Rule::number => Ok(Expr::Num(inner.as_str().parse()?)),
        Rule::param_ref => Ok(Expr::Param(inner.as_str().trim_start_matches('$').to_string())),
        Rule::expr => build_expr(inner),
        r => Err(anyhow!("unexpected factor rule {:?}", r)),
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

fn unquote(s: &str) -> String {
    s.trim_matches('"').to_string()
}
