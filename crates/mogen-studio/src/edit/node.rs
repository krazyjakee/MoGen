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
}
