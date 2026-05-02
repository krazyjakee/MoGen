//! Whole-node mutations: line-aware delete and inline duplicate. Both
//! operate on the byte span the node occupies plus the indentation on its
//! line so the surrounding source stays formatted.

use mogen_core::Span;

use super::internals::{clamp_span, splice};

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

/// Drop spans whose interval is fully contained in another span (keeping
/// the outer). Used by multi-delete: when the user selects a parent and one
/// of its descendants, the parent's deletion already removes the descendant,
/// and a follow-up delete on a stale child span would corrupt the source.
/// Exact duplicates are also collapsed. Returned order is unspecified;
/// callers typically sort by `start` afterwards.
pub fn dedup_contained_spans(spans: &[Span]) -> Vec<Span> {
    // Sort by (start asc, end desc) so any container of `s` precedes `s`.
    let mut sorted: Vec<Span> = spans.to_vec();
    sorted.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out: Vec<Span> = Vec::with_capacity(sorted.len());
    for s in sorted {
        if out
            .iter()
            .any(|o| o.start <= s.start && o.end >= s.end)
        {
            continue;
        }
        out.push(s);
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn span_of(src: &str, needle: &str) -> Span {
        let start = src.find(needle).expect("needle not found");
        Span::new(start, start + needle.len())
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
    fn delete_two_siblings_right_to_left_keeps_source_valid() {
        // Multi-select delete: callers must sort spans by `start` DESCENDING
        // before applying. Going right-to-left means each delete only shrinks
        // bytes after the next-to-be-deleted span's start, so the next span
        // remains addressable. Reversing the order would silently invalidate
        // the second span and corrupt the source.
        let src = "scene {\n  box \"a\" (x=0)\n  box \"b\" (x=1)\n  box \"c\" (x=2)\n}\n";
        let span_a = span_of(src, "box \"a\" (x=0)");
        let span_c = span_of(src, "box \"c\" (x=2)");
        // Delete c (later) first, then a (earlier) — the right-to-left order.
        let mut out = delete_node(src, span_c);
        out = delete_node(&out, span_a);
        assert_eq!(out, "scene {\n  box \"b\" (x=1)\n}\n");
    }

    #[test]
    fn dedup_contained_spans_drops_nested_child() {
        // Multi-select that includes a parent + one of its children: the
        // child must be dropped before applying deletes, otherwise the
        // parent's deletion would invalidate the child's span and the
        // follow-up delete would corrupt the source.
        let parent = Span::new(10, 50);
        let child = Span::new(20, 30);
        let kept = dedup_contained_spans(&[parent, child]);
        assert_eq!(kept, vec![parent]);
        // Order-independent: same result if child appears first.
        let kept = dedup_contained_spans(&[child, parent]);
        assert_eq!(kept, vec![parent]);
    }

    #[test]
    fn dedup_contained_spans_keeps_disjoint_siblings() {
        let a = Span::new(5, 15);
        let b = Span::new(20, 30);
        let kept = dedup_contained_spans(&[a, b]);
        assert_eq!(kept, vec![a, b]);
    }

    #[test]
    fn dedup_contained_spans_collapses_exact_duplicates() {
        let s = Span::new(10, 20);
        assert_eq!(dedup_contained_spans(&[s, s, s]), vec![s]);
    }

    #[test]
    fn multi_delete_parent_with_dedup_removes_subtree_cleanly() {
        // End-to-end shape of multi-delete on a parent + child selection:
        // dedup drops the child, leaving just the parent, which deletes
        // both in one go. This is the contract the inspector + viewport
        // hotkey both rely on.
        let src = "scene {\n  group \"g\" {\n    box \"x\" (x=0)\n  }\n}\n";
        let span_parent = span_of(src, "group \"g\" {\n    box \"x\" (x=0)\n  }");
        let span_child = span_of(src, "box \"x\" (x=0)");
        let mut spans = dedup_contained_spans(&[span_parent, span_child]);
        spans.sort_by(|a, b| b.start.cmp(&a.start));
        let mut out = src.to_string();
        for s in spans {
            out = delete_node(&out, s);
        }
        assert_eq!(out, "scene {\n}\n");
    }
}
