//! MOGB bytes → AST.
//!
//! The inverse of [`crate::encode`]. Decoded nodes carry default spans (the
//! text `.mog` is canonical for source locations); `use_id` / `origin` are
//! `None` because module expansion and import resolution run *after* parsing,
//! downstream of anything this format captures.

use anyhow::{bail, Result};
use mogen_core::Span;
use mogen_dsl::ast::{BinOp, Expr, FaceEntry, FaceUv, GradientDef, Node, Value};

use crate::wire::*;
use crate::{MAGIC, VERSION};

/// Decode MOGB bytes into an AST forest.
pub fn decode(bytes: &[u8]) -> Result<Vec<Node>> {
    let mut r = Reader::new(bytes);
    let magic = r.bytes(4)?;
    if magic != MAGIC {
        bail!("MOGB: bad magic (not a .mogb file)");
    }
    let version = r.u8()?;
    if version != VERSION {
        bail!("MOGB: unsupported version {version} (this build writes v{VERSION})");
    }
    let _flags = r.u8()?; // lossy flag is informational; tags are self-describing

    // String table: preset dictionary followed by the per-file strings.
    let mut strings: Vec<String> = crate::preset::PRESET.iter().map(|s| s.to_string()).collect();
    let local_count = r.uvarint()?;
    for _ in 0..local_count {
        let len = r.uvarint()? as usize;
        let raw = r.bytes(len)?;
        strings.push(String::from_utf8(raw.to_vec()).map_err(|_| {
            anyhow::anyhow!("MOGB: string table entry is not valid UTF-8")
        })?);
    }

    let node_count = r.uvarint()?;
    let mut nodes = Vec::with_capacity(node_count as usize);
    for _ in 0..node_count {
        nodes.push(decode_node(&mut r, &strings)?);
    }
    Ok(nodes)
}

fn strref(r: &mut Reader, strings: &[String]) -> Result<String> {
    let i = r.uvarint()? as usize;
    strings
        .get(i)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("MOGB: string reference {i} out of range"))
}

fn decode_node(r: &mut Reader, strings: &[String]) -> Result<Node> {
    let kind = strref(r, strings)?;
    let flags = r.u8()?;
    let name = if flags & 0b001 != 0 {
        Some(strref(r, strings)?)
    } else {
        None
    };
    let mut attrs = Vec::new();
    if flags & 0b010 != 0 {
        let n = r.uvarint()?;
        attrs.reserve(n as usize);
        for _ in 0..n {
            let key = strref(r, strings)?;
            let value = decode_value(r, strings)?;
            attrs.push((key, value));
        }
    }
    let mut children = Vec::new();
    if flags & 0b100 != 0 {
        let n = r.uvarint()?;
        children.reserve(n as usize);
        for _ in 0..n {
            children.push(decode_node(r, strings)?);
        }
    }
    Ok(Node {
        kind,
        name,
        attrs,
        children,
        span: Span::default(),
        kind_span: Span::default(),
        use_id: None,
        origin: None,
    })
}

