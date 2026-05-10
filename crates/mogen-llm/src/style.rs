//! Visual-style hint bundled with every LLM call. Steers geometry density,
//! material palette, and texture look toward one of a fixed catalogue of
//! video-game looks (PS1, voxel, cel-shaded, …).
//!
//! The style is opt-in: every public entry point that takes a `style`
//! argument accepts `Option<Style>`, and `None` is a complete passthrough
//! (no prompt suffix, no `meta(style=…)` line). Existing prompts and
//! goldens stay byte-identical when the user doesn't pick one.
//!
//! Round-trip:
//! - The CLI / Studio capture a `Style` selection at the call site.
//! - `apply_style_to_prompt` prepends a "## Style" guidance block to the
//!   user prompt so the model sees it once per call.
//! - After generation, `crate::stamp_style_header` writes
//!   `meta(style="<key>")` into the saved DSL via `upsert_meta_attr`.
//! - On the next modify/animate/repair, `crate::parse_style_header`
//!   recovers the style from the file so the same look applies even
//!   when the user didn't re-pick it on the CLI.

/// One of ten fixed video-game looks. Hand-curated rather than free-form
/// so (a) the dropdown stays opinionated and short, (b) the on-disk
/// `meta(style=…)` token is a stable enum key, and (c) downstream
/// pipelines (texture style hint, future material presets) can branch
/// on the variant rather than parsing arbitrary prose.
///
/// The string-typed `Meta::style` field, however, accepts any value —
/// hand-edited `.mog` files can carry an experimental key without
/// breaking the validator. Unknown values surface here as
/// `Style::parse` -> `None` and degrade gracefully to "no style".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    Ps1,
    N64,
    LowPoly,
    HighDetail,
    Arcade,
    Voxel,
    CelShaded,
    StylizedFantasy,
    Cyberpunk,
    PixelArt,
}

/// Catalogue order. Drives the Studio dropdown and the CLI `--help` listing
/// so the two surfaces always offer the same set in the same order.
pub const STYLES: &[Style] = &[
    Style::Ps1,
    Style::N64,
    Style::LowPoly,
    Style::HighDetail,
    Style::Arcade,
    Style::Voxel,
    Style::CelShaded,
    Style::StylizedFantasy,
    Style::Cyberpunk,
    Style::PixelArt,
];

