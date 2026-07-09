//! AST → MOGB bytes.
//!
//! Two passes over the tree share one buffer: the node stream is encoded into
//! `body` while every string it references is interned into `table`. Because
//! preset strings never enter the file table, the header can be assembled after
//! the walk (magic + version + flags + file string table + node stream).

use std::collections::HashMap;

use mogen_dsl::ast::{Expr, GradientDef, Node, Value};

use crate::wire::*;
use crate::{FLAG_LOSSY, MAGIC, VERSION};

/// Encode an AST forest to MOGB, lossless (every `f32` round-trips exactly).
pub fn encode(nodes: &[Node]) -> Vec<u8> {
    encode_inner(nodes, false)
}

/// Encode an AST forest to MOGB, forcing `/1000` fixed-point on all numbers.
/// Smaller output at ~3 decimal places of precision — lossy for anything
/// finer.
pub fn encode_lossy(nodes: &[Node]) -> Vec<u8> {
    encode_inner(nodes, true)
}

fn encode_inner(nodes: &[Node], lossy: bool) -> Vec<u8> {
    let mut table = StringTable::new();
    let mut body = Vec::new();
    write_uvarint(&mut body, nodes.len() as u64);
    for n in nodes {
        encode_node(&mut body, &mut table, n, lossy);
    }

    let mut out = Vec::with_capacity(body.len() + 64);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(if lossy { FLAG_LOSSY } else { 0 });
    table.serialize(&mut out);
    out.extend_from_slice(&body);
    out
}

/// Interns strings against the preset dictionary first, then a per-file table.
/// A reference is `preset_index` for a hit, or `PRESET.len() + file_index`
/// otherwise. Only the file-local strings are serialised.
struct StringTable {
    preset: HashMap<&'static str, u64>,
    local: HashMap<String, u64>,
    ordered: Vec<String>,
}

impl StringTable {
    fn new() -> Self {
        let mut preset = HashMap::new();
        for (i, s) in crate::preset::PRESET.iter().enumerate() {
            // First index wins for any accidental duplicate in the preset.
            preset.entry(*s).or_insert(i as u64);
        }
        StringTable {
            preset,
            local: HashMap::new(),
            ordered: Vec::new(),
        }
    }

