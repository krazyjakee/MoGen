//! `import "<path>"` line insertion for the import dialog. Idempotent: a
//! re-run with the same selection is a no-op so the dialog can be hammered
//! without piling up duplicate lines.

use super::internals::{
    is_ident_byte, match_closing_paren, skip_leading_comments_and_blanks, splice,
};

/// Insert `import "<path>"` lines for each entry in `paths` near the top of
/// `src`. Paths already present (matched as quoted-string literals) are
/// skipped so re-running the import dialog with the same selection is a
/// no-op. New imports land next to any contiguous run of existing `import`
/// lines; if there are none but a leading `meta(...)` block exists, they
/// land beneath it; otherwise they go after the leading comment/blank
/// header. Each inserted line ends with `\n`; if the surrounding source
/// needs a blank line of separation, this fn handles it.
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
    // If a `meta(...)` block sits at the top, jump past it so imports land
    // beneath the meta block rather than ahead of it.
    if let Some(after_meta) = skip_meta_block(src, cursor) {
        cursor = after_meta;
    }
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

/// If a `meta(...)` block is the first declaration at or after `start`,
/// return the position just past it (skipping any trailing blank lines).
/// Returns `None` if there is no leading meta block.
fn skip_meta_block(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let i = skip_blank_and_comment_lines(src, start);
    if !src[i..].starts_with("meta") {
        return None;
    }
    let after_kw = i + 4;
    if after_kw < bytes.len() && is_ident_byte(bytes[after_kw]) {
        return None;
    }
    let mut j = after_kw;
    while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n') {
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b'(' {
        return None;
    }
    let close = match_closing_paren(src, j, bytes.len())?;
    let after_close = close + 1;
    let line_end = next_line_end(bytes, after_close);
    let cursor = if line_end < bytes.len() { line_end + 1 } else { line_end };
    Some(skip_blank_lines(src, cursor))
}

fn skip_blank_lines(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    while i < bytes.len() {
        let line_end = next_line_end(bytes, i);
        if src[i..line_end].trim().is_empty() {
            i = if line_end < bytes.len() { line_end + 1 } else { line_end };
        } else {
            break;
        }
    }
    i
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn insert_imports_lands_under_meta_block() {
        let src = "meta (\n  name = \"chair\",\n  seed = \"1\"\n)\n\nscene { box \"b\" (size=1) }\n";
        let out = insert_imports(src, &["chair.mog"]);
        assert!(out.starts_with(
            "meta (\n  name = \"chair\",\n  seed = \"1\"\n)\n\nimport \"chair.mog\"\n\n"
        ));
        assert!(out.contains("scene { box"));
    }

    #[test]
    fn insert_imports_after_existing_imports_below_meta() {
        let src = "meta (\n  name = \"x\"\n)\n\nimport \"a.mog\"\n\nscene {}\n";
        let out = insert_imports(src, &["b.mog"]);
        assert_eq!(
            out,
            "meta (\n  name = \"x\"\n)\n\nimport \"a.mog\"\nimport \"b.mog\"\n\nscene {}\n"
        );
    }

    #[test]
    fn insert_imports_no_paths_is_noop() {
        let src = "scene {}\n";
        let out = insert_imports(src, &[]);
        assert_eq!(out, src);
    }
}
