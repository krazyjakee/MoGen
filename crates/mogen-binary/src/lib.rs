//! MOGB — an experimental schema-aware binary container for `.mog` sources.
//!
//! # Why this exists
//!
//! Generic compressors (gzip/zstd) have to *learn* a `.mog` file's structure
//! from the bytes: they rediscover that `cylinder` and `radius` recur, and they
//! cannot touch float mantissas at all. MOGB exploits two things they can't:
//!
//! 1. **A shared, versioned dictionary.** Every node kind and common attribute
//!    key is a keyword from a fixed vocabulary ([`preset`]). Those strings ship
//!    *with the decoder*, so the first occurrence of `cylinder` costs one varint
//!    index and zero bytes of `"cylinder"`. Strings outside the preset are
//!    interned once into a per-file table and referenced by index thereafter.
//! 2. **Semantic float coding.** Authored numbers are overwhelmingly round and
//!    metric (`0.3`, `1.2`, `90`). MOGB stores each as the smallest exact form —
//!    zig-zag varint integer, `/1000` fixed-point, or (fallback) raw `f32` — so
//!    round values collapse to 1–2 bytes instead of 4 bytes of mantissa noise.
//!
//! # What it encodes
//!
//! The **parsed AST** ([`mogen_dsl::ast::Node`]), not the lowered `SceneGraph`.
//! That keeps the DSL's semantics intact — seeds, modules, procedural nodes,
//! editability — and lets a `.mogb` round-trip straight back to `.mog` text and
//! feed the existing lowering pipeline unchanged. Spans and comments are *not*
//! preserved (the text `.mog` stays canonical); decoded nodes carry default
//! spans, which only affects diagnostic locations if you build from a `.mogb`.
//!
//! # Modes
//!
//! [`encode`] is **lossless**: every `f32` round-trips bit-for-bit (round values
//! via varint, everything else via raw `f32`). [`encode_lossy`] forces `/1000`
//! fixed-point on all numbers — smaller, at ~3 decimal places of precision. Use
//! lossless unless you have measured that the size win is worth the rounding.

use anyhow::Result;
use mogen_dsl::ast::Node;

mod decode;
mod encode;
pub mod preset;
mod print;
mod wire;

pub use decode::decode;
pub use encode::{encode, encode_lossy};
pub use print::to_mog_text;

/// File magic: `MOGB` in ASCII.
pub(crate) const MAGIC: &[u8; 4] = b"MOGB";
/// Format version. Bump whenever the byte layout or [`preset::PRESET`] changes;
/// the decoder rejects versions it does not understand.
pub(crate) const VERSION: u8 = 1;

/// Header flag: numbers were written in lossy `/1000` fixed-point mode. Purely
/// informational on decode (the per-value tags are self-describing).
pub(crate) const FLAG_LOSSY: u8 = 0b0000_0001;

/// Convenience: parse `src` to an AST and encode it (lossless).
pub fn pack_source(src: &str) -> Result<Vec<u8>> {
    let ast = mogen_dsl::parse(src)?;
    Ok(encode(&ast))
}

/// Convenience: decode `bytes` to an AST, then render it back to `.mog` text.
pub fn unpack_to_source(bytes: &[u8]) -> Result<String> {
    let ast = decode(bytes)?;
    Ok(to_mog_text(&ast))
}

/// Structural equality of two AST forests, **ignoring spans / `use_id` /
/// `origin`** (which encoding intentionally drops). Used by the round-trip
/// tests and available to callers that want to verify fidelity.
pub fn nodes_equivalent(a: &[Node], b: &[Node]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| node_equiv(x, y))
}

fn node_equiv(a: &Node, b: &Node) -> bool {
    a.kind == b.kind
        && a.name == b.name
        && a.attrs.len() == b.attrs.len()
        && a.attrs
            .iter()
            .zip(&b.attrs)
            .all(|((ka, va), (kb, vb))| ka == kb && value_equiv(va, vb))
        && nodes_equivalent(&a.children, &b.children)
}

fn value_equiv(a: &mogen_dsl::ast::Value, b: &mogen_dsl::ast::Value) -> bool {
    use mogen_dsl::ast::Value::*;
    // f32 compared by bits so NaN and -0.0 round-trip checks stay honest in
    // lossless mode. Lists compare element-wise the same way.
    let bits = |xs: &[f32]| xs.iter().map(|f| f.to_bits()).collect::<Vec<_>>();
    match (a, b) {
        (Number(x), Number(y)) => x.to_bits() == y.to_bits(),
        (Vec3(x), Vec3(y)) => bits(x) == bits(y),
        (String(x), String(y)) => x == y,
        (Ident(x), Ident(y)) => x == y,
        (Expr(x), Expr(y)) => expr_equiv(x, y),
        (Vec3Expr(x), Vec3Expr(y)) => x.iter().zip(y).all(|(p, q)| expr_equiv(p, q)),
        (List(x), List(y)) => bits(x) == bits(y),
        (ListExpr(x), ListExpr(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| expr_equiv(p, q))
        }
        (ListVec3(x), ListVec3(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| bits(p) == bits(q))
        }
        (ListPair(x), ListPair(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| bits(p) == bits(q))
        }
        (ListQuad(x), ListQuad(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| bits(p) == bits(q))
        }
        (ListString(x), ListString(y)) => x == y,
        (Gradient(x), Gradient(y)) => {
            x.kind == y.kind
                && x.attrs.len() == y.attrs.len()
                && x.attrs
                    .iter()
                    .zip(&y.attrs)
                    .all(|((ka, va), (kb, vb))| ka == kb && value_equiv(va, vb))
        }
        (FaceList(x), FaceList(y)) => {
            x.len() == y.len()
                && x.iter().zip(y).all(|(p, q)| {
                    p.mat == q.mat
                        && match (p.uv, q.uv) {
                            (None, None) => true,
                            (Some(u), Some(v)) => {
                                bits(&u.scale) == bits(&v.scale)
                                    && bits(&u.offset) == bits(&v.offset)
                                    && u.swap == v.swap
                            }
                            _ => false,
                        }
                })
        }
        _ => false,
    }
}

fn expr_equiv(a: &mogen_dsl::ast::Expr, b: &mogen_dsl::ast::Expr) -> bool {
    use mogen_dsl::ast::Expr::*;
    match (a, b) {
        (Num(x), Num(y)) => x.to_bits() == y.to_bits(),
        (Param(x), Param(y)) => x == y,
        (Bin(la, oa, ra), Bin(lb, ob, rb)) => {
            oa == ob && expr_equiv(la, lb) && expr_equiv(ra, rb)
        }
        _ => false,
    }
}