impl Style {
    /// Slug stamped into `meta(style=…)` and accepted by [`Style::parse`].
    /// Snake-case matches the rest of MoGen's DSL attribute conventions.
    pub fn key(self) -> &'static str {
        match self {
            Style::Ps1 => "ps1",
            Style::N64 => "n64",
            Style::LowPoly => "low_poly",
            Style::HighDetail => "high_detail",
            Style::Arcade => "arcade",
            Style::Voxel => "voxel",
            Style::CelShaded => "cel_shaded",
            Style::StylizedFantasy => "stylized_fantasy",
            Style::Cyberpunk => "cyberpunk",
            Style::PixelArt => "pixel_art",
        }
    }

    /// Title-cased label for UI surfaces (Studio combobox, `--help` text).
    pub fn label(self) -> &'static str {
        match self {
            Style::Ps1 => "PS1",
            Style::N64 => "Nintendo 64",
            Style::LowPoly => "Low Poly",
            Style::HighDetail => "High Detail",
            Style::Arcade => "Arcade",
            Style::Voxel => "Voxel",
            Style::CelShaded => "Cel Shaded",
            Style::StylizedFantasy => "Stylized Fantasy",
            Style::Cyberpunk => "Cyberpunk",
            Style::PixelArt => "Pixel Art",
        }
    }

    /// One-paragraph guidance fed to the LLM via [`style_prompt_block`].
    /// Phrased as concrete authoring constraints rather than vibes so the
    /// model has actionable instructions for primitives, segment counts,
    /// material colour, and silhouette shape.
    pub fn description(self) -> &'static str {
        match self {
            Style::Ps1 => {
                "Original PlayStation: chunky low-poly geometry, cylinders capped at \
                 ~12 segments, vertex-lit shading with hard-edged faces, dithered \
                 low-resolution textures, no smooth normals. Avoid bevels and \
                 chamfers — embrace the angular silhouette."
            }
            Style::N64 => {
                "Nintendo 64: slightly higher polygon count than PS1 but still chunky, \
                 with rounded silhouettes and soft blurry filtered textures. Use a \
                 subdued, saturated palette and gentle curves; avoid fine geometric \
                 detail and sharp PS1-style facets."
            }
            Style::LowPoly => {
                "Modern indie low-poly: clean angular silhouettes, faceted \
                 shading with crisp triangle facets, \
                 oversized hero features (big hands as mittens, big boots, blocky \
                 head), flat single-colour painterly materials per surface, gentle \
                 chamfers on edges, and textures used only for painted face details \
                 — no fabric weave or brick patterns. For humanoid characters use \
                 `humanoid_full` with vec3 colour params; build clothing, weapons, \
                 and accessories as primitives and `attach` them to the figure's \
                 `slot_*` connectors (slot_crown, slot_chest_back, slot_waist_*, \
                 slot_hand_l_grip, slot_hand_r_grip, …)."
            }
            Style::HighDetail => {
                "Dense AAA geometry: high-segment curves (cylinders ≥ 32 segments, \
                 spheres ≥ 32 rings/segments), fine bevels and chamfers on every \
                 hard edge, subdivided meshes with secondary forms, and layered \
                 material slots that can carry separate metal / paint / trim regions."
            }
            Style::Arcade => {
                "1980s arcade cabinet: bold primary colours (saturated reds, blues, \
                 yellows), exaggerated chunky shapes, hard contrasts between \
                 elements, and no fine detail. Silhouettes should read at a glance \
                 from across a room."
            }
            Style::Voxel => {
                "Minecraft-style voxel: every primitive is a stack or grid of cubes, \
                 no curves anywhere. Prefer `box`, `grid`, and `stack` over \
                 `cylinder`, `sphere`, `capsule`, or `torus`. Silhouettes are blocky \
                 and axis-aligned; even nominally round things (heads, wheels) are \
                 cube clusters."
            }
            Style::CelShaded => {
                "Toon / anime cel-shaded: bold flat colour bands with sharp \
                 transitions between lit and shaded regions. Use exaggerated \
                 readable silhouettes; consider thin emissive outline materials on \
                 prominent edges. Avoid gradients and photorealistic textures."
            }
            Style::StylizedFantasy => {
                "Hand-painted Hearthstone / World of Warcraft: oversized hero \
                 features (big handles, big buckles, big blade fullers), \
                 exaggerated proportions that read at distance, warm painterly \
                 colours, and soft chamfers everywhere. Materials feel hand-painted \
                 rather than scanned."
            }
            Style::Cyberpunk => {
                "Neon-on-grime cyberpunk: dark base materials (worn metals, matte \
                 plastics) with strong emissive accents in magenta, cyan, and \
                 yellow. Use glossy metals and high-contrast palette splits. \
                 Silhouettes are hard-edged and industrial."
            }
            Style::PixelArt => {
                "Retro pixel-art: chunky blocky primitives arranged as if they were \
                 upscaled pixels, with a deliberately limited 8-bit-style palette \
                 (≤ 16 colours). No smooth gradients, no anti-aliased edges, and no \
                 fine geometric detail."
            }
        }
    }

    /// Parse a slug back into a `Style`. Accepts the canonical [`Style::key`]
    /// form plus a few common aliases so hand-typed CLI invocations and
    /// hand-edited `.mog` files survive (`low-poly`, `playstation1`,
    /// `cel-shaded`, `pixel-art`, …). Case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ps1" | "playstation" | "playstation1" | "psx" => Some(Style::Ps1),
            "n64" | "nintendo64" | "nintendo_64" => Some(Style::N64),
            "low_poly" | "low-poly" | "lowpoly" => Some(Style::LowPoly),
            "high_detail" | "high-detail" | "highdetail" | "aaa" => Some(Style::HighDetail),
            "arcade" => Some(Style::Arcade),
            "voxel" | "minecraft" => Some(Style::Voxel),
            "cel_shaded" | "cel-shaded" | "celshaded" | "toon" => Some(Style::CelShaded),
            "stylized_fantasy" | "stylized-fantasy" | "stylizedfantasy" | "fantasy" => {
                Some(Style::StylizedFantasy)
            }
            "cyberpunk" => Some(Style::Cyberpunk),
            "pixel_art" | "pixel-art" | "pixelart" => Some(Style::PixelArt),
            _ => None,
        }
    }
}

