//! Extraction and stamping of the optional top-level `meta(...)` block.
//!
//! Shape (all fields optional, block itself optional):
//!
//! ```text
//! meta (
//!   name = "wooden_chair",
//!   version = "1.2.0",
//!   mogen_version = "0.1.1",
//!   description = "A simple four-legged dining chair.",
//!   tags = ["furniture", "chair", "wood"],
//!   seed = "1777726918483806000",
//!   thinking = "medium",
//!   prompt = "a simple four-legged dining chair",
//! )
//! ```
//!
//! `mogen_version` is auto-stamped from `CARGO_PKG_VERSION` whenever the CLI
//! or Studio writes a `.mog` file (see [`stamp_mogen_version`]). The
//! `seed` / `thinking` / `prompt` fields are written by the LLM commands so
//! future calls can reproduce the same generation without re-supplying flags.
//! The other fields are preserved verbatim across rewrites.
//!
//! Extraction is lenient — unknown attrs and malformed values surface as
//! validator diagnostics, not parse errors, so a half-edited file still
//! lowers far enough for Studio's live preview.

use mogen_core::Meta;

use crate::ast::{Node, Value};

/// Walk the top-level AST nodes and lift the (optional) `meta` block into a
/// typed [`Meta`]. Returns `None` when no `meta` node is present.
///
/// Only the FIRST `meta` block is honoured; duplicates are caught by the
/// validator and ignored here.
pub fn extract_meta(ast: &[Node]) -> Option<Meta> {
    for n in ast {
        if n.kind == "meta" {
            return Some(meta_from_node(n));
        }
    }
    None
}

fn meta_from_node(n: &Node) -> Meta {
    let mut m = Meta::default();
    for (k, v) in &n.attrs {
        match (k.as_str(), v) {
            ("name", Value::String(s)) | ("name", Value::Ident(s)) => m.name = Some(s.clone()),
            ("version", Value::String(s)) | ("version", Value::Ident(s)) => {
                m.version = Some(s.clone())
            }
            ("mogen_version", Value::String(s)) | ("mogen_version", Value::Ident(s)) => {
                m.mogen_version = Some(s.clone())
            }
            ("description", Value::String(s)) | ("description", Value::Ident(s)) => {
                m.description = Some(s.clone())
            }
            ("tags", Value::ListString(items)) => m.tags = items.clone(),
            ("tags", Value::String(s)) | ("tags", Value::Ident(s)) => m.tags = vec![s.clone()],
            ("seed", Value::String(s)) | ("seed", Value::Ident(s)) => m.seed = s.parse().ok(),
            ("thinking", Value::String(s)) | ("thinking", Value::Ident(s)) => {
                m.thinking = Some(s.clone())
            }
            ("prompt", Value::String(s)) | ("prompt", Value::Ident(s)) => {
                m.prompt = Some(s.clone())
            }
            _ => {} // type mismatches surfaced by the validator
        }
    }
    m
}

/// Insert or update the top-level `meta(...)` block so its `mogen_version`
/// equals `current_version`. Other fields are preserved verbatim.
pub fn stamp_mogen_version(dsl: &str, current_version: &str) -> String {
    upsert_meta_attr(dsl, "mogen_version", current_version)
}

/// Insert or update a single `key = "value"` attribute inside the top-level
/// `meta(...)` block. Creates the meta block when none exists, placing it
/// before the first non-comment, non-blank declaration.
///
/// The function is text-level on purpose — it must work on half-edited drafts
/// from Studio that do not yet parse cleanly. It only touches what it needs to.
pub fn upsert_meta_attr(dsl: &str, key: &str, value: &str) -> String {
    if let Some(updated) = update_existing_meta(dsl, key, value) {
        return updated;
    }
    insert_fresh_meta(dsl, key, value)
}

