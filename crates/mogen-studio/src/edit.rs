//! Span-aware `.mog` text mutations used by the viewport editor. Operates on
//! raw source strings and byte ranges (the `Span` already carried on every
//! `SceneNode`) so diagnostics and formatting survive untouched — a full AST
//! round-trip would normalise whitespace and lose the user's formatting.
//!
//! All three functions return the full new source. `set_attr` is the hot path
//! (gizmos + inspector fields) and is exercised heavily by the tests; bugs
//! here cause silent DSL corruption.

use mogen_core::Span;

/// Set or insert an attribute on the node covered by `span`. If the attribute
/// already exists on that node, its value is replaced up to the next
/// top-level comma or the closing `)`. If not, ` name=val` is appended just
/// before the header's closing `)`. When the node has no attribute list at
/// all, one is synthesised right after the node's name/kind.
pub fn set_attr(src: &str, span: Span, name: &str, value: &str) -> String {
    let (start, end) = clamp_span(src, span);
    let Some((hdr_open, hdr_close)) = find_header_parens(src, start, end) else {
        // No `(...)` — insert one right after the name/kind.
        let insert_at = find_attr_insert_point(src, start, end);
        return splice(src, insert_at..insert_at, &format!(" ({name}={value})"));
    };

    if let Some((kstart, vend)) = find_attr_in_header(src, hdr_open + 1, hdr_close, name) {
        return splice(src, kstart..vend, &format!("{name}={value}"));
    }

    // Append before closing `)`. Preserve leading spacing: if the header
    // already has trailing content immediately before `)`, prepend ", ";
    // otherwise just the attr.
    let body = &src[hdr_open + 1..hdr_close];
    let trimmed = body.trim_end();
    let has_content = !trimmed.is_empty();
    let prefix = if has_content { ", " } else { "" };
    // Insert just after the last non-whitespace byte (skips trailing commas
    // and newlines inside multiline headers so the new attr stays flush with
    // the existing last attr rather than getting pushed past a `\n    )`
    // indent.)
    let insert_at = hdr_open + 1 + trimmed.len();
    splice(src, insert_at..insert_at, &format!("{prefix}{name}={value}"))
}

/// Remove a single `name=value` attribute from the header of the node covered
/// by `span`. No-op when the attribute is absent. Eats the leading comma + any
/// whitespace so the neighbouring attrs close up cleanly; when the removed
/// attr was the first in the list we eat the trailing comma instead. The
/// gizmo commit path calls this to strip DSL shortcut attrs (`x`/`y`/`z`,
/// `rx`/`ry`/`rz`, `from`/`to`) that would otherwise shadow the canonical
/// `pos=`/`rot=` writeback on recompile.
pub fn delete_attr(src: &str, span: Span, name: &str) -> String {
    let (start, end) = clamp_span(src, span);
    let Some((hdr_open, hdr_close)) = find_header_parens(src, start, end) else {
        return src.to_string();
    };
    let Some((kstart, vend)) = find_attr_in_header(src, hdr_open + 1, hdr_close, name) else {
        return src.to_string();
    };
    // Expand the deletion to swallow a neighbouring separator so we don't
    // leave `(, size=…)` or `(size=…, )` behind. Prefer eating a leading
    // comma (we're in the middle of a list); otherwise eat a trailing comma
    // (we were the first attr).
    let bytes = src.as_bytes();
    let body_start = hdr_open + 1;
    let mut rm_start = kstart;
    while rm_start > body_start && bytes[rm_start - 1].is_ascii_whitespace() {
        rm_start -= 1;
    }
    let mut rm_end = vend;
    if rm_start > body_start && bytes[rm_start - 1] == b',' {
        rm_start -= 1;
    } else {
        while rm_end < hdr_close && bytes[rm_end].is_ascii_whitespace() {
            rm_end += 1;
        }
        if rm_end < hdr_close && bytes[rm_end] == b',' {
            rm_end += 1;
            while rm_end < hdr_close && bytes[rm_end] == b' ' {
                rm_end += 1;
            }
        } else {
            // We didn't take a trailing separator, so restore the leading
            // whitespace we ate — it was attached to the attr, not the
            // list separator.
            rm_start = kstart;
        }
    }
    splice(src, rm_start..rm_end, "")
}