/// Render the guidance block prepended to every styled user prompt.
/// Heading is `## Style` so it nests cleanly inside the model's existing
/// markdown-tolerant prompt expectations.
pub fn style_prompt_block(s: Style) -> String {
    format!(
        "## Style\n\n\
         {description}\n\n\
         Apply this style consistently to every primitive, material, and \
         prompt-derived texture you author. Material colours, segment \
         counts, and silhouette choices must all reflect the style above.\n\n",
        description = s.description(),
    )
}

/// Prepend the style guidance block to `user_prompt` when `style` is set.
/// `None` is a passthrough — the prompt is returned unchanged so existing
/// callers and goldens are bit-for-bit identical when no style is picked.
///
/// The block goes at the very top of the user prompt so it composes with
/// the existing modify/animate scaffolds (which append the "Existing
/// file:" trailer at the end of the prompt and need to remain the last
/// thing the model reads).
pub fn apply_style_to_prompt(user_prompt: &str, style: Option<Style>) -> String {
    match style {
        Some(s) => format!("{block}{user_prompt}", block = style_prompt_block(s)),
        None => user_prompt.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrip_for_every_style() {
        for s in STYLES {
            let parsed = Style::parse(s.key());
            assert_eq!(parsed, Some(*s), "key {} did not roundtrip", s.key());
        }
    }

    #[test]
    fn parse_accepts_aliases_and_case() {
        assert_eq!(Style::parse("PS1"), Some(Style::Ps1));
        assert_eq!(Style::parse("playstation1"), Some(Style::Ps1));
        assert_eq!(Style::parse("low-poly"), Some(Style::LowPoly));
        assert_eq!(Style::parse("LowPoly"), Some(Style::LowPoly));
        assert_eq!(Style::parse("cel-shaded"), Some(Style::CelShaded));
        assert_eq!(Style::parse("toon"), Some(Style::CelShaded));
        assert_eq!(Style::parse("pixel-art"), Some(Style::PixelArt));
        assert_eq!(Style::parse("minecraft"), Some(Style::Voxel));
    }

    #[test]
    fn parse_rejects_unknown() {
        assert_eq!(Style::parse(""), None);
        assert_eq!(Style::parse("noir"), None);
        assert_eq!(Style::parse("realistic"), None);
    }

    #[test]
    fn apply_passthrough_when_none() {
        let prompt = "a wooden chair";
        assert_eq!(apply_style_to_prompt(prompt, None), prompt);
    }

    #[test]
    fn apply_prepends_block_when_some() {
        let out = apply_style_to_prompt("a wooden chair", Some(Style::Ps1));
        assert!(out.starts_with("## Style"), "got {out:?}");
        assert!(out.contains("Original PlayStation"), "got {out:?}");
        assert!(out.trim_end().ends_with("a wooden chair"), "got {out:?}");
    }

    #[test]
    fn description_is_nonempty_for_every_style() {
        for s in STYLES {
            assert!(!s.description().is_empty(), "{} description empty", s.key());
            assert!(!s.label().is_empty(), "{} label empty", s.key());
        }
    }
}