/// Find an existing top-level `meta(...)` and rewrite (or append) the given
/// attribute; returns `None` if no such block exists.
fn update_existing_meta(dsl: &str, key: &str, value: &str) -> Option<String> {
    let start = find_meta_open(dsl)?;
    let attrs_open = dsl[start..].find('(').map(|o| start + o)?;
    let attrs_close = matching_close(dsl, attrs_open)?;
    let inner = &dsl[attrs_open + 1..attrs_close];
    let new_inner = if has_attr_key(inner, key) {
        replace_attr_value(inner, key, value)
    } else {
        append_attr(inner, key, value)
    };
    let mut out = String::with_capacity(dsl.len() + 32);
    out.push_str(&dsl[..attrs_open + 1]);
    out.push_str(&new_inner);
    out.push_str(&dsl[attrs_close..]);
    Some(out)
}

/// Read the value of a single attribute in the top-level `meta(...)` block.
/// Returns the unquoted string content for `key = "..."` and the raw token
/// for `key = ident`. `None` when no meta block is present or the key is
/// absent.
///
/// Text-level so it works on half-edited drafts and avoids forcing a full
/// parse for cheap header reads.
pub fn read_meta_attr(dsl: &str, key: &str) -> Option<String> {
    let start = find_meta_open(dsl)?;
    let attrs_open = dsl[start..].find('(').map(|o| start + o)?;
    let attrs_close = matching_close(dsl, attrs_open)?;
    let inner = &dsl[attrs_open + 1..attrs_close];
    get_attr_value(inner, key)
}

/// Insert a fresh `meta(<key> = "<value>")` line. Goes after the leading
/// `//`-comment block (legacy seed / prompt / thinking headers) and before
/// the first non-comment, non-blank line.
fn insert_fresh_meta(dsl: &str, key: &str, value: &str) -> String {
    let insertion = first_non_header_offset(dsl);
    let escaped = escape_dsl_string(value);
    let line = format!("meta ({key} = \"{escaped}\")\n\n");
    let mut out = String::with_capacity(dsl.len() + line.len());
    out.push_str(&dsl[..insertion]);
    if insertion > 0 && !dsl[..insertion].ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&line);
    out.push_str(&dsl[insertion..]);
    out
}

/// Strip leading `// mogen-generate seed=…`, `// mogen-generate thinking=…`,
/// and `// prompt: …` comment lines that older versions of MoGen wrote at the
/// top of every generated `.mog`. Idempotent — files without those comments
/// are returned unchanged.
///
/// Used by `embed_seed_header` to migrate legacy files to the new
/// `meta(seed=…, thinking=…, prompt=…)` representation on the next save.
pub fn strip_legacy_seed_comments(dsl: &str) -> String {
    let mut out = String::with_capacity(dsl.len());
    let mut header_active = true;
    for line in dsl.split_inclusive('\n') {
        if header_active {
            let trimmed = line.trim_start();
            if trimmed.starts_with("// mogen-generate ") || trimmed.starts_with("// prompt:") {
                continue;
            }
            // Allow blank lines interleaved with the legacy header. Keep
            // walking comments-only territory; bail the first time we hit
            // a non-comment, non-blank line.
            let body = trimmed.trim_end_matches(['\r', '\n']);
            if !body.is_empty() && !body.starts_with("//") {
                header_active = false;
            }
        }
        out.push_str(line);
    }
    out
}

/// Byte offset of the first character after the leading run of `//`-comments
/// and blank lines, i.e. where a fresh top-of-file declaration should go.
fn first_non_header_offset(dsl: &str) -> usize {
    let mut offset = 0;
    for line in dsl.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            offset += line.len();
            continue;
        }
        break;
    }
    offset
}

