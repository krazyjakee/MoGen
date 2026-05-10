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

/// True when this material is configured for alpha-test cutout rendering
/// (`alpha_mode="mask"`). These materials need a cutout texture atlas — a
/// cluster of leaves / fronds / petals on a uniform pure-black background that
/// the texture pipeline post-processes into transparency — not a tileable
/// surface swatch. Ident and string forms both count.
pub fn is_mask_material(node: &Node) -> bool {
    matches!(
        node.attr("alpha_mode"),
        Some(Value::String(s) | Value::Ident(s)) if s == "mask"
    )
}

/// Read the optional `prompt="…"` attribute on a material. Authors use this
/// to override the auto-derived "material name + colour" framing the texture
/// pipeline falls back to — useful when the material name is generic
/// (`fabric_main`) or when the default phrasing trips Gemini's recitation
/// filter. Empty / whitespace-only strings are treated as absent so a stale
/// `prompt=""` left over from an LLM repair doesn't suppress the fallback.
pub fn material_prompt(node: &Node) -> Option<&str> {
    let v = node.attr("prompt")?;
    let s = match v {
        Value::String(s) | Value::Ident(s) => s.as_str(),
        _ => return None,
    };
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Marker line the retry helper rewrites on `IMAGE_RECITATION`. The prompt
/// builders emit at most one `Material note: …` line — sourced from the
/// material's own `prompt="…"` attribute — so the retry path can find,
/// rephrase, and replace it without disturbing the rest of the prompt. The
/// note is supplementary on top of the auto-derived subject (material name,
/// colour, anatomy motif), not a replacement for it.
pub const NOTE_PREFIX: &str = "Material note: ";

/// One `decal` node the texture pipeline should produce an image for. Carries
/// the AST node so callers can read placement-related attrs and the resolved
/// display name (decal name, falling back to "decal").
pub struct DecalHit<'a> {
    pub node: &'a Node,
    pub name: String,
}

/// Recursively walk `ast` and pull out every `decal` node. Unlike `material`,
/// decals can live anywhere geometry can — inside `scene`, `group`, `solid`,
/// CSG containers, etc. — so the walk is depth-first across all children.
pub fn collect_decals<'a>(ast: &'a [Node]) -> Vec<DecalHit<'a>> {
    fn walk<'a>(n: &'a Node, out: &mut Vec<DecalHit<'a>>) {
        if n.kind == "decal" {
            let name = n.name.clone().unwrap_or_else(|| "decal".to_string());
            out.push(DecalHit { node: n, name });
        }
        for c in &n.children {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    for n in ast {
        walk(n, &mut out);
    }
    out
}

/// Build the image prompt for a `decal` node. The prompt asks Gemini for an
/// RGBA cutout against a transparent background — no chroma-key step, the
/// alpha channel is requested directly. Source of the user-facing description,
/// in priority order: the `prompt="…"` attribute, then the decal's name.
pub fn build_decal_prompt(hit: &DecalHit<'_>, style: &str) -> String {
    let intent = decal_intent(hit);
    let mut s = String::new();
    s.push_str(
        "Output a transparent-background DECAL image for projection onto a 3D \
         model surface — a logo, sticker, label, badge, handwritten note, \
         seal, stamp, or similar small piece of artwork. The image is a \
         flat 2D motif rendered against a fully transparent backdrop \
         (alpha = 0 outside the artwork). The 3D scene this decal will be \
         pasted onto is rendered separately; this image must NOT contain \
         any of that scene.\n\n\
         Framing: the artwork sits centred in a square frame, fitting \
         comfortably within ~85% of the image area so the silhouette has a \
         small alpha margin. The artwork itself is what the viewer sees; \
         everything outside it must be fully transparent (no background, \
         no card, no paper, no fabric \u{2014} just alpha=0).\n\n\
         Hard exclusions (image must contain NONE of these):\n\
         - no background, no scenery, no environment, no surface texture \
         behind the motif\n\
         - no body parts, characters, animals, or models the decal would be \
         applied to (we are NOT rendering the shirt/page/wall \u{2014} just \
         the decal that goes on it)\n\
         - no drop shadows, glows, halos, vignettes, or rim lighting bleeding \
         into the alpha channel\n\
         - no rectangular borders, frames, sticker outlines, or paper edges \
         unless those are explicitly part of the requested artwork\n\
         - no watermarks, signatures, or unrelated text — only what the \
         description asks for\n\n\
         Required qualities:\n\
         - RGBA output: opaque pixels for the artwork, fully transparent \
         (alpha=0) for everything else; soft anti-aliased alpha along edges \
         is fine\n\
         - flat, even, diffuse lighting on the artwork itself \u{2014} no \
         strong directional highlights\n\
         - crisp silhouette: the motif should still read clearly when scaled \
         down to ~256px\n\
         - square aspect, motif centred in the frame\n\n",
    );
    s.push_str(&format!("Decal subject: {intent}\n"));
    s.push_str(&format!("Style: {style}\n"));
    s.push_str(
        "\nReminder: a single decal motif on FULLY TRANSPARENT background. \
         No surrounding paper, fabric, wall, or scene. The transparent area \
         is the alpha channel \u{2014} not white, not black, not a colour key.",
    );
    s
}

