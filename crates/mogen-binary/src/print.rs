//! AST → `.mog` text, for `unpack`.
//!
//! Best-effort canonical formatting: comments and the author's original
//! whitespace are gone (they never survive parsing), but the output re-parses
//! to an equivalent AST. Two-space indentation, one attribute list per node,
//! children in a `{ … }` block.

use std::fmt::Write;

use mogen_dsl::ast::{Expr, GradientDef, Node, Value};

/// Render an AST forest as `.mog` source.
pub fn to_mog_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for n in nodes {
        write_node(&mut out, n, 0);
    }
    out
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_node(out: &mut String, n: &Node, depth: usize) {
    indent(out, depth);
    out.push_str(&n.kind);
    if let Some(name) = &n.name {
        let _ = write!(out, " \"{name}\"");
    }
    if !n.attrs.is_empty() {
        out.push_str(" (");
        for (i, (k, v)) in n.attrs.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "{k}=");
            write_value(out, v);
        }
        out.push(')');
    }
    if n.children.is_empty() {
        out.push('\n');
    } else {
        out.push_str(" {\n");
        for c in &n.children {
            write_node(out, c, depth + 1);
        }
        indent(out, depth);
        out.push_str("}\n");
    }
}

fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Number(n) => out.push_str(&fmt_num(*n)),
        Value::Vec3(a) => write_num_list(out, a),
        Value::String(s) => {
            let _ = write!(out, "\"{s}\"");
        }
        Value::Ident(s) => out.push_str(s),
        Value::Expr(e) => write_expr(out, e, 0),
        Value::List(xs) => write_num_list(out, xs),
        Value::ListVec3(rows) => {
            out.push('[');
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_num_list(out, row);
            }
            out.push(']');
        }
        Value::ListPair(rows) => {
            out.push('[');
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_num_list(out, row);
            }
            out.push(']');
        }
        Value::ListQuad(rows) => {
            out.push('[');
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_num_list(out, row);
            }
            out.push(']');
        }
        Value::ListString(xs) => {
            out.push('[');
            for (i, s) in xs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "\"{s}\"");
            }
            out.push(']');
        }
        Value::Gradient(g) => write_gradient(out, g),
        Value::FaceList(faces) => {
            out.push('[');
            for (i, f) in faces.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match &f.uv {
                    None => {
                        let _ = write!(out, "\"{}\"", f.mat);
                    }
                    Some(uv) => {
                        let _ = write!(out, "face(\"{}\"", f.mat);
                        out.push_str(", uv_scale=");
                        write_num_list(out, &uv.scale);
                        out.push_str(", uv_offset=");
                        write_num_list(out, &uv.offset);
                        let _ = write!(out, ", uv_swap={}", uv.swap);
                        out.push(')');
                    }
                }
            }
            out.push(']');
        }
        Value::Vec3Expr(es) => {
            out.push('[');
            for (i, e) in es.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_expr(out, e, 0);
            }
            out.push(']');
        }
        Value::ListExpr(es) => {
            out.push('[');
            for (i, e) in es.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_expr(out, e, 0);
            }
            out.push(']');
        }
    }
}

fn write_gradient(out: &mut String, g: &GradientDef) {
    out.push_str(&g.kind);
    out.push('(');
    for (i, (k, v)) in g.attrs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{k}=");
        write_value(out, v);
    }
    out.push(')');
}

fn write_num_list(out: &mut String, xs: &[f32]) {
    out.push('[');
    for (i, x) in xs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&fmt_num(*x));
    }
    out.push(']');
}

/// Shortest decimal that round-trips the `f32`. Rust's `Display` already emits
/// the minimal round-tripping form (`1.0` → `1`, `0.3` → `0.3`).
fn fmt_num(n: f32) -> String {
    format!("{n}")
}

/// Precedence of a binary operator: comparisons bind loosest, then `+`/`-`,
/// then `*`/`/`. Used to insert the minimal set of parentheses.
fn op_prec(op: mogen_dsl::ast::BinOp) -> u8 {
    use mogen_dsl::ast::BinOp::*;
    match op {
        Lt | Le | Gt | Ge | Eq | Ne => 0,
        Add | Sub => 1,
        Mul | Div => 2,
    }
}

fn op_str(op: mogen_dsl::ast::BinOp) -> &'static str {
    use mogen_dsl::ast::BinOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        Eq => "==",
        Ne => "!=",
    }
}

/// Print an expression, parenthesising a child only when its operator binds
/// looser than the surrounding `parent_prec`.
fn write_expr(out: &mut String, e: &Expr, parent_prec: u8) {
    match e {
        Expr::Num(n) => out.push_str(&fmt_num(*n)),
        Expr::Param(name) => {
            out.push('$');
            out.push_str(name);
        }
        Expr::Bin(a, op, b) => {
            let prec = op_prec(*op);
            let need_parens = prec < parent_prec;
            if need_parens {
                out.push('(');
            }
            write_expr(out, a, prec);
            let _ = write!(out, " {} ", op_str(*op));
            // Right child at prec+1 so equal-precedence right operands
            // (left-associative) get parenthesised.
            write_expr(out, b, prec + 1);
            if need_parens {
                out.push(')');
            }
        }
    }
}