/// Locate the `meta` keyword at the start of a top-level declaration. Returns
/// the byte offset of the `m` in `meta`, or `None` if no top-level `meta`
/// node exists.
fn find_meta_open(dsl: &str) -> Option<usize> {
    let bytes = dsl.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip leading whitespace + line comments at the start of a line.
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\n' || bytes[i] == b'\r') {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if dsl[i..].starts_with("meta")
            && dsl[i..]
                .as_bytes()
                .get(4)
                .map(|c| !is_ident_char(*c))
                .unwrap_or(true)
        {
            return Some(i);
        }
        // Not meta — only the first non-comment top-level declaration matters.
        return None;
    }
    None
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Find the byte offset of the `)` matching the `(` at `open`.
fn matching_close(dsl: &str, open: usize) -> Option<usize> {
    let bytes = dsl.as_bytes();
    let mut depth: i32 = 0;
    let mut i = open;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == b'"' && bytes.get(i.wrapping_sub(1)).copied() != Some(b'\\') {
                in_string = false;
            }
        } else {
            match c {
                b'"' => in_string = true,
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Whether an attr-list inner already has a `key =` entry.
fn has_attr_key(inner: &str, key: &str) -> bool {
    let mut i = 0;
    let bytes = inner.as_bytes();
    while i < bytes.len() {
        // Skip whitespace and line comments.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Read an ident.
        let start = i;
        while i < bytes.len() && is_ident_char(bytes[i]) {
            i += 1;
        }
        let ident = &inner[start..i];
        if ident == key {
            return true;
        }
        // Skip to next comma at depth 0.
        let mut depth = 0;
        let mut in_string = false;
        while i < bytes.len() {
            let c = bytes[i];
            if in_string {
                if c == b'"' {
                    in_string = false;
                }
            } else {
                match c {
                    b'"' => in_string = true,
                    b'[' | b'(' => depth += 1,
                    b']' | b')' => depth -= 1,
                    b',' if depth == 0 => {
                        i += 1;
                        break;
                    }
                    _ => {}
                }
            }
            i += 1;
        }
    }
    false
}

/// Read the value text for `key` in an attr-list inner. Returns the unquoted
/// string for quoted values and the raw token for identifiers/numbers.
fn get_attr_value(inner: &str, key: &str) -> Option<String> {
    let mut i = 0;
    let bytes = inner.as_bytes();
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < bytes.len() && is_ident_char(bytes[i]) {
            i += 1;
        }
        let ident = &inner[start..i];
        let key_match = ident == key;
        // Skip `=` (and any whitespace around it).
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if key_match {
            if i >= bytes.len() || bytes[i] != b'=' {
                return None;
            }
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() {
                return None;
            }
            if bytes[i] == b'"' {
                // Read until the closing unescaped quote.
                i += 1;
                let v_start = i;
                let mut out = String::new();
                while i < bytes.len() {
                    let c = bytes[i];
                    if c == b'\\' && i + 1 < bytes.len() {
                        out.push(bytes[i + 1] as char);
                        i += 2;
                        continue;
                    }
                    if c == b'"' {
                        return Some(if out.is_empty() {
                            inner[v_start..i].to_string()
                        } else {
                            out
                        });
                    }
                    out.push(c as char);
                    i += 1;
                }
                return None;
            }
            // Unquoted value: read until next comma at depth 0.
            let v_start = i;
            let mut depth = 0;
            while i < bytes.len() {
                let c = bytes[i];
                match c {
                    b'[' | b'(' => depth += 1,
                    b']' | b')' => depth -= 1,
                    b',' if depth == 0 => break,
                    _ => {}
                }
                i += 1;
            }
            return Some(inner[v_start..i].trim().to_string());
        }
        // Skip past this attr to the next comma at depth 0.
        let mut depth = 0;
        let mut in_string = false;
        while i < bytes.len() {
            let c = bytes[i];
            if in_string {
                if c == b'"' {
                    in_string = false;
                }
            } else {
                match c {
                    b'"' => in_string = true,
                    b'[' | b'(' => depth += 1,
                    b']' | b')' => depth -= 1,
                    b',' if depth == 0 => {
                        i += 1;
                        break;
                    }
                    _ => {}
                }
            }
            i += 1;
        }
    }
    None
}

/// Rewrite `key = …` (up to its closing comma or end-of-list) with
/// `key = "value"`.
fn replace_attr_value(inner: &str, key: &str, value: &str) -> String {
    let bytes = inner.as_bytes();
    let mut out = String::with_capacity(inner.len() + value.len());
    let mut i = 0;
    while i < bytes.len() {
        // Copy whitespace + comments verbatim.
        let preserve_start = i;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out.push_str(&inner[preserve_start..i]);
            continue;
        }
        out.push_str(&inner[preserve_start..i]);
        if i >= bytes.len() {
            break;
        }
        // Read an ident.
        let start = i;
        while i < bytes.len() && is_ident_char(bytes[i]) {
            i += 1;
        }
        let ident = &inner[start..i];
        // Find the next comma (or end) at depth 0.
        let mut j = i;
        let mut depth = 0;
        let mut in_string = false;
        while j < bytes.len() {
            let c = bytes[j];
            if in_string {
                if c == b'"' {
                    in_string = false;
                }
            } else {
                match c {
                    b'"' => in_string = true,
                    b'[' | b'(' => depth += 1,
                    b']' | b')' => depth -= 1,
                    b',' if depth == 0 => break,
                    _ => {}
                }
            }
            j += 1;
        }
        if ident == key {
            out.push_str(ident);
            out.push_str(" = \"");
            out.push_str(&escape_dsl_string(value));
            out.push('"');
        } else {
            out.push_str(&inner[start..j]);
        }
        if j < bytes.len() && bytes[j] == b',' {
            out.push(',');
            j += 1;
        }
        i = j;
    }
    out
}

