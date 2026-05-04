//! Top-level `lod_scale (value=…)` accessors used by the studio LOD slider.
//! Read-modify-write the directive in place so any user-authored `lod_scale`
//! line keeps its position in the file rather than getting demoted to the
//! bottom on every slider tick.

use mogen_core::Span;

use super::attr::set_attr;
use super::internals::{is_ident_byte, match_closing_paren, skip_leading_comments_and_blanks, splice};
use super::node::delete_node;

/// Read the current top-level `lod_scale (value=…)` from `src`. Returns
/// `None` when no declaration exists or the value isn't parseable. The studio
/// slider uses this to seed itself from whatever's already in the source.
pub fn get_lod_scale(src: &str) -> Option<f32> {
    let (start, end) = find_top_level_decl(src, "lod_scale")?;
    let body = &src[start..end];
    let open = body.find('(')?;
    let close = body.rfind(')')?;
    let inner = &body[open + 1..close];
    for chunk in inner.split(',') {
        let mut parts = chunk.splitn(2, '=');
        let k = parts.next()?.trim();
        let v = parts.next()?.trim();
        if k == "value" {
            return v.parse::<f32>().ok();
        }
    }
    None
}

/// Update the top-level `lod_scale (value=…)` declaration to `scale`. Inserts
/// one (right after any leading `//` header) when absent and removes any
/// existing one when `scale == 1.0` so the no-op case keeps the source clean.
pub fn set_lod_scale(src: &str, scale: f32) -> String {
    let is_default = (scale - 1.0).abs() < 1e-6;
    if let Some((start, end)) = find_top_level_decl(src, "lod_scale") {
        let span = Span::new(start, end);
        if is_default {
            return delete_node(src, span);
        }
        return set_attr(src, span, "value", &format_scale(scale));
    }
    if is_default {
        return src.to_string();
    }
    let insert_at = skip_leading_header(src);
    let payload = format!("lod_scale (value={})\n\n", format_scale(scale));
    splice(src, insert_at..insert_at, &payload)
}

/// Skip past leading comments + blank lines and any top-level `meta(...)`
/// block so a freshly-inserted directive lands *after* the meta header rather
/// than ahead of it.
fn skip_leading_header(src: &str) -> usize {
    let mut i = skip_leading_comments_and_blanks(src);
    let bytes = src.as_bytes();
    if src[i..].starts_with("meta")
        && bytes
            .get(i + 4)
            .map(|c| !is_ident_byte(*c))
            .unwrap_or(true)
    {
        // Skip the kind ident + any whitespace before the `(`.
        let mut j = i + 4;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'(' {
            if let Some(close) = match_closing_paren(src, j, src.len()) {
                let mut k = close + 1;
                while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b'\n' {
                    k += 1;
                }
                while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t' || bytes[k] == b'\n' || bytes[k] == b'\r') {
                    k += 1;
                }
                i = k;
            }
        }
    }
    i
}

fn format_scale(scale: f32) -> String {
    // `{}` already trims trailing zeros for `f32` — 2.0 → "2", 0.5 → "0.5",
    // 0.25 → "0.25". Avoid a fixed precision that would leave "1.5000".
    format!("{}", scale)
}

/// Locate the first top-level (depth=0, not inside any brace/paren/bracket
/// or string/comment) declaration of the given `kind`. Returns the byte range
/// from the start of the kind ident through the closing `)` of its header
/// (or end-of-ident when no header is present). Used by `set_lod_scale` to
/// find an existing `lod_scale (value=…)` directive.
fn find_top_level_decl(src: &str, kind: &str) -> Option<(usize, usize)> {
    let bytes = src.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_comment = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_comment {
            if c == b'\n' {
                in_comment = false;
            }
            i += 1;
            continue;
        }
        if in_string {
            if c == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_comment = true;
            i += 2;
            continue;
        }
        match c {
            b'"' => {
                in_string = true;
                i += 1;
            }
            b'{' | b'(' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b')' | b']' => {
                depth -= 1;
                i += 1;
            }
            _ if depth == 0 && (c.is_ascii_alphabetic() || c == b'_') => {
                let kstart = i;
                while i < bytes.len() && is_ident_byte(bytes[i]) {
                    i += 1;
                }
                if &src[kstart..i] != kind {
                    continue;
                }
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'"' {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' && i + 1 < bytes.len() {
                            i += 2;
                            continue;
                        }
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                }
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'(' {
                    let close = match_closing_paren(src, i, bytes.len())?;
                    return Some((kstart, close + 1));
                }
                return Some((kstart, i));
            }
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_lod_scale_inserts_at_top_when_absent() {
        let src = "scene {\n  box \"b\" (size=1)\n}\n";
        let out = set_lod_scale(src, 0.5);
        assert!(out.starts_with("lod_scale (value=0.5)\n\nscene {"), "got: {out}");
    }

    #[test]
    fn set_lod_scale_keeps_meta_header_at_top() {
        let src = "meta (seed = \"42\")\n\nscene {\n  box \"b\" (size=1)\n}\n";
        let out = set_lod_scale(src, 1.5);
        assert!(out.starts_with("meta (seed = \"42\")"), "header demoted: {out}");
        assert!(out.contains("lod_scale (value=1.5)"), "lod_scale missing: {out}");
        let meta_pos = out.find("meta (").unwrap();
        let lod_pos = out.find("lod_scale").unwrap();
        let scene_pos = out.find("scene {").unwrap();
        assert!(meta_pos < lod_pos && lod_pos < scene_pos);
    }

    #[test]
    fn set_lod_scale_updates_existing_value() {
        let src = "lod_scale (value=2)\n\nscene {\n  box \"b\" (size=1)\n}\n";
        let out = set_lod_scale(src, 0.25);
        assert!(out.contains("lod_scale (value=0.25)"), "value not updated: {out}");
        assert_eq!(out.matches("lod_scale").count(), 1, "should still have exactly one decl");
    }

    #[test]
    fn set_lod_scale_one_removes_existing_decl() {
        let src = "lod_scale (value=0.5)\n\nscene {\n  box \"b\" (size=1)\n}\n";
        let out = set_lod_scale(src, 1.0);
        assert!(!out.contains("lod_scale"), "decl should be removed: {out}");
    }

    #[test]
    fn set_lod_scale_one_on_empty_scene_is_noop() {
        let src = "scene {\n  box \"b\" (size=1)\n}\n";
        let out = set_lod_scale(src, 1.0);
        assert_eq!(out, src);
    }

    #[test]
    fn set_lod_scale_roundtrips_through_compile() {
        // Inserting a directive must produce source that compiles cleanly and
        // actually scales the mesh — the studio slider relies on this.
        let src = "scene {\n  sphere \"s\" (radius=0.5)\n}\n";
        let baseline = crate::pipeline::compile(src, None);
        let baseline_verts = baseline
            .scene
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.name == "s")
            .unwrap()
            .mesh
            .as_ref()
            .unwrap()
            .positions
            .len();

        let scaled_src = set_lod_scale(src, 0.5);
        let scaled = crate::pipeline::compile(&scaled_src, None);
        assert!(
            matches!(scaled.stage, crate::pipeline::Stage::Ok),
            "scaled scene should compile: stage={:?} diags={:?}",
            scaled.stage,
            scaled.diagnostics
        );
        let scaled_verts = scaled
            .scene
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.name == "s")
            .unwrap()
            .mesh
            .as_ref()
            .unwrap()
            .positions
            .len();
        assert!(scaled_verts < baseline_verts, "lod_scale=0.5 should reduce sphere verts");
    }
}
