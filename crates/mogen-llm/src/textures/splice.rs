use std::collections::HashMap;

use anyhow::{bail, Result};
use mogen_core::Span;

/// Splice edit: add `attr="rel_path"` into the attr list whose outer node
/// covers `span`, unless the attr is already present. Produced by
/// [`super::run::run_plan`] and consumed by [`splice_textures`].
#[derive(Debug, Clone)]
pub struct Edit {
    pub span: Span,
    pub attr: &'static str,
    pub rel_path: String,
}

/// Apply a batch of [`Edit`]s to `src`. Edits that touch the same material
/// node are merged into a single rewrite, and any attr already present in
/// that node is left untouched.
pub fn splice_textures(src: &str, edits: &[Edit]) -> Result<String> {
    // Group edits by material span so we rewrite each node at most once.
    let mut by_span: HashMap<(usize, usize), Vec<&Edit>> = HashMap::new();
    for e in edits {
        by_span
            .entry((e.span.start, e.span.end))
            .or_default()
            .push(e);
    }

    // Apply in reverse span order so earlier byte offsets aren't invalidated.
    let mut keys: Vec<(usize, usize)> = by_span.keys().copied().collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.0));

    let mut out = src.to_string();
    for k in keys {
        let group = by_span.remove(&k).unwrap();
        let span = Span { start: k.0, end: k.1 };
        out = splice_many(&out, span, &group)?;
    }
    Ok(out)
}

