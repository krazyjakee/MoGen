//! Top-level node insertion and `import` enumeration for the viewport
//! context menu's Add → Primitive / Add → Imports submenus.
//!
//! Both ops are span-aware text mutations — no AST round-trip — so the
//! user's formatting and any in-flight diagnostics survive.

use super::internals::splice;

/// One `import "<path>" (as=<alias>?)` line discovered in the source. The
/// `module_name` is the effective name the file contributes: the explicit
/// alias when one is supplied, otherwise the file stem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportEntry {
    pub module_name: String,
    pub path: String,
}

/// Walk the source line-by-line and collect every top-level `import` line.
/// Tolerates mid-edit source that the parser would reject — useful because
/// the context menu shows imports while the user is still typing.
pub fn list_imports(src: &str) -> Vec<ImportEntry> {
    let mut out = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("import") else {
            continue;
        };
        // The keyword must be followed by whitespace before the quoted path.
        if !rest.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let rest = rest.trim_start();
        let Some(after_quote) = rest.strip_prefix('"') else {
            continue;
        };
        let Some(end) = after_quote.find('"') else {
            continue;
        };
        let path = after_quote[..end].to_string();
        let trail = &after_quote[end + 1..];
        let module_name = parse_as_alias(trail).unwrap_or_else(|| stem_of(&path));
        out.push(ImportEntry { module_name, path });
    }
    out
}

/// Append `body` as the last child of the first top-level `scene { … }`
/// block in `src`. If no scene block exists, append `scene { body }`.
/// Indentation is two spaces — the project convention; files using a
/// different indent stay valid but won't match the surrounding block.
///
/// Multi-line bodies (e.g. a CSG op with two operands on separate lines)
/// are re-indented so every line lands at the scene's inner indent. The
/// caller is expected to already indent the body's interior relative to
/// its first line — the merge here just shifts the whole block right.
pub fn append_to_scene(src: &str, body: &str) -> String {
    if let Some((open, close)) = find_top_level_scene_block(src) {
        let block_indent = block_indent_for_close(src, close);
        let inner_indent = format!("{block_indent}  ");
        let bytes = src.as_bytes();
        let mut splice_at = close;
        while splice_at > open {
            let b = bytes[splice_at - 1];
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                splice_at -= 1;
            } else {
                break;
            }
        }
        let indented = body.replace('\n', &format!("\n{inner_indent}"));
        let payload = format!("\n{inner_indent}{indented}\n{block_indent}");
        return splice(src, splice_at..close, &payload);
    }
    let mut out = src.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    let indented = body.replace('\n', "\n  ");
    out.push_str("scene {\n  ");
    out.push_str(&indented);
    out.push_str("\n}\n");
    out
}

/// Suggest a unique `<kind>_<n>` name for a freshly-inserted primitive of
/// `kind`, by scanning the source for any existing `"<kind>_N"` literal and
/// returning `<kind>_<max+1>`. Returns `<kind>_1` when no match is found.
pub fn suggest_primitive_name(src: &str, kind: &str) -> String {
    let prefix = format!("\"{kind}_");
    let mut max_n: u32 = 0;
    let bytes = src.as_bytes();
    let mut i = 0;
    while i + prefix.len() < bytes.len() {
        if &bytes[i..i + prefix.len()] == prefix.as_bytes() {
            let mut j = i + prefix.len();
            let num_start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > num_start && j < bytes.len() && bytes[j] == b'"' {
                if let Ok(n) = std::str::from_utf8(&bytes[num_start..j])
                    .unwrap_or("")
                    .parse::<u32>()
                {
                    if n > max_n {
                        max_n = n;
                    }
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    format!("{kind}_{}", max_n + 1)
}

/// Read the optional `(as=<ident>)` attribute trailing the import's quoted
/// path. Returns `None` when the attribute is missing or malformed — the
/// caller falls back to the file stem.
fn parse_as_alias(trail: &str) -> Option<String> {
    let trail = trail.trim_start();
    let inside = trail.strip_prefix('(')?;
    let close = inside.find(')')?;
    let attrs = &inside[..close];
    for piece in attrs.split(',') {
        let piece = piece.trim();
        let Some(rest) = piece.strip_prefix("as") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(after_eq) = rest.strip_prefix('=') else {
            continue;
        };
        let after_eq = after_eq.trim_start();
        if let Some(quoted) = after_eq.strip_prefix('"') {
            if let Some(end) = quoted.find('"') {
                return Some(quoted[..end].to_string());
            }
            return None;
        }
        let ident: String = after_eq
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !ident.is_empty() {
            return Some(ident);
        }
    }
    None
}

fn stem_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Locate the first top-level `scene { … }` block. Returns the byte offsets
/// of the opening and closing braces. Skips strings and `//` comments.
fn find_top_level_scene_block(src: &str) -> Option<(usize, usize)> {
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' {
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
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let prev_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
        if prev_ok
            && i + 5 <= bytes.len()
            && &bytes[i..i + 5] == b"scene"
            && (i + 5 == bytes.len() || !is_ident_byte(bytes[i + 5]))
        {
            let mut j = i + 5;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                j += 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        j += 2;
                        continue;
                    }
                    j += 1;
                }
                if j < bytes.len() {
                    j += 1;
                }
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
            }
            if j < bytes.len() && bytes[j] == b'(' {
                if let Some(end) = match_close(bytes, j, b'(', b')') {
                    j = end + 1;
                }
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
            }
            if j < bytes.len() && bytes[j] == b'{' {
                if let Some(close) = match_close(bytes, j, b'{', b'}') {
                    return Some((j, close));
                }
            }
        }
        i += 1;
    }
    None
}

