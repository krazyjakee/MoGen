//! Compact, grammar-safe number formatting.
//!
//! The `.mog` grammar has no exponent syntax, so `{}` on an `f32` is not safe
//! (`1e-7` would not parse). Everything goes through fixed precision with
//! trailing zeros trimmed, and `-0` folded to `0` so a value that rounds to
//! zero never differs from one that was zero.

use std::fmt::Write;

pub(crate) fn num(v: f32, precision: usize) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let mut s = format!("{:.*}", precision, v);
    if s.contains('.') {
        let trimmed = s.trim_end_matches('0').trim_end_matches('.').len();
        s.truncate(trimmed);
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

/// `[a, b, c]`
pub(crate) fn vec<const N: usize>(v: [f32; N], precision: usize) -> String {
    let parts: Vec<String> = v.iter().map(|&c| num(c, precision)).collect();
    format!("[{}]", parts.join(", "))
}

/// `[[a, b, c], [d, e, f], …]`
pub(crate) fn vec_list<const N: usize>(items: &[[f32; N]], precision: usize) -> String {
    let mut out = String::with_capacity(items.len() * N * 8);
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&vec(*item, precision));
    }
    out.push(']');
    out
}

/// `[0, 1, 2, …]`
pub(crate) fn index_list(indices: &[u32]) -> String {
    let mut out = String::with_capacity(indices.len() * 6);
    out.push('[');
    for (i, idx) in indices.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{idx}");
    }
    out.push(']');
    out
}

/// A `.mog` string literal: the grammar's escape is a backslash before any
/// character, so only `\` and `"` need it.
pub(crate) fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_compact_and_never_use_exponents() {
        assert_eq!(num(1.0, 5), "1");
        assert_eq!(num(0.5, 5), "0.5");
        assert_eq!(num(-0.000001, 5), "0");
        assert_eq!(num(1e-7, 5), "0");
        assert_eq!(num(123456.0, 5), "123456");
        assert_eq!(num(0.123456, 5), "0.12346");
        assert_eq!(num(f32::NAN, 5), "0");
        assert_eq!(num(2.5, 0), "2");
    }

    #[test]
    fn strings_escape_quotes_and_backslashes() {
        assert_eq!(string(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn lists_format_as_nested_brackets() {
        assert_eq!(
            vec_list(&[[0.0, 1.0], [0.25, 0.5]], 5),
            "[[0, 1], [0.25, 0.5]]"
        );
        assert_eq!(index_list(&[0, 1, 2]), "[0, 1, 2]");
    }
}