/// Rewrite one material node, inserting every requested attr that isn't
/// already declared. If the node has no attr list (`material "x"` with no
/// parens), one is added.
fn splice_many(src: &str, span: Span, edits: &[&Edit]) -> Result<String> {
    if span.end > src.len() || span.start > span.end {
        bail!("bad span {:?} for source of len {}", span, src.len());
    }
    let slice = &src[span.start..span.end];

    let open = slice.find('(');
    let close = slice.rfind(')');

    let mut out = String::with_capacity(src.len() + 128);
    out.push_str(&src[..span.start]);

    match (open, close) {
        (Some(o), Some(c)) if c > o => {
            let body = &slice[o + 1..c];
            let new_attrs: Vec<String> = edits
                .iter()
                .filter(|e| !attr_already_present(body, e.attr))
                .map(|e| format!(r#"{}="{}""#, e.attr, e.rel_path))
                .collect();

            out.push_str(&slice[..=o]);
            if body.trim().is_empty() {
                out.push_str(&new_attrs.join(", "));
            } else {
                out.push_str(body);
                if !new_attrs.is_empty() {
                    let trimmed = body.trim_end();
                    if !trimmed.ends_with(',') {
                        out.push_str(", ");
                    }
                    out.push_str(&new_attrs.join(", "));
                }
            }
            out.push_str(&slice[c..]);
        }
        _ => {
            let new_attrs: Vec<String> = edits
                .iter()
                .map(|e| format!(r#"{}="{}""#, e.attr, e.rel_path))
                .collect();
            out.push_str(slice);
            out.push_str(&format!(" ({})", new_attrs.join(", ")));
        }
    }

    out.push_str(&src[span.end..]);
    Ok(out)
}

/// Rough check whether `attr=` already appears in the parenthesised body of
/// a material declaration. Looks for the attr name followed (after whitespace)
/// by `=`. Fine for our well-formed AST inputs — we never call this on
/// arbitrary text.
fn attr_already_present(body: &str, attr: &str) -> bool {
    let needle = attr;
    let mut idx = 0;
    while let Some(pos) = body[idx..].find(needle) {
        let abs = idx + pos;
        let before_ok = abs == 0
            || matches!(
                body.as_bytes()[abs - 1],
                b',' | b'(' | b' ' | b'\t' | b'\n'
            );
        let after = &body[abs + needle.len()..];
        let after_ok = after.trim_start().starts_with('=');
        if before_ok && after_ok {
            return true;
        }
        idx = abs + needle.len();
    }
    false
}

/// Convert a material name into a filesystem-safe stem.
pub fn safe_filename_stem(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if c == '_' || c == '-' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('_');
        }
        // Drop everything else.
    }
    if out.is_empty() {
        "material".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogen_dsl::ast::Node;
    use mogen_dsl::parse;

    fn parse_or_panic(src: &str) -> Vec<Node> {
        parse(src).expect("parse")
    }

    fn e(span: Span, attr: &'static str, rel: &str) -> Edit {
        Edit { span, attr, rel_path: rel.to_string() }
    }

    #[test]
    fn splice_inserts_before_closing_paren() {
        let src = r#"material "wood" (color=[0.5, 0.3, 0.1], roughness=0.8)"#;
        let ast = parse_or_panic(src);
        let node = &ast[0];
        let out = splice_textures(
            src,
            &[e(node.span, "base_color_texture", "textures/wood_albedo.png")],
        )
        .unwrap();
        assert!(out.contains(r#", base_color_texture="textures/wood_albedo.png")"#));
        assert!(out.contains("color=[0.5, 0.3, 0.1]"));
        assert!(out.contains("roughness=0.8"));
    }

    #[test]
    fn splice_handles_empty_attr_list() {
        let src = r#"material "x" ()"#;
        let ast = parse_or_panic(src);
        let node = &ast[0];
        let out = splice_textures(
            src,
            &[e(node.span, "base_color_texture", "textures/x_albedo.png")],
        )
        .unwrap();
        assert_eq!(
            out,
            r#"material "x" (base_color_texture="textures/x_albedo.png")"#
        );
    }

    #[test]
    fn splice_preserves_trailing_comma() {
        let src = r#"material "x" (color=[1,0,0],)"#;
        let ast = parse_or_panic(src);
        let node = &ast[0];
        let out =
            splice_textures(src, &[e(node.span, "base_color_texture", "t.png")]).unwrap();
        assert!(out.contains(r#"color=[1,0,0],base_color_texture="t.png""#));
    }

    #[test]
    fn splice_many_attrs_on_same_material() {
        let src = r#"material "wood" (color=[1,0,0])"#;
        let ast = parse_or_panic(src);
        let node = &ast[0];
        let out = splice_textures(
            src,
            &[
                e(node.span, "base_color_texture", "t/wood_albedo.png"),
                e(node.span, "normal_texture", "t/wood_normal.png"),
                e(node.span, "metallic_roughness_texture", "t/wood_mr.png"),
                e(node.span, "occlusion_texture", "t/wood_ao.png"),
            ],
        )
        .unwrap();
        assert!(out.contains(r#"base_color_texture="t/wood_albedo.png""#));
        assert!(out.contains(r#"normal_texture="t/wood_normal.png""#));
        assert!(out.contains(r#"metallic_roughness_texture="t/wood_mr.png""#));
        assert!(out.contains(r#"occlusion_texture="t/wood_ao.png""#));
        // Original attr intact.
        assert!(out.contains("color=[1,0,0]"));
    }

    #[test]
    fn splice_skips_attrs_already_present() {
        let src = r#"material "wood" (color=[1,0,0], normal_texture="old.png")"#;
        let ast = parse_or_panic(src);
        let node = &ast[0];
        let out = splice_textures(
            src,
            &[
                e(node.span, "base_color_texture", "t/a.png"),
                // This one duplicates an existing attr; must not be inserted.
                e(node.span, "normal_texture", "t/n.png"),
            ],
        )
        .unwrap();
        assert!(out.contains(r#"base_color_texture="t/a.png""#));
        // Only one normal_texture definition — the original one.
        assert_eq!(out.matches("normal_texture=").count(), 1);
        assert!(out.contains(r#"normal_texture="old.png""#));
    }

    #[test]
    fn splice_many_in_reverse_order() {
        let src = "material \"a\" (color=[1,0,0])\nmaterial \"b\" (color=[0,1,0])\n";
        let ast = parse_or_panic(src);
        let out = splice_textures(
            &src,
            &[
                e(ast[0].span, "base_color_texture", "a.png"),
                e(ast[1].span, "base_color_texture", "b.png"),
            ],
        )
        .unwrap();
        assert!(out.contains(r#", base_color_texture="a.png""#));
        assert!(out.contains(r#", base_color_texture="b.png""#));
        let a = out.find("\"a\"").unwrap();
        let b = out.find("\"b\"").unwrap();
        assert!(a < b);
    }

    #[test]
    fn safe_filename_stem_sanitizes() {
        assert_eq!(safe_filename_stem("oak wood"), "oak_wood");
        assert_eq!(safe_filename_stem("Rust/Iron"), "rustiron");
        assert_eq!(safe_filename_stem("my-mat_01"), "my-mat_01");
        assert_eq!(safe_filename_stem(""), "material");
    }

    #[test]
    fn attr_already_present_matches_whole_words() {
        let body = r#"color=[1,0,0], normal_texture="a.png""#;
        assert!(attr_already_present(body, "normal_texture"));
        assert!(attr_already_present(body, "color"));
        // Mustn't match a prefix of another attr: "texture" should not match
        // "normal_texture" here because there's no `texture=` in the body.
        assert!(!attr_already_present(body, "texture"));
        assert!(!attr_already_present(body, "base_color_texture"));
    }
}
