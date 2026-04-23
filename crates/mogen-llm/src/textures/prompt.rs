use std::collections::{BTreeSet, HashMap};

use mogen_dsl::ast::{Node, Value};

/// A single material the AST reports — name + full node span + attrs.
/// We keep a reference to the [`Node`] so callers can inspect existing attrs
/// (e.g. to skip materials that already declare a texture).
pub struct MaterialHit<'a> {
    pub node: &'a Node,
    pub name: String,
}

/// Extract every top-level `material` node from the parsed AST.
pub fn collect_materials<'a>(ast: &'a [Node]) -> Vec<MaterialHit<'a>> {
    ast.iter()
        .filter(|n| n.kind == "material")
        .filter_map(|n| {
            n.name.as_ref().map(|name| MaterialHit {
                node: n,
                name: name.clone(),
            })
        })
        .collect()
}

/// Build the image prompt for one material. Includes:
///   - material name (strongest signal — "oak" vs "denim" drives the output),
///   - authored color as an RGB hex hint (preserves artist intent),
///   - a rough/polished word from `roughness`,
///   - an optional subject hint parsed from the DSL's `// prompt:` header,
///   - an optional anatomy hint (de-duped `role=`/`tags=` values from primitives
///     that reference this material) — disambiguates fur on a tiger's
///     shoulder vs. its belly when both share the same `<creature>_fur`
///     style but want different patterns.
pub fn build_prompt(
    hit: &MaterialHit<'_>,
    style: &str,
    subject: Option<&str>,
    anatomy: Option<&str>,
) -> String {
    let color = hit.node.attr("color").and_then(|v| match v {
        Value::Vec3([r, g, b]) => Some([*r, *g, *b]),
        _ => None,
    });
    let roughness = hit.node.attr("roughness").and_then(|v| match v {
        Value::Number(n) => Some(*n),
        _ => None,
    });

    let mut s = String::new();
    s.push_str(
        "Seamless tileable PBR base-color (albedo) texture. \
         Flat overhead lighting, no directional shadows, no baked-in ambient occlusion, \
         no highlights. The image must tile perfectly when placed edge-to-edge. \
         Output a square image.\n\n",
    );
    s.push_str(&format!("Material name: {}\n", hit.name));
    if let Some([r, g, b]) = color {
        s.push_str(&format!(
            "Target color (approximate, hex): {}\n",
            rgb_to_hex(r, g, b)
        ));
    }
    if let Some(r) = roughness {
        s.push_str(&format!("Surface finish: {}\n", roughness_word(r)));
    }
    s.push_str(&format!("Style: {style}\n"));
    if let Some(ctx) = subject {
        let trimmed = ctx.trim();
        if !trimmed.is_empty() {
            s.push_str(&format!("Subject context (for mood/era only): {trimmed}\n"));
        }
    }
    if let Some(hint) = anatomy {
        let trimmed = hint.trim();
        if !trimmed.is_empty() {
            s.push_str(&format!("Anatomy / role hints: {trimmed}\n"));
        }
    }
    s
}

/// Walk every node in `ast` and build a map from material-name → comma
/// separated anatomical hint string sourced from each user's `role=` and
/// `tags=` attrs. Used by the texture pipeline to enrich the per-material
/// albedo prompt — e.g. a tiger's `tiger_back_fur` material referenced by a
/// torso with `role="back", tags="dorsal"` gets `Anatomy / role hints:
/// back, dorsal` appended to the image prompt, steering the LLM toward
/// rosette patterns rather than flat colour.
pub fn collect_material_anatomy(ast: &[Node]) -> HashMap<String, String> {
    let mut hints: HashMap<String, BTreeSet<String>> = HashMap::new();
    fn walk(n: &Node, hints: &mut HashMap<String, BTreeSet<String>>) {
        if let Some(mat_name) = string_attr(n, "mat") {
            let bag = hints.entry(mat_name).or_default();
            for key in &["role", "tags"] {
                if let Some(s) = string_attr(n, key) {
                    for piece in s.split(|c: char| c == ',' || c.is_whitespace()) {
                        let p = piece.trim();
                        if !p.is_empty() && p != "floating" {
                            bag.insert(p.to_string());
                        }
                    }
                }
            }
        }
        for c in &n.children {
            walk(c, hints);
        }
    }
    for n in ast {
        walk(n, &mut hints);
    }
    hints
        .into_iter()
        .map(|(k, set)| {
            let joined = set.into_iter().collect::<Vec<_>>().join(", ");
            (k, joined)
        })
        .collect()
}