    fn intern(&mut self, s: &str) -> u64 {
        if let Some(&i) = self.preset.get(s) {
            return i;
        }
        if let Some(&i) = self.local.get(s) {
            return i;
        }
        let base = crate::preset::PRESET.len() as u64;
        let idx = base + self.ordered.len() as u64;
        self.local.insert(s.to_string(), idx);
        self.ordered.push(s.to_string());
        idx
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        write_uvarint(out, self.ordered.len() as u64);
        for s in &self.ordered {
            write_uvarint(out, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
    }
}

fn write_strref(out: &mut Vec<u8>, table: &mut StringTable, s: &str) {
    let i = table.intern(s);
    write_uvarint(out, i);
}

fn encode_node(out: &mut Vec<u8>, table: &mut StringTable, n: &Node, lossy: bool) {
    write_strref(out, table, &n.kind);
    let has_name = n.name.is_some();
    let has_attrs = !n.attrs.is_empty();
    let has_children = !n.children.is_empty();
    let flags = (has_name as u8) | ((has_attrs as u8) << 1) | ((has_children as u8) << 2);
    out.push(flags);
    if let Some(name) = &n.name {
        write_strref(out, table, name);
    }
    if has_attrs {
        write_uvarint(out, n.attrs.len() as u64);
        for (k, v) in &n.attrs {
            write_strref(out, table, k);
            encode_value(out, table, v, lossy);
        }
    }
    if has_children {
        write_uvarint(out, n.children.len() as u64);
        for c in &n.children {
            encode_node(out, table, c, lossy);
        }
    }
}

/// Pick the tightest lossless mode for a set of numbers (or force [`M_QUANT`]
/// when `lossy`). Integer values → [`M_INT`]; values exactly reconstructable at
/// `/1000` → [`M_QUANT`]; anything else → [`M_F32`].
fn choose_mode(values: &[f32], lossy: bool) -> u8 {
    let all_int = values
        .iter()
        .all(|&v| v.is_finite() && v == (v as i64) as f32);
    if all_int {
        return M_INT;
    }
    let quant_lossless = values.iter().all(|&v| {
        v.is_finite() && {
            let q = (v * QUANT).round();
            q.is_finite() && (q as i64) as f32 / QUANT == v
        }
    });
    if quant_lossless || (lossy && values.iter().all(|v| v.is_finite())) {
        return M_QUANT;
    }
    M_F32
}

fn write_number(out: &mut Vec<u8>, v: f32, mode: u8) {
    match mode {
        M_INT => write_ivarint(out, v as i64),
        M_QUANT => write_ivarint(out, (v * QUANT).round() as i64),
        _ => out.extend_from_slice(&v.to_le_bytes()),
    }
}

/// Write a group of numbers as `mode byte` + payloads. Returns nothing; the
/// caller has already written any tag/count.
fn write_number_group(out: &mut Vec<u8>, values: &[f32], lossy: bool) {
    let mode = choose_mode(values, lossy);
    out.push(mode);
    for &v in values {
        write_number(out, v, mode);
    }
}

fn encode_value(out: &mut Vec<u8>, table: &mut StringTable, v: &Value, lossy: bool) {
    match v {
        Value::Number(n) => {
            let mode = choose_mode(&[*n], lossy);
            out.push(mode); // tag == mode for scalars (T_NUM_INT/QUANT/F32)
            write_number(out, *n, mode);
        }
        Value::Vec3(a) => {
            out.push(T_VEC3);
            write_number_group(out, a, lossy);
        }
        Value::String(s) => {
            out.push(T_STRING);
            write_strref(out, table, s);
        }
        Value::Ident(s) => {
            out.push(T_IDENT);
            write_strref(out, table, s);
        }
        Value::Expr(e) => {
            out.push(T_EXPR);
            encode_expr(out, table, e, lossy);
        }
        Value::List(xs) => {
            out.push(T_LIST_NUM);
            let mode = choose_mode(xs, lossy);
            out.push(mode);
            write_uvarint(out, xs.len() as u64);
            for &x in xs {
                write_number(out, x, mode);
            }
        }
        Value::ListVec3(rows) => {
            out.push(T_LIST_VEC3);
            let flat: Vec<f32> = rows.iter().flatten().copied().collect();
            let mode = choose_mode(&flat, lossy);
            out.push(mode);
            write_uvarint(out, rows.len() as u64);
            for x in &flat {
                write_number(out, *x, mode);
            }
        }
        Value::ListPair(rows) => {
            out.push(T_LIST_PAIR);
            let flat: Vec<f32> = rows.iter().flatten().copied().collect();
            let mode = choose_mode(&flat, lossy);
            out.push(mode);
            write_uvarint(out, rows.len() as u64);
            for x in &flat {
                write_number(out, *x, mode);
            }
        }
        Value::ListQuad(rows) => {
            out.push(T_LIST_QUAD);
            let flat: Vec<f32> = rows.iter().flatten().copied().collect();
            let mode = choose_mode(&flat, lossy);
            out.push(mode);
            write_uvarint(out, rows.len() as u64);
            for x in &flat {
                write_number(out, *x, mode);
            }
        }
        Value::ListString(xs) => {
            out.push(T_LIST_STRING);
            write_uvarint(out, xs.len() as u64);
            for s in xs {
                write_strref(out, table, s);
            }
        }
        Value::Gradient(g) => {
            out.push(T_GRADIENT);
            encode_gradient(out, table, g, lossy);
        }
        Value::FaceList(faces) => {
            out.push(T_FACELIST);
            write_uvarint(out, faces.len() as u64);
            for f in faces {
                write_strref(out, table, &f.mat);
                match &f.uv {
                    None => out.push(0),
                    Some(uv) => {
                        out.push(1);
                        write_number_group(
                            out,
                            &[uv.scale[0], uv.scale[1], uv.offset[0], uv.offset[1]],
                            lossy,
                        );
                        out.push(uv.swap as u8);
                    }
                }
            }
        }
        Value::Vec3Expr(es) => {
            out.push(T_VEC3_EXPR);
            for e in es {
                encode_expr(out, table, e, lossy);
            }
        }
        Value::ListExpr(es) => {
            out.push(T_LIST_EXPR);
            write_uvarint(out, es.len() as u64);
            for e in es {
                encode_expr(out, table, e, lossy);
            }
        }
    }
}

fn encode_gradient(out: &mut Vec<u8>, table: &mut StringTable, g: &GradientDef, lossy: bool) {
    write_strref(out, table, &g.kind);
    write_uvarint(out, g.attrs.len() as u64);
    for (k, v) in &g.attrs {
        write_strref(out, table, k);
        encode_value(out, table, v, lossy);
    }
}

fn encode_expr(out: &mut Vec<u8>, table: &mut StringTable, e: &Expr, lossy: bool) {
    match e {
        Expr::Num(n) => {
            out.push(E_NUM);
            let mode = choose_mode(&[*n], lossy);
            out.push(mode);
            write_number(out, *n, mode);
        }
        Expr::Param(name) => {
            out.push(E_PARAM);
            write_strref(out, table, name);
        }
        Expr::Bin(a, op, b) => {
            out.push(E_BIN);
            encode_expr(out, table, a, lossy);
            encode_expr(out, table, b, lossy);
            out.push(binop_code(*op));
        }
    }
}

fn binop_code(op: mogen_dsl::ast::BinOp) -> u8 {
    use mogen_dsl::ast::BinOp::*;
    match op {
        Add => 0,
        Sub => 1,
        Mul => 2,
        Div => 3,
        Lt => 4,
        Le => 5,
        Gt => 6,
        Ge => 7,
        Eq => 8,
        Ne => 9,
    }
}