/// Remove the node covered by `span` along with its leading indentation on
/// the same line and a single trailing newline. No-ops if the span is empty
/// or out of range.
pub fn delete_node(src: &str, span: Span) -> String {
    let (start, end) = clamp_span(src, span);
    if start >= end {
        return src.to_string();
    }
    let rm_start = expand_to_line_start(src, start);
    let rm_end = expand_to_newline(src, end);
    splice(src, rm_start..rm_end, "")
}

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
    let insert_at = skip_leading_comments_and_blanks(src);
    let payload = format!("lod_scale (value={})\n\n", format_scale(scale));
    splice(src, insert_at..insert_at, &payload)
}

fn format_scale(scale: f32) -> String {
    // `{}` already trims trailing zeros for `f32` — 2.0 → "2", 0.5 → "0.5",
    // 0.25 → "0.25". Avoid a fixed precision that would leave "1.5000".
    format!("{}", scale)
}

/// Insert `import "<path>"` lines for each entry in `paths` near the top of
/// `src`. Paths already present (matched as quoted-string literals) are
/// skipped so re-running the import dialog with the same selection is a
/// no-op. New imports land after any contiguous run of existing `import`
/// lines; if there are none, they go after the leading comment/blank header.
/// Each inserted line ends with `\n`; if the surrounding source needs a
/// blank line of separation, the caller is responsible for it.
pub fn insert_imports(src: &str, paths: &[&str]) -> String {
    let mut new_paths: Vec<&str> = Vec::with_capacity(paths.len());
    for p in paths {
        let needle = format!("\"{p}\"");
        let already = src
            .lines()
            .filter(|l| l.trim_start().starts_with("import"))
            .any(|l| l.contains(&needle));
        if !already && !new_paths.iter().any(|q| q == p) {
            new_paths.push(p);
        }
    }
    if new_paths.is_empty() {
        return src.to_string();
    }

    let mut payload = String::new();
    for p in &new_paths {
        payload.push_str(&format!("import \"{p}\"\n"));
    }

    // Find the end of the contiguous run of existing `import` lines starting
    // after the file's comment/blank header. If no run exists, insert a blank
    // line after the new block so it doesn't fuse with the next declaration.
    let header_end = skip_leading_comments_and_blanks(src);
    let mut cursor = header_end;
    let bytes = src.as_bytes();
    let mut had_existing_imports = false;
    loop {
        // Eat blank lines and comment lines between the cursor and the next
        // candidate line so a header like `// note\n\nimport "a.mog"` keeps
        // the cursor advancing through the imports block.
        let next = skip_blank_and_comment_lines(src, cursor);
        let line_end = next_line_end(bytes, next);
        let line = &src[next..line_end];
        if line.trim_start().starts_with("import") {
            had_existing_imports = true;
            cursor = if line_end < bytes.len() { line_end + 1 } else { line_end };
        } else {
            break;
        }
    }

    let insert_at = cursor;
    let separator = if had_existing_imports { "" } else { "\n" };
    splice(src, insert_at..insert_at, &format!("{payload}{separator}"))
}

fn skip_blank_and_comment_lines(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    while i < bytes.len() {
        let line_end = next_line_end(bytes, i);
        let line = &src[i..line_end];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            i = if line_end < bytes.len() { line_end + 1 } else { line_end };
        } else {
            break;
        }
    }
    i
}

fn next_line_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

/// Duplicate the node covered by `span`, inserting the copy immediately
/// after the original on its own line. The copy inherits the original's
/// leading indentation. Returns the new source.
pub fn duplicate_node(src: &str, span: Span) -> String {
    let (start, end) = clamp_span(src, span);
    if start >= end {
        return src.to_string();
    }
    let indent_start = expand_to_line_start(src, start);
    let indent = &src[indent_start..start];
    let body = &src[start..end];
    let insert = format!("\n{indent}{body}");
    splice(src, end..end, &insert)
}

// --- internals ---------------------------------------------------------------

fn clamp_span(src: &str, span: Span) -> (usize, usize) {
    let s = span.start.min(src.len());
    let e = span.end.min(src.len()).max(s);
    (s, e)
}