fn decal_intent(hit: &DecalHit<'_>) -> String {
    if let Some(p) = hit.node.attr_string("prompt") {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // Fall back to the decal's name — usually a short identifier, but for
    // decals it's idiomatic to write a longer descriptive name (e.g.
    // `decal "embroidered logo, white thread"`), so this is often enough.
    hit.name.clone()
}

/// Build the image prompt for one material. Two prompt families:
///
/// - **Surface swatch** (default): a tileable PBR albedo for a continuous
///   surface. Framed as a macro-photo of a 30 cm \u{00D7} 30 cm sample with an
///   exclusion list that stops Gemini from drawing the material's *subject*
///   (a tiger, a chair) onto every swatch. Tileability is spelled out as an
///   edge-wrap requirement.
/// - **Cutout atlas** (when the material has `alpha_mode="mask"`): a cluster
///   of leaves / fronds / petals on a uniform pure-black background. The
///   texture pipeline post-processes the black background into alpha=0 so
///   the leaf_card mesh shows the cluster shape, the way game-foliage atlas
///   textures do in SpeedTree-style trees.
///
/// Both branches honour the optional motif hint (de-duped `role=`/`tags=`
/// values) framed as inspiration with a "do not depict" guard so Gemini
/// doesn't draw the body part.
pub fn build_prompt(hit: &MaterialHit<'_>, style: &str, anatomy: Option<&str>) -> String {
    if is_mask_material(hit.node) {
        return build_cutout_atlas_prompt(hit, style, anatomy);
    }
    if hit.name == "face" {
        return build_face_panel_prompt(hit, style);
    }
    build_surface_swatch_prompt(hit, style, anatomy)
}

/// Special-case prompt for `humanoid_full`'s painted face panel. The texture
/// is NOT tileable — it lands on a single front-face quad and renders the
/// character's eyes, brows, nose hint, and mouth as flat painted features.
/// Background is a solid skin tone so the panel blends with the head's edges.
fn build_face_panel_prompt(hit: &MaterialHit<'_>, style: &str) -> String {
    let color = hit.node.attr("color").and_then(|v| match v {
        Value::Vec3([r, g, b]) => Some([*r, *g, *b]),
        _ => None,
    });
    let mut s = String::new();
    s.push_str(
        "Output a single 512×512 PNG of a stylized low-poly character face, \
         painted onto a flat panel. This will be applied as a base-color \
         texture to the front face of a Synty/Quaternius-style humanoid head.\n\n\
         Required composition:\n\
         - facial features painted in flat colour blocks: two simple \
         almond eyes (dark sclera or solid dot) at vertical center, \
         small dark mouth in the lower third, optional thin eyebrow lines \
         above the eyes\n\
         - features symmetrical, centered horizontally\n\
         - background: uniform skin tone fills the entire panel (no \
         transparency, no border, no margin) so edges blend seamlessly \
         with the head geometry\n\n\
         Hard exclusions:\n\
         - no nose geometry (the head model carries a separate nose mesh)\n\
         - no hair, no ears, no neck, no head silhouette outline\n\
         - no scenery, props, text, watermarks, or signatures\n\
         - no rim lighting, drop shadows, or 3D shading on the features — \
         flat painted look only\n\n",
    );
    if let Some([r, g, b]) = color {
        s.push_str(&format!(
            "Background skin tone (approximate, hex): {}\n",
            rgb_to_hex(r, g, b)
        ));
    }
    s.push_str(&format!("Style: {style}\n"));
    if let Some(note) = material_prompt(hit.node) {
        s.push_str(&format!("{NOTE_PREFIX}{note}\n"));
    }
    s
}

fn build_surface_swatch_prompt(
    hit: &MaterialHit<'_>,
    style: &str,
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
        "Output a single seamless, perfectly tileable PBR base-color (albedo) \
         texture map. This is a material swatch, NOT a picture of anything.\n\n\
         Framing: extreme overhead macro photograph of a flat 30 cm \u{00D7} 30 cm \
         material sample lying under a copy stand. The whole frame is one \
         continuous surface filling edge to edge.\n\n\
         Hard exclusions (image must contain NONE of these):\n\
         - no objects, props, tools, furniture, vehicles, or items of any kind\n\
         - no characters, people, faces, animals, or body parts (no fur on a \
         leg, no scales on a tail \u{2014} just the surface itself)\n\
         - no scenery, landscapes, skies, horizons, or environments\n\
         - no logos, text, numbers, watermarks, signatures, or symbols\n\
         - no borders, frames, vignettes, drop shadows, or rounded corners\n\
         - no directional lighting, cast shadows, baked ambient occlusion, \
         or specular highlights\n\n\
         Required qualities:\n\
         - repeating natural micro-detail across the entire frame, no central \
         focal point or composed subject\n\
         - tiles seamlessly: detail, color, and structure wrap continuously \
         across all four edges with no visible seam\n\
         - even, flat, diffuse lighting as if shot for a PBR scan library\n\
         - square aspect, surface fills the frame corner to corner\n\n",
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
    if let Some(hint) = anatomy {
        let trimmed = hint.trim();
        if !trimmed.is_empty() {
            s.push_str(&format!(
                "Pattern motif (texture inspiration only \u{2014} DO NOT depict \
                 the named body part, object, or scene; use it only to bias \
                 the surface pattern, e.g. \"back\" \u{2192} dorsal-stripe \
                 layout, \"belly\" \u{2192} paler finer texture): {trimmed}\n"
            ));
        }
    }
    // Author-supplied prompt is supplementary to the framing above — it
    // sharpens the subject (e.g. "navy nylon ripstop weave") without
    // replacing the material-name / colour / motif lines. The retry helper
    // rephrases just this line on `IMAGE_RECITATION`, leaving the rest
    // intact.
    if let Some(note) = material_prompt(hit.node) {
        s.push_str(&format!("{NOTE_PREFIX}{note}\n"));
    }
    s.push_str(
        "\nReminder: the output is a flat tileable surface scan. \
         No subject. No scene. Surface only.",
    );
    s
}

fn build_cutout_atlas_prompt(
    hit: &MaterialHit<'_>,
    style: &str,
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
        "Output a foliage CUTOUT ATLAS texture for an alpha-tested billboard \
         leaf card \u{2014} the kind used on game trees and bushes (SpeedTree-style). \
         The image is a *cluster* of overlapping leaves, fronds, or petals \
         photographed against a uniform pure-black background; the texture \
         pipeline keys the black to transparency so the leaf shapes become \
         the visible silhouette.\n\n\
         Framing: top-down view of a flat foliage spray pinned against a pure \
         black studio backdrop. 5\u{2013}15 leaves arranged naturally with \
         realistic overlap, slight rotation variation, and small gaps between \
         leaves so the black background reads through where the cluster ends. \
         Leaves fill most of the frame but do NOT touch the image edges \
         \u{2014} leave a thin black margin all around so the silhouette \
         resolves cleanly when the background is keyed to alpha.\n\n\
         Hard exclusions (image must contain NONE of these):\n\
         - no branches, twigs, stems, tree trunks, or wood (just the leaf \
         blades themselves \u{2014} their petioles can be hinted but no woody \
         growth)\n\
         - no characters, people, faces, animals, hands, or body parts\n\
         - no scenery, sky, ground, soil, water, mulch, or environment\n\
         - no logos, text, numbers, watermarks, signatures, or symbols\n\
         - no borders, frames, vignettes, drop shadows from the leaves onto \
         the backdrop, or directional rim lighting\n\
         - no gradient backgrounds, no studio bokeh, no atmospheric haze \
         \u{2014} the background is FLAT pure RGB(0, 0, 0)\n\n\
         Required qualities:\n\
         - background is solid, uniform, pure black (RGB 0, 0, 0) everywhere \
         outside the leaf silhouettes; no near-black grays, no dark green \
         spill, no gradient\n\
         - leaves are clearly defined with crisp silhouette edges so the \
         alpha cutout reads cleanly at small sizes\n\
         - flat, even, diffuse lighting on the leaves themselves \u{2014} no \
         strong specular, no cast shadows on neighbouring leaves\n\
         - square aspect, the cluster centred in the frame\n\n",
    );
    s.push_str(&format!("Material name: {}\n", hit.name));
    if let Some([r, g, b]) = color {
        s.push_str(&format!(
            "Target leaf color (approximate, hex): {}\n",
            rgb_to_hex(r, g, b)
        ));
    }
    if let Some(r) = roughness {
        s.push_str(&format!("Leaf surface finish: {}\n", roughness_word(r)));
    }
    s.push_str(&format!("Style: {style}\n"));
    if let Some(hint) = anatomy {
        let trimmed = hint.trim();
        if !trimmed.is_empty() {
            s.push_str(&format!(
                "Species / shape motif (use only to bias leaf SHAPE and \
                 arrangement \u{2014} do NOT draw the named subject): {trimmed}\n"
            ));
        }
    }
    if let Some(note) = material_prompt(hit.node) {
        s.push_str(&format!("{NOTE_PREFIX}{note}\n"));
    }
    s.push_str(
        "\nReminder: foliage cluster on PURE BLACK. Keep a thin black margin \
         around the cluster. No branches, no scene, no horizons. The black \
         backdrop becomes transparent in the final asset.",
    );
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

/// Read the original natural-language prompt from the file's `meta(prompt=…)`
/// attribute, falling back to the legacy `// prompt: …` comment header for
/// files written by older versions of MoGen. Used as subject-context
/// enrichment for per-material prompts.
pub fn parse_prompt_header(src: &str) -> Option<String> {
    if let Some(v) = mogen_dsl::read_meta_attr(src, "prompt") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
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
    fn build_prompt_enforces_surface_only_framing() {
        // The exclusion list is the load-bearing part of the rewrite — if
        // these phrases ever drift out of the prompt, Gemini starts drawing
        // the material's subject (a tiger, a chair) instead of a swatch.
        let src = r#"material "stone" (color=[0.5,0.5,0.5])"#;
        let ast = parse_or_panic(src);
        let hits = collect_materials(&ast);
        let p = build_prompt(&hits[0], "photorealistic", None);
        assert!(p.contains("seamless"));
        assert!(p.contains("tileable"));
        assert!(p.contains("no characters"));
        assert!(p.contains("body parts"));
        assert!(p.contains("no scenery"));
        assert!(p.contains("Surface only"));
    }

    #[test]
    fn anatomy_hint_appears_when_provided() {
        let src = r#"material "fur" (color=[0.5, 0.3, 0.1], roughness=0.85)"#;
        let ast = parse_or_panic(src);
        let hits = collect_materials(&ast);
        let p = build_prompt(&hits[0], "photorealistic", Some("back, shoulder"));
        // Hint must come through, but framed as motif inspiration with the
        // explicit don't-depict guard so Gemini doesn't draw a shoulder.
        assert!(p.contains("back, shoulder"));
        assert!(p.contains("Pattern motif"));
        assert!(p.contains("DO NOT depict"));
    }

    #[test]
    fn material_prompt_appears_as_supplementary_note() {
        // Author-supplied `prompt="…"` is *additive*: the auto-derived
        // material-name / colour / anatomy framing stays, and the user's
        // text is appended as a `Material note:` line that the retry helper
        // can rephrase on `IMAGE_RECITATION`.
        let src = r#"material "fabric_main" (
            color=[0.15, 0.30, 0.60], roughness=0.9,
            prompt="navy nylon ripstop weave"
        )"#;
        let ast = parse_or_panic(src);
        let hits = collect_materials(&ast);
        let p = build_prompt(&hits[0], "photorealistic", Some("back"));
        assert!(p.contains("Material note: navy nylon ripstop weave"));
        // Auto-derived framing stays intact.
        assert!(p.contains("Material name: fabric_main"));
        assert!(p.contains("Pattern motif"));
    }

    #[test]
    fn empty_material_prompt_emits_no_note() {
        // A blank `prompt=""` (e.g. left over from an LLM repair) shouldn't
        // produce an empty `Material note:` line — the absence is treated
        // identically to the attribute being missing.
        let src = r#"material "stone" (color=[0.5,0.5,0.5], prompt="   ")"#;
        let ast = parse_or_panic(src);
        let hits = collect_materials(&ast);
        let p = build_prompt(&hits[0], "photorealistic", None);
        assert!(!p.contains(NOTE_PREFIX));
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
    fn parse_prompt_header_reads_meta() {
        let src = "meta (prompt = \"a wooden stool\")\nmaterial \"a\" ()\n";
        assert_eq!(parse_prompt_header(src).as_deref(), Some("a wooden stool"));
    }

    #[test]
    fn parse_prompt_header_falls_back_to_legacy_comment() {
        let src = "// mogen-generate seed=1\n// prompt: a wooden stool\nmaterial \"a\" ()\n";
        assert_eq!(parse_prompt_header(src).as_deref(), Some("a wooden stool"));
    }

    #[test]
    fn parse_prompt_header_absent() {
        assert!(parse_prompt_header("material \"a\" ()").is_none());
    }

    #[test]
    fn mask_material_swaps_to_cutout_atlas_prompt() {
        // Default surface-swatch prompt is mutually exclusive with the cutout
        // atlas one — different exclusion lists, different framing. Detect the
        // alpha_mode="mask" flag and emit the foliage-cluster-on-black prompt
        // so the chroma-key step has a clean backdrop to convert to alpha.
        let src = r#"
            material "oak_leaf" (
                color=[0.2, 0.55, 0.22], roughness=0.65,
                alpha_mode="mask", alpha_cutoff=0.5, double_sided=1
            )
        "#;
        let ast = parse_or_panic(src);
        let hits = collect_materials(&ast);
        assert!(is_mask_material(hits[0].node));
        let p = build_prompt(&hits[0], "photorealistic", None);
        // Cutout-atlas-specific framing.
        assert!(p.contains("CUTOUT ATLAS"), "missing cutout-atlas tag");
        assert!(p.contains("pure-black background"));
        assert!(p.contains("cluster"), "missing cluster framing");
        assert!(p.contains("realistic overlap"), "missing overlap guidance");
        assert!(p.contains("RGB(0, 0, 0)"));
        // Must NOT carry the swatch prompt's tileable-surface framing.
        assert!(
            !p.contains("perfectly tileable"),
            "cutout prompt leaked tileable-surface phrasing"
        );
        assert!(
            !p.contains("material swatch"),
            "cutout prompt leaked swatch phrasing"
        );
    }

    #[test]
    fn opaque_material_keeps_surface_swatch_prompt() {
        // Default path stays untouched for materials that don't opt in to
        // alpha cutout — bark, stone, wood etc.
        let src = r#"material "oak_bark" (color=[0.36, 0.25, 0.15], roughness=0.95)"#;
        let ast = parse_or_panic(src);
        let hits = collect_materials(&ast);
        assert!(!is_mask_material(hits[0].node));
        let p = build_prompt(&hits[0], "photorealistic", None);
        assert!(p.contains("perfectly tileable"));
        assert!(!p.contains("CUTOUT ATLAS"));
    }

    #[test]
    fn cutout_prompt_carries_color_and_motif_hints() {
        // Authored color + species motif must come through the cutout branch
        // too — that's how Gemini knows whether to draw oak vs. maple vs. fern.
        let src = r#"material "leaf" (color=[0.18, 0.45, 0.20], alpha_mode="mask")"#;
        let ast = parse_or_panic(src);
        let hits = collect_materials(&ast);
        let p = build_prompt(&hits[0], "stylized", Some("oak, autumn"));
        assert!(p.contains("Target leaf color"));
        assert!(p.contains("#2E7333"));
        assert!(p.contains("Style: stylized"));
        assert!(p.contains("oak, autumn"));
        assert!(p.contains("Species / shape motif"));
    }
}