fn string_attr(n: &Node, key: &str) -> Option<String> {
    match n.attr(key)? {
        Value::String(s) | Value::Ident(s) => Some(s.clone()),
        _ => None,
    }
}

fn rgb_to_hex(r: f32, g: f32, b: f32) -> String {
    let c = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", c(r), c(g), c(b))
}

fn roughness_word(r: f32) -> &'static str {
    if r >= 0.85 {
        "very rough, fully matte"
    } else if r >= 0.6 {
        "rough, matte"
    } else if r >= 0.35 {
        "semi-gloss"
    } else if r >= 0.15 {
        "smooth, glossy"
    } else {
        "polished, near-mirror"
    }
}

/// Parse the first `// prompt: …` line produced by `embed_seed_header`, if
/// present. Used as subject-context enrichment for per-material prompts.
pub fn parse_prompt_header(src: &str) -> Option<String> {
    for line in src.lines().take(8) {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("// prompt:") {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogen_dsl::parse;

    fn parse_or_panic(src: &str) -> Vec<Node> {
        parse(src).expect("parse")
    }

    #[test]
    fn collects_top_level_materials_only() {
        let src = r#"material "wood" (color=[0.5, 0.3, 0.1])
material "fabric" (color=[0.2, 0.3, 0.5])
scene { box "b" (size=[1,1,1]) }"#;
        let ast = parse_or_panic(src);
        let mats = collect_materials(&ast);
        assert_eq!(mats.len(), 2);
        assert_eq!(mats[0].name, "wood");
        assert_eq!(mats[1].name, "fabric");
    }

    #[test]
    fn build_prompt_includes_name_color_and_roughness() {
        let src = r#"material "oak" (color=[0.55, 0.35, 0.18], roughness=0.75)"#;
        let ast = parse_or_panic(src);
        let hits = collect_materials(&ast);
        let p = build_prompt(&hits[0], "photorealistic", None, None);
        assert!(p.contains("Material name: oak"));
        assert!(p.contains("#8C592E"));
        assert!(p.contains("rough, matte"));
        assert!(p.contains("photorealistic"));
    }

    #[test]
    fn anatomy_hint_appears_when_provided() {
        let src = r#"material "fur" (color=[0.5, 0.3, 0.1], roughness=0.85)"#;
        let ast = parse_or_panic(src);
        let hits = collect_materials(&ast);
        let p = build_prompt(&hits[0], "photorealistic", None, Some("back, shoulder"));
        assert!(p.contains("Anatomy / role hints: back, shoulder"));
    }

    #[test]
    fn collect_material_anatomy_dedups_role_and_tags() {
        let src = r#"
            material "fur" (color=[0.5,0.3,0.1])
            scene {
              capsule "leg_l" (mat="fur", role="leg", tags="left,limb", radius=0.05, height=0.4)
              capsule "leg_r" (mat="fur", role="leg", tags="right,limb", radius=0.05, height=0.4)
            }
        "#;
        let ast = parse_or_panic(src);
        let hints = collect_material_anatomy(&ast);
        let fur = hints.get("fur").expect("fur hint present");
        // Set-based de-dup → "leg, left, limb, right" alphabetical.
        assert!(fur.contains("leg"));
        assert!(fur.contains("left"));
        assert!(fur.contains("right"));
        assert!(fur.contains("limb"));
    }

    #[test]
    fn parse_prompt_header_reads_first_8_lines() {
        let src = "// mogen-generate seed=1\n// prompt: a wooden stool\nmaterial \"a\" ()\n";
        assert_eq!(parse_prompt_header(src).as_deref(), Some("a wooden stool"));
    }

    #[test]
    fn parse_prompt_header_absent() {
        assert!(parse_prompt_header("material \"a\" ()").is_none());
    }
}
