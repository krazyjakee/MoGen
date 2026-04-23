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
///   - an optional subject hint parsed from the DSL's `// prompt:` header.
pub fn build_prompt(hit: &MaterialHit<'_>, style: &str, subject: Option<&str>) -> String {
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
    s
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
        let p = build_prompt(&hits[0], "photorealistic", None);
        assert!(p.contains("Material name: oak"));
        assert!(p.contains("#8C592E"));
        assert!(p.contains("rough, matte"));
        assert!(p.contains("photorealistic"));
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