fn decode_value(r: &mut Reader, strings: &[String]) -> Result<Value> {
    let tag = r.u8()?;
    Ok(match tag {
        T_NUM_INT => Value::Number(r.number(M_INT)?),
        T_NUM_QUANT => Value::Number(r.number(M_QUANT)?),
        T_NUM_F32 => Value::Number(r.number(M_F32)?),
        T_VEC3 => {
            let mode = r.u8()?;
            Value::Vec3([r.number(mode)?, r.number(mode)?, r.number(mode)?])
        }
        T_STRING => Value::String(strref(r, strings)?),
        T_IDENT => Value::Ident(strref(r, strings)?),
        T_EXPR => Value::Expr(decode_expr(r, strings)?),
        T_LIST_NUM => {
            let mode = r.u8()?;
            let n = r.uvarint()?;
            let mut xs = Vec::with_capacity(n as usize);
            for _ in 0..n {
                xs.push(r.number(mode)?);
            }
            Value::List(xs)
        }
        T_LIST_VEC3 => {
            let mode = r.u8()?;
            let n = r.uvarint()?;
            let mut rows = Vec::with_capacity(n as usize);
            for _ in 0..n {
                rows.push([r.number(mode)?, r.number(mode)?, r.number(mode)?]);
            }
            Value::ListVec3(rows)
        }
        T_LIST_PAIR => {
            let mode = r.u8()?;
            let n = r.uvarint()?;
            let mut rows = Vec::with_capacity(n as usize);
            for _ in 0..n {
                rows.push([r.number(mode)?, r.number(mode)?]);
            }
            Value::ListPair(rows)
        }
        T_LIST_QUAD => {
            let mode = r.u8()?;
            let n = r.uvarint()?;
            let mut rows = Vec::with_capacity(n as usize);
            for _ in 0..n {
                rows.push([r.number(mode)?, r.number(mode)?, r.number(mode)?, r.number(mode)?]);
            }
            Value::ListQuad(rows)
        }
        T_LIST_STRING => {
            let n = r.uvarint()?;
            let mut xs = Vec::with_capacity(n as usize);
            for _ in 0..n {
                xs.push(strref(r, strings)?);
            }
            Value::ListString(xs)
        }
        T_GRADIENT => Value::Gradient(decode_gradient(r, strings)?),
        T_FACELIST => {
            let n = r.uvarint()?;
            let mut faces = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let mat = strref(r, strings)?;
                let uv = if r.u8()? != 0 {
                    let mode = r.u8()?;
                    let scale = [r.number(mode)?, r.number(mode)?];
                    let offset = [r.number(mode)?, r.number(mode)?];
                    let swap = r.u8()? != 0;
                    Some(FaceUv { scale, offset, swap })
                } else {
                    None
                };
                faces.push(FaceEntry {
                    mat,
                    uv,
                    span: Span::default(),
                });
            }
            Value::FaceList(faces)
        }
        T_VEC3_EXPR => Value::Vec3Expr([
            decode_expr(r, strings)?,
            decode_expr(r, strings)?,
            decode_expr(r, strings)?,
        ]),
        T_LIST_EXPR => {
            let n = r.uvarint()?;
            let mut es = Vec::with_capacity(n as usize);
            for _ in 0..n {
                es.push(decode_expr(r, strings)?);
            }
            Value::ListExpr(es)
        }
        other => bail!("MOGB: unknown value tag {other}"),
    })
}

fn decode_gradient(r: &mut Reader, strings: &[String]) -> Result<GradientDef> {
    let kind = strref(r, strings)?;
    let n = r.uvarint()?;
    let mut attrs = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let key = strref(r, strings)?;
        let value = decode_value(r, strings)?;
        attrs.push((key, value));
    }
    Ok(GradientDef {
        kind,
        attrs,
        span: Span::default(),
    })
}

fn decode_expr(r: &mut Reader, strings: &[String]) -> Result<Expr> {
    Ok(match r.u8()? {
        E_NUM => {
            let mode = r.u8()?;
            Expr::Num(r.number(mode)?)
        }
        E_PARAM => Expr::Param(strref(r, strings)?),
        E_BIN => {
            let a = decode_expr(r, strings)?;
            let b = decode_expr(r, strings)?;
            let op = decode_binop(r.u8()?)?;
            Expr::Bin(Box::new(a), op, Box::new(b))
        }
        other => bail!("MOGB: unknown expr opcode {other}"),
    })
}

fn decode_binop(code: u8) -> Result<BinOp> {
    Ok(match code {
        0 => BinOp::Add,
        1 => BinOp::Sub,
        2 => BinOp::Mul,
        3 => BinOp::Div,
        4 => BinOp::Lt,
        5 => BinOp::Le,
        6 => BinOp::Gt,
        7 => BinOp::Ge,
        8 => BinOp::Eq,
        9 => BinOp::Ne,
        other => bail!("MOGB: unknown binop {other}"),
    })
}