/// Scan within `[start..end)` for the header's outer `(...)` pair, returning
/// `(open_index, close_index)` (both pointing at the literal paren bytes).
/// Only the FIRST top-level `(` is treated as the header opener; anything
/// nested (`from=[...]`) is ignored via bracket tracking.
fn find_header_parens(src: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let bytes = src.as_bytes();
    let mut i = start;
    // Walk forward from the node start, skipping over the kind ident, the
    // optional quoted name, and any whitespace — the first `(` or `{` we hit
    // either opens the header or is the body.
    let mut in_string = false;
    while i < end {
        let c = bytes[i];
        if in_string {
            if c == b'\\' && i + 1 < end {
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
            b'"' => {
                in_string = true;
                i += 1;
            }
            b'{' => return None, // body before any paren header
            b'(' => {
                let close = match_closing_paren(src, i, end)?;
                return Some((i, close));
            }
            _ => i += 1,
        }
    }
    None
}

fn match_closing_paren(src: &str, open_idx: usize, limit: usize) -> Option<usize> {
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

/// Locate `name=…` inside the header body `[body_start..body_end)` (exclusive
/// of the paren characters themselves). Returns `(start_of_name, end_of_value)`
/// where the value terminates at the first top-level `,` or the body end.
/// Nested brackets (`[…]`) are skipped so `from=[1,2,3]` doesn't trick the
/// comma scan into cutting mid-list.
fn find_attr_in_header(
    src: &str,
    body_start: usize,
    body_end: usize,
    name: &str,
) -> Option<(usize, usize)> {
    let bytes = src.as_bytes();
    let mut i = body_start;
    while i < body_end {
        skip_ws_and_commas(bytes, &mut i, body_end);
        if i >= body_end {
            return None;
        }
        let key_start = i;
        while i < body_end && is_ident_byte(bytes[i]) {
            i += 1;
        }
        let key_end = i;
        if key_start == key_end {
            // Unexpected character — bail to avoid infinite loop.
            i += 1;
            continue;
        }
        let key = &src[key_start..key_end];
        // Skip whitespace between key and `=`.
        while i < body_end && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= body_end || bytes[i] != b'=' {
            // Key without `=` — treat as a value token, skip it.
            continue;
        }
        let eq_idx = i;
        i += 1;
        // Skip WS after `=`.
        while i < body_end && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // Advance through the value. Values may be scalars, strings, vec3
        // literals, or nested lists — track brackets and quotes.
        let mut depth: i32 = 0;
        let mut in_string = false;
        while i < body_end {
            let c = bytes[i];
            if in_string {
                if c == b'\\' && i + 1 < body_end {
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
                b'[' | b'(' => depth += 1,
                b']' | b')' => depth -= 1,
                b',' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        let val_end = trim_trailing_ws_end(bytes, eq_idx + 1, i);
        if key == name {
            return Some((key_start, val_end));
        }
    }
    None
}

fn skip_ws_and_commas(bytes: &[u8], i: &mut usize, end: usize) {
    while *i < end {
        let c = bytes[*i];
        if c.is_ascii_whitespace() || c == b',' {
            *i += 1;
            continue;
        }
        // Line comments are rare inside a header but survive if present.
        if c == b'/' && *i + 1 < end && bytes[*i + 1] == b'/' {
            while *i < end && bytes[*i] != b'\n' {
                *i += 1;
            }
            continue;
        }
        break;
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn trim_trailing_ws_end(bytes: &[u8], start: usize, mut end: usize) -> usize {
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    end
}

/// Where to insert a synthetic `(attr=val)` header for a node that has no
/// attribute list yet. Walk past the kind ident and optional quoted name
/// and stop at the first whitespace / body character.
fn find_attr_insert_point(src: &str, start: usize, end: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = start;
    // Skip kind ident.
    while i < end && is_ident_byte(bytes[i]) {
        i += 1;
    }
    // Skip whitespace.
    while i < end && bytes[i].is_ascii_whitespace() && bytes[i] != b'\n' {
        i += 1;
    }
    // Optional quoted name.
    if i < end && bytes[i] == b'"' {
        i += 1;
        while i < end {
            let c = bytes[i];
            if c == b'\\' && i + 1 < end {
                i += 2;
                continue;
            }
            if c == b'"' {
                i += 1;
                break;
            }
            i += 1;
        }
    }
    i
}

fn expand_to_line_start(src: &str, pos: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = pos;
    while i > 0 {
        let c = bytes[i - 1];
        if c == b' ' || c == b'\t' {
            i -= 1;
            continue;
        }
        break;
    }
    i
}

fn expand_to_newline(src: &str, pos: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = pos;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\n' {
        i += 1;
    }
    i
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

/// Walk past leading whitespace, blank lines, and `//` comment lines so a
/// freshly-inserted top-level declaration lands after any seed/header comment
/// rather than ahead of it.
fn skip_leading_comments_and_blanks(src: &str) -> usize {
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

fn splice(src: &str, range: std::ops::Range<usize>, replacement: &str) -> String {
    let mut out = String::with_capacity(src.len() + replacement.len());
    out.push_str(&src[..range.start]);
    out.push_str(replacement);
    out.push_str(&src[range.end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_of(src: &str, needle: &str) -> Span {
        let start = src.find(needle).expect("needle not found");
        Span::new(start, start + needle.len())
    }

    #[test]
    fn set_attr_replaces_existing_vec3() {
        let src = "scene {\n  box \"b\" (pos=[0,0,0], size=[1,1,1])\n}\n";
        let span = span_of(src, "box \"b\" (pos=[0,0,0], size=[1,1,1])");
        let out = set_attr(src, span, "pos", "[1,2,3]");
        assert!(
            out.contains("pos=[1,2,3]"),
            "missing replaced pos: {out}"
        );
        assert!(out.contains("size=[1,1,1]"), "size preserved: {out}");
    }

    #[test]
    fn set_attr_replaces_scalar() {
        let src = "box \"b\" (x=0.5, y=1.0)";
        let span = span_of(src, "box \"b\" (x=0.5, y=1.0)");
        let out = set_attr(src, span, "x", "2.5");
        assert_eq!(out, "box \"b\" (x=2.5, y=1.0)");
    }

    #[test]
    fn set_attr_inserts_when_absent() {
        let src = "box \"b\" (x=1)";
        let span = span_of(src, "box \"b\" (x=1)");
        let out = set_attr(src, span, "y", "2");
        assert_eq!(out, "box \"b\" (x=1, y=2)");
    }

    #[test]
    fn set_attr_creates_header_when_none() {
        let src = "box \"b\" { }";
        let span = span_of(src, "box \"b\" { }");
        let out = set_attr(src, span, "x", "3");
        assert!(
            out.starts_with("box \"b\" (x=3)"),
            "expected header inserted, got {out}"
        );
    }

    #[test]
    fn set_attr_preserves_nested_brackets() {
        // wall holes is a list of 4-element sublists — replacing `x` must not
        // confuse its comma scan with the inner commas.
        let src = "wall \"w\" (x=0, holes=[[-1, -1, 1, 1], [0, 0, 2, 2]], z=0)";
        let span = span_of(src, "wall \"w\" (x=0, holes=[[-1, -1, 1, 1], [0, 0, 2, 2]], z=0)");
        let out = set_attr(src, span, "x", "9");
        assert!(out.contains("x=9"));
        assert!(out.contains("holes=[[-1, -1, 1, 1], [0, 0, 2, 2]]"));
        assert!(out.contains("z=0"));
    }

    #[test]
    fn set_attr_only_targets_the_requested_node() {
        // Two nodes with the same attr name: editing the second must not
        // touch the first.
        let src = "scene {\n  box \"a\" (x=0)\n  box \"b\" (x=0)\n}";
        let span = span_of(src, "box \"b\" (x=0)");
        let out = set_attr(src, span, "x", "5");
        assert!(out.contains("box \"a\" (x=0)"), "first node untouched: {out}");
        assert!(out.contains("box \"b\" (x=5)"), "second node updated: {out}");
    }

    #[test]
    fn delete_node_removes_line_and_indent() {
        let src = "scene {\n  box \"a\" (x=0)\n  box \"b\" (x=1)\n}\n";
        let span = span_of(src, "box \"a\" (x=0)");
        let out = delete_node(src, span);
        assert_eq!(out, "scene {\n  box \"b\" (x=1)\n}\n");
    }

    #[test]
    fn delete_node_last_child_no_trailing_content() {
        let src = "scene {\n  box \"a\" (x=0)\n}\n";
        let span = span_of(src, "box \"a\" (x=0)");
        let out = delete_node(src, span);
        assert_eq!(out, "scene {\n}\n");
    }

    #[test]
    fn duplicate_node_inserts_copy_after() {
        let src = "scene {\n  box \"a\" (x=0)\n}\n";
        let span = span_of(src, "box \"a\" (x=0)");
        let out = duplicate_node(src, span);
        assert_eq!(
            out,
            "scene {\n  box \"a\" (x=0)\n  box \"a\" (x=0)\n}\n"
        );
    }

    #[test]
    fn duplicate_preserves_indentation_with_tabs() {
        let src = "scene {\n\tbox \"a\" (x=0)\n}\n";
        let span = span_of(src, "box \"a\" (x=0)");
        let out = duplicate_node(src, span);
        assert_eq!(out, "scene {\n\tbox \"a\" (x=0)\n\tbox \"a\" (x=0)\n}\n");
    }

    #[test]
    fn set_attr_handles_multiline_header() {
        let src = "wall \"b\" (\n  x=0,\n  y=1,\n  size=[1,1,1]\n)";
        let span = span_of(src, "wall \"b\" (\n  x=0,\n  y=1,\n  size=[1,1,1]\n)");
        let out = set_attr(src, span, "y", "9");
        assert!(out.contains("y=9"), "should replace y: {out}");
        assert!(out.contains("x=0"), "should preserve x: {out}");
        assert!(out.contains("size=[1,1,1]"), "size preserved: {out}");
    }

    #[test]
    fn set_attr_leaves_surrounding_whitespace_intact() {
        // The old value ends before the trailing space — whatever spacing the
        // author chose between value and comma is preserved on replacement so
        // diff churn stays minimal.
        let src = "box \"b\" (y=1.0 , x=0)";
        let span = span_of(src, "box \"b\" (y=1.0 , x=0)");
        let out = set_attr(src, span, "y", "2.0");
        assert_eq!(out, "box \"b\" (y=2.0 , x=0)");
    }

    #[test]
    fn delete_attr_removes_middle_attr_and_leading_comma() {
        let src = "box \"b\" (pos=[0,0,0], y=1.5, size=[1,1,1])";
        let span = span_of(src, "box \"b\" (pos=[0,0,0], y=1.5, size=[1,1,1])");
        let out = delete_attr(src, span, "y");
        assert_eq!(out, "box \"b\" (pos=[0,0,0], size=[1,1,1])");
    }

    #[test]
    fn delete_attr_removes_first_attr_and_trailing_comma() {
        let src = "box \"b\" (y=1.5, size=[1,1,1])";
        let span = span_of(src, "box \"b\" (y=1.5, size=[1,1,1])");
        let out = delete_attr(src, span, "y");
        assert_eq!(out, "box \"b\" (size=[1,1,1])");
    }

    #[test]
    fn delete_attr_absent_is_noop() {
        let src = "box \"b\" (size=[1,1,1])";
        let span = span_of(src, "box \"b\" (size=[1,1,1])");
        let out = delete_attr(src, span, "y");
        assert_eq!(out, src);
    }

    #[test]
    fn delete_attr_preserves_nested_brackets() {
        // holes=[[...], [...]] contains commas — the delete scan must treat
        // those as inside the bracket group, not as attr separators.
        let src = "wall \"w\" (x=0, holes=[[-1, -1, 1, 1], [0, 0, 2, 2]], z=0)";
        let span = span_of(src, "wall \"w\" (x=0, holes=[[-1, -1, 1, 1], [0, 0, 2, 2]], z=0)");
        let out = delete_attr(src, span, "x");
        assert!(out.contains("holes=[[-1, -1, 1, 1], [0, 0, 2, 2]]"));
        assert!(!out.contains("x=0"));
    }

    #[test]
    fn set_lod_scale_inserts_at_top_when_absent() {
        let src = "scene {\n  box \"b\" (size=1)\n}\n";
        let out = set_lod_scale(src, 0.5);
        assert!(out.starts_with("lod_scale (value=0.5)\n\nscene {"), "got: {out}");
    }

    #[test]
    fn set_lod_scale_keeps_seed_header_at_top() {
        let src = "// mogen-generate seed=42\nscene {\n  box \"b\" (size=1)\n}\n";
        let out = set_lod_scale(src, 1.5);
        assert!(out.starts_with("// mogen-generate seed=42\n"), "header demoted: {out}");
        assert!(out.contains("\nlod_scale (value=1.5)\n\nscene {"), "lod_scale missing: {out}");
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

    #[test]
    fn gizmo_canonical_commit_strips_shortcut_shadow() {
        // The regression that caused the snap-back: source uses the `ry`
        // shortcut, so a plain `rot=` writeback was silently defeated by
        // resolve_rot picking `ry` over `rot.y`. The canonical commit path
        // must delete `ry` (and friends) before setting `rot=`.
        let src = "scene {\n  box \"seat\" (pos=[0, 0.5, 0], ry=30)\n}\n";
        let node_src = "box \"seat\" (pos=[0, 0.5, 0], ry=30)";
        let span = span_of(src, node_src);
        let with_delete = delete_attr(src, span, "ry");
        assert!(!with_delete.contains("ry="), "ry should be stripped: {with_delete}");
        let out = set_attr(&with_delete, span, "rot", "[0, 45, 0]");
        assert!(out.contains("rot=[0, 45, 0]"), "rot written: {out}");
        // Recompile and confirm Y rotation actually landed (no shadow left).
        let recompiled = crate::pipeline::compile(&out, None);
        assert!(
            matches!(recompiled.stage, crate::pipeline::Stage::Ok),
            "scene compiles clean: stage={:?} diags={:?}",
            recompiled.stage,
            recompiled.diagnostics
        );
        let seat = recompiled
            .scene
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.name == "seat")
            .unwrap();
        let (_, ry, _) = seat.transform.rotation.to_euler(glam::EulerRot::XYZ);
        assert!(
            (ry.to_degrees() - 45.0).abs() < 1e-3,
            "expected 45° Y rotation, got {}°",
            ry.to_degrees()
        );
    }

    #[test]
    fn gizmo_rotation_commit_roundtrips_through_compile() {
        // Exercise the exact path the rotation gizmo takes: grab the
        // selected node's span out of a compiled scene, splice a new
        // `rot=[…]` onto it, recompile, and confirm the rotation landed
        // on the right node. A regression here is what "the object
        // snaps back on release" would look like — the scene still
        // compiles but the rotation doesn't stick.
        let src = "scene {\n  box \"seat\" (pos=[0, 0.5, 0], size=[1.0, 0.1, 1.0])\n}\n";
        let compiled = crate::pipeline::compile(src, None);
        assert!(
            matches!(compiled.stage, crate::pipeline::Stage::Ok),
            "baseline scene should compile cleanly: stage={:?} diags={:?}",
            compiled.stage,
            compiled.diagnostics
        );
        // Find the "seat" node in the compiled scene.
        let scene = compiled.scene.as_ref().expect("scene present");
        let (seat_idx, _) = scene
            .nodes
            .iter()
            .enumerate()
            .find(|(_, n)| n.name == "seat")
            .expect("seat node present");
        let span = compiled.node_spans[seat_idx].expect("seat has a source span");

        // Splice in a 45° Y rotation — the same vector the gizmo's
        // rotate commit would produce after a Y-axis drag.
        let out = set_attr(src, span, "rot", "[0, 45, 0]");
        assert_ne!(out, src, "set_attr must actually mutate the source");
        assert!(out.contains("rot=[0, 45, 0]"), "rot attr missing: {out}");

        // Recompile and confirm the rotation is present on the seat.
        let recompiled = crate::pipeline::compile(&out, None);
        assert!(
            matches!(recompiled.stage, crate::pipeline::Stage::Ok),
            "rotated scene must still compile clean: stage={:?} diags={:?}",
            recompiled.stage,
            recompiled.diagnostics
        );
        let scene2 = recompiled.scene.as_ref().unwrap();
        let seat2 = scene2.nodes.iter().find(|n| n.name == "seat").unwrap();
        let (_, ry, _) = seat2
            .transform
            .rotation
            .to_euler(glam::EulerRot::XYZ);
        assert!(
            (ry.to_degrees() - 45.0).abs() < 1e-3,
            "expected 45° Y rotation, got {}°",
            ry.to_degrees()
        );
    }

    #[test]
    fn insert_imports_after_existing_imports_block() {
        let src = "import \"a.mog\"\nimport \"b.mog\"\n\nscene {}\n";
        let out = insert_imports(src, &["c.mog"]);
        assert_eq!(
            out,
            "import \"a.mog\"\nimport \"b.mog\"\nimport \"c.mog\"\n\nscene {}\n"
        );
    }

    #[test]
    fn insert_imports_skips_duplicates() {
        let src = "import \"a.mog\"\n\nscene {}\n";
        let out = insert_imports(src, &["a.mog", "b.mog"]);
        // a.mog is already there; only b.mog is added.
        assert!(out.contains("import \"b.mog\""));
        assert_eq!(out.matches("import \"a.mog\"").count(), 1);
    }

    #[test]
    fn insert_imports_into_file_with_no_existing_imports() {
        let src = "// header comment\n\nscene { box \"b\" (size=1) }\n";
        let out = insert_imports(src, &["chair.mog"]);
        // Lands after the header, before the scene, with one blank line of
        // separation.
        assert!(out.starts_with("// header comment\n\nimport \"chair.mog\"\n\n"));
        assert!(out.contains("scene { box"));
    }

    #[test]
    fn insert_imports_no_paths_is_noop() {
        let src = "scene {}\n";
        let out = insert_imports(src, &[]);
        assert_eq!(out, src);
    }
}
