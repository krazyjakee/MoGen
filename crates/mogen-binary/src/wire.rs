//! Shared wire primitives: LEB128 varints, zig-zag, the value-tag / number-mode
//! byte constants, and a bounds-checked [`Reader`]. Kept in one place so the
//! encoder and decoder can never drift on the byte layout.

use anyhow::{bail, Result};

// ── value tags (one byte, precedes every attribute value payload) ───────────
pub const T_NUM_INT: u8 = 0; // scalar number, exact integer  → ivarint
pub const T_NUM_QUANT: u8 = 1; // scalar number, /1000 fixed   → ivarint
pub const T_NUM_F32: u8 = 2; // scalar number, raw            → 4 bytes LE
pub const T_VEC3: u8 = 3; // mode + 3 numbers
pub const T_STRING: u8 = 4; // strref
pub const T_IDENT: u8 = 5; // strref
pub const T_EXPR: u8 = 6; // expr tree
pub const T_LIST_NUM: u8 = 7; // mode + count + numbers      (Value::List)
pub const T_LIST_VEC3: u8 = 8; // mode + count + count*3
pub const T_LIST_PAIR: u8 = 9; // mode + count + count*2
pub const T_LIST_QUAD: u8 = 10; // mode + count + count*4
pub const T_LIST_STRING: u8 = 11; // count + count*strref
pub const T_GRADIENT: u8 = 12; // strref(kind) + attr block
pub const T_FACELIST: u8 = 13; // count + entries
pub const T_VEC3_EXPR: u8 = 14; // 3 exprs
pub const T_LIST_EXPR: u8 = 15; // count + exprs

// ── number container modes (one byte, applies to every number in a group) ───
pub const M_INT: u8 = 0; // each: ivarint of the exact integer value
pub const M_QUANT: u8 = 1; // each: ivarint of round(v * 1000)
pub const M_F32: u8 = 2; // each: 4 bytes LE

// ── expr opcodes ────────────────────────────────────────────────────────────
pub const E_NUM: u8 = 0; // mode byte + number payload
pub const E_PARAM: u8 = 1; // strref
pub const E_BIN: u8 = 2; // <lhs expr><rhs expr><binop byte>

/// Quantisation scale for [`M_QUANT`] / lossy mode: three decimal places.
pub const QUANT: f32 = 1000.0;

// ── varint writers ──────────────────────────────────────────────────────────
pub fn write_uvarint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

pub fn write_ivarint(out: &mut Vec<u8>, v: i64) {
    // zig-zag: small-magnitude negatives stay small.
    write_uvarint(out, ((v << 1) ^ (v >> 63)) as u64);
}

/// Byte length `write_uvarint` would emit for `v` — lets the encoder size the
/// compressed branch exactly before committing to it.
pub fn uvarint_len(mut v: u64) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

/// Cursor over a byte slice with bounds-checked reads. Every read returns a
/// `Result` so a truncated or malformed `.mogb` is a clean error, never a panic.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn u8(&mut self) -> Result<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| anyhow::anyhow!("MOGB: unexpected end of input"))?;
        self.pos += 1;
        Ok(b)
    }

    /// The not-yet-consumed tail of the input.
    pub fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.buf.len())
            .ok_or_else(|| anyhow::anyhow!("MOGB: unexpected end of input"))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn uvarint(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            if shift >= 64 {
                bail!("MOGB: varint overflow");
            }
            let byte = self.u8()?;
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        Ok(result)
    }

    pub fn ivarint(&mut self) -> Result<i64> {
        let u = self.uvarint()?;
        Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
    }

    pub fn f32(&mut self) -> Result<f32> {
        let b = self.bytes(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read one number given a container `mode` byte.
    pub fn number(&mut self, mode: u8) -> Result<f32> {
        Ok(match mode {
            M_INT => self.ivarint()? as f32,
            M_QUANT => self.ivarint()? as f32 / QUANT,
            M_F32 => self.f32()?,
            other => bail!("MOGB: bad number mode {other}"),
        })
    }
}