fn match_close(bytes: &[u8], open: usize, open_ch: u8, close_ch: u8) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut i = open;
    let mut in_str = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if c == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == open_ch {
            depth += 1;
        } else if c == close_ch {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn block_indent_for_close(src: &str, pos: usize) -> String {
    let bytes = src.as_bytes();
    let mut line_start = pos;
    while line_start > 0 && bytes[line_start - 1] != b'\n' {
        line_start -= 1;
    }
    let mut indent_end = line_start;
    while indent_end < pos && (bytes[indent_end] == b' ' || bytes[indent_end] == b'\t') {
        indent_end += 1;
    }
    String::from_utf8_lossy(&bytes[line_start..indent_end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_imports_basic() {
        let src = "import \"a.mog\"\nimport \"sub/b.mog\" (as=foo)\n\nscene {}\n";
        let entries = list_imports(src);
        assert_eq!(
            entries,
            vec![
                ImportEntry {
                    module_name: "a".into(),
                    path: "a.mog".into()
                },
                ImportEntry {
                    module_name: "foo".into(),
                    path: "sub/b.mog".into()
                },
            ]
        );
    }

    #[test]
    fn list_imports_skips_inline_comments_and_invalid_lines() {
        let src = "// import \"x.mog\"\nimport \"ok.mog\"\nimport bare\n";
        let entries = list_imports(src);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "ok.mog");
    }

    #[test]
    fn append_to_scene_inserts_before_close() {
        let src = "scene {\n  box \"a\" (size=1)\n}\n";
        let out = append_to_scene(src, "sphere \"b\" (radius=0.5)");
        assert_eq!(
            out,
            "scene {\n  box \"a\" (size=1)\n  sphere \"b\" (radius=0.5)\n}\n"
        );
    }

    #[test]
    fn append_to_scene_creates_block_when_missing() {
        let src = "module \"m\" () {}\n";
        let out = append_to_scene(src, "box \"b\" (size=1)");
        assert!(out.ends_with("scene {\n  box \"b\" (size=1)\n}\n"));
        assert!(out.contains("module \"m\""));
    }

    #[test]
    fn append_to_scene_handles_single_line_block() {
        let src = "scene { box \"a\" }\n";
        let out = append_to_scene(src, "sphere \"b\" (radius=0.5)");
        // Tolerates the existing single-line layout — the appended item lands
        // on its own line, the closing brace shifts down.
        assert!(out.contains("box \"a\""));
        assert!(out.contains("sphere \"b\" (radius=0.5)"));
        assert!(out.ends_with("}\n"));
    }

    #[test]
    fn append_to_scene_indents_multiline_body() {
        let src = "scene {\n  box \"a\" (size=1)\n}\n";
        let body = "difference \"d\" () {\n  box \"a\" (size=[1, 1, 1])\n  box \"b\" (size=[0.7, 1.2, 0.7])\n}";
        let out = append_to_scene(src, body);
        assert_eq!(
            out,
            "scene {\n  box \"a\" (size=1)\n  difference \"d\" () {\n    box \"a\" (size=[1, 1, 1])\n    box \"b\" (size=[0.7, 1.2, 0.7])\n  }\n}\n"
        );
    }

    #[test]
    fn append_to_scene_skips_comment_with_scene_word() {
        let src = "// scene { not really }\nmodule \"m\" () {}\n";
        let out = append_to_scene(src, "box \"b\" (size=1)");
        // No real scene block existed — a fresh one was synthesised.
        assert!(out.contains("scene {\n  box \"b\""));
    }

    #[test]
    fn suggest_primitive_name_increments() {
        let src = "scene {\n  box \"box_1\" (size=1)\n  box \"box_2\" (size=1)\n}\n";
        assert_eq!(suggest_primitive_name(src, "box"), "box_3");
    }

    #[test]
    fn suggest_primitive_name_starts_at_one_when_absent() {
        let src = "scene {}\n";
        assert_eq!(suggest_primitive_name(src, "sphere"), "sphere_1");
    }
}