/// Append `key = "value"` to an attr-list inner, inserting a comma between
/// the existing content and the new entry as needed.
fn append_attr(inner: &str, key: &str, value: &str) -> String {
    let trimmed = inner.trim_end();
    let needs_comma = !trimmed.is_empty() && !trimmed.ends_with(',');
    let trailing_ws = &inner[trimmed.len()..];
    let escaped = escape_dsl_string(value);
    let mut out = String::with_capacity(inner.len() + key.len() + escaped.len() + 8);
    out.push_str(trimmed);
    if needs_comma {
        out.push(',');
    }
    if !trimmed.is_empty() {
        out.push(' ');
    }
    out.push_str(key);
    out.push_str(" = \"");
    out.push_str(&escaped);
    out.push('"');
    out.push_str(trailing_ws);
    out
}

/// Sanitise a value for embedding inside a DSL string literal.
///
/// The DSL grammar (see `grammar.pest`) does not support escape sequences
/// inside strings, so a literal `"` would terminate the value early and a
/// trailing `\` would confuse the meta-block scanners. Substitute both with
/// readable replacements rather than truncating the prompt.
fn escape_dsl_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push('\''),
            '\\' => out.push('/'),
            '\n' | '\r' | '\t' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_present_meta() {
        let src = "meta (name=\"chair\", version=\"1.0\", tags=[\"a\",\"b\"])\nscene { }\n";
        let ast = crate::parse(src).unwrap();
        let m = extract_meta(&ast).unwrap();
        assert_eq!(m.name.as_deref(), Some("chair"));
        assert_eq!(m.version.as_deref(), Some("1.0"));
        assert_eq!(m.tags, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn extract_missing_meta() {
        let ast = crate::parse("scene { }\n").unwrap();
        assert!(extract_meta(&ast).is_none());
    }

    #[test]
    fn extract_seed_thinking_prompt() {
        let src = "meta (seed=\"1777726918483806000\", thinking=\"medium\", prompt=\"a chair\")\nscene { }\n";
        let ast = crate::parse(src).unwrap();
        let m = extract_meta(&ast).unwrap();
        assert_eq!(m.seed, Some(1777726918483806000));
        assert_eq!(m.thinking.as_deref(), Some("medium"));
        assert_eq!(m.prompt.as_deref(), Some("a chair"));
    }

    #[test]
    fn stamp_inserts_when_absent() {
        let src = "scene {\n  box \"b\" (size=[1,1,1])\n}\n";
        let out = stamp_mogen_version(src, "0.1.1");
        assert!(out.contains("meta (mogen_version = \"0.1.1\")"));
        let meta_pos = out.find("meta (").unwrap();
        let scene_pos = out.find("scene {").unwrap();
        assert!(meta_pos < scene_pos);
    }

    #[test]
    fn stamp_updates_existing_mogen_version() {
        let src = "meta (name = \"x\", mogen_version = \"0.0.1\", version = \"1.0\")\nscene {}\n";
        let out = stamp_mogen_version(src, "0.1.1");
        assert!(out.contains("mogen_version = \"0.1.1\""));
        assert!(!out.contains("0.0.1"));
        assert!(out.contains("name = \"x\""));
        assert!(out.contains("version = \"1.0\""));
    }

    #[test]
    fn stamp_appends_when_meta_lacks_version() {
        let src = "meta (name = \"x\")\nscene {}\n";
        let out = stamp_mogen_version(src, "0.1.1");
        assert!(out.contains("name = \"x\""));
        assert!(out.contains("mogen_version = \"0.1.1\""));
    }

    #[test]
    fn stamp_inserts_for_empty_file() {
        let out = stamp_mogen_version("", "0.1.1");
        assert!(out.starts_with("meta (mogen_version = \"0.1.1\")"));
    }

    #[test]
    fn upsert_creates_meta_for_seed() {
        let src = "scene { }\n";
        let out = upsert_meta_attr(src, "seed", "42");
        assert!(out.contains("meta (seed = \"42\")"));
        assert_eq!(read_meta_attr(&out, "seed").as_deref(), Some("42"));
    }

    #[test]
    fn upsert_appends_alongside_existing_attrs() {
        let src = "meta (mogen_version = \"0.1.1\")\nscene {}\n";
        let out = upsert_meta_attr(src, "thinking", "medium");
        assert!(out.contains("mogen_version = \"0.1.1\""));
        assert!(out.contains("thinking = \"medium\""));
    }

    #[test]
    fn read_meta_attr_handles_quotes_and_idents() {
        let src = "meta (name = chair, version = \"1.0\")\nscene {}\n";
        assert_eq!(read_meta_attr(src, "name").as_deref(), Some("chair"));
        assert_eq!(read_meta_attr(src, "version").as_deref(), Some("1.0"));
        assert!(read_meta_attr(src, "missing").is_none());
    }

    #[test]
    fn read_meta_attr_returns_none_without_meta() {
        assert!(read_meta_attr("scene {}\n", "seed").is_none());
    }

    #[test]
    fn strip_legacy_seed_comments_removes_header() {
        let src = "// mogen-generate seed=42\n// mogen-generate thinking=medium\n// prompt: foo\nscene {}\n";
        let out = strip_legacy_seed_comments(src);
        assert_eq!(out, "scene {}\n");
    }

    #[test]
    fn strip_legacy_seed_comments_preserves_other_comments() {
        let src = "// mogen-generate seed=42\n// my notes here\nscene {}\n";
        let out = strip_legacy_seed_comments(src);
        assert!(!out.contains("seed=42"));
        assert!(out.contains("// my notes here"));
    }

    #[test]
    fn strip_legacy_seed_comments_only_touches_leading_block() {
        let src = "scene {} // mogen-generate seed=42\n";
        assert_eq!(strip_legacy_seed_comments(src), src);
    }

    #[test]
    fn upsert_sanitises_special_characters_in_prompt() {
        let src = "scene {}\n";
        let prompt = "a \"quoted\" prompt with\nnewline and \\ backslash";
        let out = upsert_meta_attr(src, "prompt", prompt);
        // The DSL grammar has no escape sequences, so we substitute unsafe
        // characters with readable replacements: " → ', \ → /, newline → space.
        let read = read_meta_attr(&out, "prompt").unwrap();
        assert_eq!(read, "a 'quoted' prompt with newline and / backslash");
        // Round-trip through the parser: still extractable as a meta value.
        let ast = crate::parse(&out).unwrap();
        let meta = crate::extract_meta(&ast).unwrap();
        assert_eq!(meta.prompt.as_deref(), Some("a 'quoted' prompt with newline and / backslash"));
    }
}
