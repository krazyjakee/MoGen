//! Span / byte-scanning helpers shared across all `edit::*` op modules.
//! Intentionally small and dependency-free so each op file can import what
//! it needs without cycles.

use mogen_core::Span;

pub(super) fn clamp_span(src: &str, span: Span) -> (usize, usize) {
    let s = span.start.min(src.len());
    let e = span.end.min(src.len()).max(s);
    (s, e)
}

pub(super) fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

pub(super) fn match_closing_paren(src: &str, open_idx: usize, limit: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth: i32 = 0;
    let mut i = open_idx;
    let mut in_string = false;
    while i < limit {
        let c = bytes[i];
        if in_string {
            if c == b'\\' && i + 1 < limit {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'(' | b'[' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Walk past leading whitespace, blank lines, and `//` comment lines so a
/// freshly-inserted top-level declaration lands after any seed/header comment
/// rather than ahead of it.
pub(super) fn skip_leading_comments_and_blanks(src: &str) -> usize {
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' {
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        break;
    }
    i
}

pub(super) fn splice(src: &str, range: std::ops::Range<usize>, replacement: &str) -> String {
    let mut out = String::with_capacity(src.len() + replacement.len());
    out.push_str(&src[..range.start]);
    out.push_str(replacement);
    out.push_str(&src[range.end..]);
    out
}
