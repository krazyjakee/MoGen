//! `set_attr` / `delete_attr` and their header-scanning helpers. These are
//! the hot-path mutations called by every gizmo drag and inspector field; a
//! bug here corrupts the .mog buffer silently.

use mogen_core::Span;

use super::internals::{clamp_span, is_ident_byte, match_closing_paren, splice};

/// Read the value substring of `name=…` in the header of the node covered by
/// `span`, returning `None` when the node has no header or the attr is absent.
/// The returned slice is trimmed: `pos=[1,2,3]` → `[1,2,3]`, `noise=0.3 ` →
/// `0.3`. Inspector panels use this to show the user's current authored value
/// without re-parsing the AST.
pub fn get_attr<'a>(src: &'a str, span: Span, name: &str) -> Option<&'a str> {
    let (start, end) = clamp_span(src, span);
    let (hdr_open, hdr_close) = find_header_parens(src, start, end)?;
    let (kstart, vend) = find_attr_in_header(src, hdr_open + 1, hdr_close, name)?;
    let slice = &src[kstart..vend];
    let eq = slice.find('=')?;
    Some(slice[eq + 1..].trim())
}

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

    // Append before closing `)`. Insert flush with the last *real* attribute,
    // not after a trailing `//` comment — `mushrooms=24, // note` must become
    // `mushrooms=24, newattr=val // note`, never `… // note newattr=val`
    // (which buries the attr inside the comment and silently drops it).
    let body_start = hdr_open + 1;
    let insert_at = last_code_byte_end(src, body_start, hdr_close);
    // Preserve leading spacing: if the last real content is itself a `,` (the
    // multi-line idiom `last_attr,\n)`), prepend just a space so we don't
    // produce `,,`; otherwise prepend ", ".
    let prefix = if insert_at == body_start {
        "" // empty (or comment-only) header body
    } else if src.as_bytes()[insert_at - 1] == b',' {
        " "
    } else {
        ", "
    };
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
                // Scan to `src.len()` rather than `end` so the matcher tracks
                // the real closing `)` even when an earlier edit (or batch
                // member) has expanded the header past the captured span end.
                // Without this, a follow-up `set_attr` on the same span would
                // miss the now-shifted `)` and synthesise a second `(...)`
                // header. `match_closing_paren` is paren-depth-tracking, so
                // widening the limit doesn't tempt it past the true close.
                let close = match_closing_paren(src, i, src.len())?;
                return Some((i, close));
            }
            _ => i += 1,
        }
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
            // A `//` comment ends the value (its text — including any comma —
            // is not part of the attribute). Leave `i` on the comment so the
            // outer scan skips it before looking for the next key.
            if c == b'/' && i + 1 < body_end && bytes[i + 1] == b'/' {
                break;
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

/// Index one past the last byte in `[start..end)` that is real attribute
/// content — i.e. not whitespace and not inside a `//` line comment. String
/// contents (which may contain `//`) count as real. Returns `start` when the
/// range holds nothing but whitespace and/or comments. Used by `set_attr` to
/// append a new attribute flush with the last existing one instead of after a
/// trailing comment.
fn last_code_byte_end(src: &str, start: usize, end: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = start;
    let mut last = start;
    let mut in_string = false;
    while i < end {
        let c = bytes[i];
        if in_string {
            if c == b'\\' && i + 1 < end {
                i += 2;
                last = i;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            last = i;
            continue;
        }
        if c == b'/' && i + 1 < end && bytes[i + 1] == b'/' {
            while i < end && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'"' {
            in_string = true;
        }
        if !c.is_ascii_whitespace() {
            last = i + 1;
        }
        i += 1;
    }
    last
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
    fn set_attr_appends_before_trailing_line_comment() {
        // Regression: the last attr line ends in a `//` comment. The new attr
        // must land flush with the real last attr, NOT inside the comment
        // (which would silently disable it — the "Show POI markers" freeze).
        let src = "cave \"c\" (\n  seed=1,\n  mushrooms=24,  // markers on chamber floors\n)";
        let span = span_of(src, "cave \"c\" (");
        let out = set_attr(src, span, "debug_show_poi", "1");
        assert_eq!(
            out,
            "cave \"c\" (\n  seed=1,\n  mushrooms=24, debug_show_poi=1  // markers on chamber floors\n)"
        );
        // And it round-trips: get_attr/set_attr see the real attr, not the
        // comment text, so toggling back off rewrites the value in place.
        let span2 = span_of(&out, "cave \"c\" (");
        assert_eq!(get_attr(&out, span2, "debug_show_poi"), Some("1"));
        let off = set_attr(&out, span2, "debug_show_poi", "0");
        assert!(
            off.contains("debug_show_poi=0  // markers on chamber floors"),
            "toggle-off must rewrite value and keep the comment intact: {off}"
        );
    }

    #[test]
    fn set_attr_ignores_attr_name_inside_comment() {
        // A key that only appears in a comment must not be matched/replaced;
        // set_attr should append a fresh attribute instead.
        let src = "box \"b\" (x=1 // size=99 here\n)";
        let span = span_of(src, "box \"b\" (");
        let out = set_attr(src, span, "size", "[2,2,2]");
        assert!(
            out.contains("x=1, size=[2,2,2] // size=99 here"),
            "size must be appended, not written into the comment: {out}"
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
    fn set_attr_multi_insert_same_span_grows_correctly() {
        // The viewport gizmo's relative-placed Translate writeback emits
        // three `x=`/`y=`/`z=` set ops with one captured span. Each insert
        // grows the source past the original span end; the closing `)`
        // scan must follow it instead of bailing and synthesising a second
        // `(...)` header. Pin the round-trip so the helper stays usable for
        // multi-edit batches.
        let src = "scene {\n  box \"seat\" ()\n}\n";
        let span = span_of(src, "box \"seat\" ()");
        let s1 = set_attr(src, span, "x", "1");
        let s2 = set_attr(&s1, span, "y", "2");
        let s3 = set_attr(&s2, span, "z", "3");
        assert!(
            s3.contains("box \"seat\" (x=1, y=2, z=3)"),
            "expected fused header, got {s3}"
        );
        assert_eq!(s3.matches('(').count(), 1, "no synthesised second header: {s3}");
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
    fn set_attr_appends_after_multiline_trailing_comma() {
        // Regression: the multi-line idiom `last_attr,\n)` ends the body
        // with a trailing comma after `trim_end()`. The append path used
        // to prepend `, ` unconditionally, producing `last_attr,, new=…`
        // — invalid DSL. The fix is to detect the trailing `,` and prepend
        // just a space so the comma already there serves as the separator.
        let src = "building \"h\" (\n  rooms=14,\n  mat=\"plaster\",\n)";
        let span = span_of(src, "building \"h\" (\n  rooms=14,\n  mat=\"plaster\",\n)");
        let out = set_attr(src, span, "door_w", "1");
        assert!(
            !out.contains(",,"),
            "must not produce a double comma: {out}"
        );
        assert!(out.contains("door_w=1"), "door_w appended: {out}");
        assert!(out.contains("mat=\"plaster\""), "mat preserved: {out}");
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
    fn get_attr_reads_scalar_and_vec_values() {
        let src = "box \"b\" (pos=[0, 1, 2], noise=0.3, faceted=1)";
        let span = span_of(src, "box \"b\" (pos=[0, 1, 2], noise=0.3, faceted=1)");
        assert_eq!(get_attr(src, span, "noise"), Some("0.3"));
        assert_eq!(get_attr(src, span, "faceted"), Some("1"));
        assert_eq!(get_attr(src, span, "pos"), Some("[0, 1, 2]"));
        assert_eq!(get_attr(src, span, "twist_y"), None);
    }

    #[test]
    fn get_attr_returns_none_when_no_header() {
        let src = "box \"b\" { }";
        let span = span_of(src, "box \"b\" { }");
        assert_eq!(get_attr(src, span, "noise"), None);
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
    fn cave_debug_show_poi_checkbox_roundtrips_through_compile() {
        // Replicate the exact path the "Show POI markers" checkbox takes:
        // grab the cave wrapper's span out of a compiled scene, splice
        // `debug_show_poi=1` onto it, recompile, and confirm a POI marker
        // gained a mesh. The cave node carries a `{ feature … }` body block,
        // so this also guards `find_header_parens` against tripping on the
        // body brace before the header parens.
        let src = "cave \"den\" (\n  seed=3,\n  size=[20, 10, 20],\n  chambers=5,\n  resolution=40,\n  mushrooms=4,\n) {\n  feature \"spikes\" (kind=stalagmite, count=2)\n}\n";
        let compiled = crate::pipeline::compile(src, None);
        assert!(
            matches!(compiled.stage, crate::pipeline::Stage::Ok),
            "baseline cave should compile cleanly: stage={:?} diags={:?}",
            compiled.stage,
            compiled.diagnostics
        );
        let scene = compiled.scene.as_ref().expect("scene present");
        let (wrapper_idx, _) = scene
            .nodes
            .iter()
            .enumerate()
            .find(|(_, n)| n.kind == "cave")
            .expect("cave wrapper node present");
        let span = compiled.node_spans[wrapper_idx]
            .expect("cave wrapper must carry a source span for the inspector");

        let out = set_attr(src, span, "debug_show_poi", "1");
        assert_ne!(out, src, "set_attr must actually mutate the source");
        assert!(
            out.contains("debug_show_poi=1"),
            "debug_show_poi attr missing after set_attr: {out}"
        );

        let recompiled = crate::pipeline::compile(&out, None);
        assert!(
            matches!(recompiled.stage, crate::pipeline::Stage::Ok),
            "cave with debug_show_poi must still compile clean: stage={:?} diags={:?}",
            recompiled.stage,
            recompiled.diagnostics
        );
        let scene2 = recompiled.scene.as_ref().unwrap();
        let any_marker_mesh = scene2
            .nodes
            .iter()
            .any(|n| n.tags.iter().any(|t| t == "poi") && n.mesh.is_some());
        assert!(
            any_marker_mesh,
            "debug_show_poi=1 should give at least one POI marker a debug mesh"
        );

        // The next inspector frame re-reads the checkbox state via `get_attr`
        // against the RECOMPILED wrapper span (which shifted when the attr was
        // inserted). If that readback can't see `debug_show_poi`, the checkbox
        // reverts to unchecked every frame and looks inert — exactly the
        // "does not check the box" symptom. Guard it.
        let (wrapper2_idx, _) = scene2
            .nodes
            .iter()
            .enumerate()
            .find(|(_, n)| n.kind == "cave")
            .expect("cave wrapper still present after recompile");
        let span2 = recompiled.node_spans[wrapper2_idx]
            .expect("recompiled cave wrapper must keep a source span");
        let readback = get_attr(&out, span2, "debug_show_poi");
        assert_eq!(
            readback.map(str::trim),
            Some("1"),
            "inspector readback must see debug_show_poi=1 on the recompiled span"
        );
    }
}
