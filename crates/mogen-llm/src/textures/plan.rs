use std::path::{Path, PathBuf};

use mogen_core::Span;
use mogen_dsl::ast::{Node, Value};

use super::prompt::{
    build_decal_prompt, build_prompt, collect_decals, collect_material_anatomy, collect_materials,
    is_mask_material,
};
use super::splice::safe_filename_stem;

/// Default `textures_dir` for a given input `.mog` — `textures/<stem>` so
/// sibling assets under one working directory don't collide on shared
/// material names (e.g. two assets each declaring `material "wood"`).
pub fn default_textures_dir(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .map(safe_filename_stem)
        .filter(|s| !s.is_empty());
    match stem {
        Some(s) => PathBuf::from("textures").join(s),
        None => PathBuf::from("textures"),
    }
}

/// Default cap on the longer side of every LLM-generated albedo, in pixels.
/// 512² is a 4× reduction from what Gemini 2.5 Flash Image returns (~1024²)
/// and keeps tileable PBR detail usable at normal camera distance; derived
/// normal / MR / AO maps inherit the size, so one downscale cascades.
pub const DEFAULT_TEXTURE_SIZE: u32 = 512;

/// Which texture slot a [`Plan`] targets. The attr name and filename suffix
/// are derived from this — callers never spell them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Albedo,
    Normal,
    MetallicRoughness,
    Occlusion,
}

impl SlotKind {
    pub fn attr(self) -> &'static str {
        match self {
            SlotKind::Albedo => "base_color_texture",
            SlotKind::Normal => "normal_texture",
            SlotKind::MetallicRoughness => "metallic_roughness_texture",
            SlotKind::Occlusion => "occlusion_texture",
        }
    }
    pub fn suffix(self) -> &'static str {
        match self {
            SlotKind::Albedo => "_albedo.png",
            SlotKind::Normal => "_normal.png",
            SlotKind::MetallicRoughness => "_metallicRoughness.png",
            SlotKind::Occlusion => "_ao.png",
        }
    }
    pub fn short_name(self) -> &'static str {
        match self {
            SlotKind::Albedo => "albedo",
            SlotKind::Normal => "normal",
            SlotKind::MetallicRoughness => "metalRough",
            SlotKind::Occlusion => "ao",
        }
    }
}

pub struct TexturesArgs {
    pub input: PathBuf,
    pub out: Option<PathBuf>,
    pub glb: Option<PathBuf>,
    pub textures_dir: PathBuf, // relative to `.mog`
    pub style: String,
    /// Image-model name. `None` means "pick the default for whatever Gemini
    /// credential ends up being used" — see [`crate::image::default_image_model_for`].
    /// `Some(name)` is what the user typed via `--model` and is used verbatim.
    pub model: Option<String>,
    pub force: bool,
    pub dry_run: bool,
    pub no_build: bool,
    pub api_key: Option<String>,
    /// Z.ai (`glm-image`) bearer key. When `Some`, the run dispatches to
    /// Z.ai's image API instead of Gemini. Falls back to the `ZAI_API_KEY`
    /// env var when unset; an explicit empty string is treated as unset.
    pub zai_api_key: Option<String>,
    /// Disable every derived PBR map. Albedo still gets generated.
    pub no_pbr: bool,
    pub no_normal: bool,
    pub no_metallic_roughness: bool,
    pub no_occlusion: bool,
    /// Cap on the longer side of generated albedos, in pixels. `0` disables
    /// the downscale and keeps whatever Gemini returned. Derived maps (normal
    /// / metallic-roughness / AO) inherit this size.
    pub texture_size: u32,
}

impl TexturesArgs {
    #[cfg(test)]
    pub fn with_defaults(input: PathBuf) -> Self {
        let textures_dir = default_textures_dir(&input);
        Self {
            input,
            out: None,
            glb: None,
            textures_dir,
            style: "photorealistic".to_string(),
            model: None,
            force: false,
            dry_run: false,
            no_build: false,
            api_key: None,
            zai_api_key: None,
            no_pbr: false,
            no_normal: false,
            no_metallic_roughness: false,
            no_occlusion: false,
            texture_size: DEFAULT_TEXTURE_SIZE,
        }
    }
}

/// One-line plan entry printed during dry-run and real runs alike. A single
/// material produces up to four plans (one per slot) so the reported table
/// mirrors exactly what run_plan is going to do.
pub struct Plan {
    pub material: String,
    pub span: Span,
    pub kind: SlotKind,
    pub action: PlanAction,
    pub rel_path: PathBuf,
    /// Non-empty only for `SlotKind::Albedo` — image prompt for the LLM call.
    pub prompt: String,
    /// For albedo, the relative path declared in the `.mog` when the slot was
    /// already textured. Lets [`super::run::run_plan`] read those bytes from
    /// disk so derived maps can still be produced without re-running the LLM.
    pub existing_albedo_path: Option<PathBuf>,
    /// True when the material is `alpha_mode="mask"`. Routes the albedo
    /// through the foliage-cutout post-process (chroma-key the pure-black
    /// backdrop into alpha=0) and preserves RGBA through resize / re-encode.
    pub is_mask: bool,
    /// True when this plan was synthesized from a `decal` node, not a
    /// `material` declaration. Decals request a transparent-background RGBA
    /// image directly (no chroma-key); the spliced attribute is `image=`,
    /// not `base_color_texture=`; and they never produce derived PBR maps.
    pub is_decal: bool,
}

pub enum PlanAction {
    /// Call the LLM (albedo only).
    Generate,
    /// Derive locally from the albedo PNG (PBR maps only).
    Derive,
    /// A PNG already exists at the planned `rel_path` on disk. Splice the
    /// `*_texture` attribute into the source but skip the API call / local
    /// derivation. Avoids burning API credit on a regenerate when the file
    /// is already there but the source attr just hasn't been spliced yet.
    UseExisting,
    /// Do nothing — either the attr is already present, or the user disabled
    /// this map kind via a flag.
    Skip(&'static str),
}

/// Build the plan without calling the API. Used by `--dry-run` and exposed
/// for testing.
pub fn build_plan(ast: &[Node], args: &TexturesArgs) -> Vec<Plan> {
    let hits = collect_materials(ast);
    let anatomy = collect_material_anatomy(ast);
    // On-disk existence checks resolve relative to the .mog directory, the
    // same base [`super::run::run_plan`] uses when reading/writing PNGs.
    let base_dir = args
        .input
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut plans = Vec::new();

    for h in hits {
        let stem = safe_filename_stem(&h.name);
        let is_mask = is_mask_material(h.node);

        // --- albedo slot ---
        let albedo_path = args.textures_dir.join(format!("{stem}{}", SlotKind::Albedo.suffix()));
        let existing_albedo = attr_path(h.node, SlotKind::Albedo.attr());
        // Attr-says-textured but the PNG is missing on disk: treat as
        // "needs Generate" so a stale `base_color_texture=` left over from
        // a failed prior run gets healed instead of silently no-op'd. The
        // existence check is relative to the .mog (same base run_plan
        // uses for I/O).
        let attr_albedo_exists_on_disk = existing_albedo
            .as_deref()
            .map(|p| base_dir.join(p).is_file())
            .unwrap_or(false);
        let albedo_attr_is_stale =
            existing_albedo.is_some() && !attr_albedo_exists_on_disk;
        let (albedo_action, albedo_prompt) = if existing_albedo.is_some()
            && !args.force
            && !albedo_attr_is_stale
        {
            (PlanAction::Skip("already has base_color_texture"), String::new())
        } else if !args.force && base_dir.join(&albedo_path).is_file() {
            (PlanAction::UseExisting, String::new())
        } else {
            let prompt = build_prompt(
                &h,
                &args.style,
                anatomy.get(&h.name).map(|s| s.as_str()),
            );
            (PlanAction::Generate, prompt)
        };
        plans.push(Plan {
            material: h.name.clone(),
            span: h.node.span,
            kind: SlotKind::Albedo,
            action: albedo_action,
            rel_path: albedo_path,
            prompt: albedo_prompt,
            existing_albedo_path: existing_albedo.clone(),
            is_mask,
            is_decal: false,
        });

        // --- derived maps ---
        // Mask-mode materials are alpha-cutout foliage / fins / fronds whose
        // albedo is a sparse cluster of opaque pixels on a transparent
        // background. The Sobel-from-luminance bake in `pbr_maps` reads each
        // needle/leaf edge as a steep cavity, producing speckled normal and
        // metallic-roughness maps that turn every cutout pixel into a tiny
        // specular highlight. Skip the derived bake for those materials and
        // let the authored scalars (roughness=1, metallic=0) carry the look.
        for (kind, disabled) in [
            (SlotKind::Normal, args.no_normal),
            (SlotKind::MetallicRoughness, args.no_metallic_roughness),
            (SlotKind::Occlusion, args.no_occlusion),
        ] {
            if args.no_pbr || disabled || is_mask {
                continue;
            }
            let rel_path = args.textures_dir.join(format!("{stem}{}", kind.suffix()));
            // Same stale-attr heal as albedo: if the .mog points at a PNG
            // that isn't on disk, fall through to Derive so the missing
            // map gets re-emitted instead of silently skipped.
            let attr_target = attr_path(h.node, kind.attr());
            let attr_target_on_disk = attr_target
                .as_deref()
                .map(|p| base_dir.join(p).is_file())
                .unwrap_or(false);
            let attr_is_stale = attr_target.is_some() && !attr_target_on_disk;
            let action = if attr_target.is_some() && !args.force && !attr_is_stale {
                PlanAction::Skip("already present")
            } else if !args.force && base_dir.join(&rel_path).is_file() {
                PlanAction::UseExisting
            } else {
                PlanAction::Derive
            };
            plans.push(Plan {
                material: h.name.clone(),
                span: h.node.span,
                kind,
                action,
                rel_path,
                prompt: String::new(),
                existing_albedo_path: None,
                is_mask,
                is_decal: false,
            });
        }
    }

    // Decals: one albedo plan each, no derived PBR maps. Decal images are
    // RGBA with a transparent background — they're never tileable surface
    // swatches, so derived normal/MR/AO would just produce noise.
    for d in collect_decals(ast) {
        let stem = safe_filename_stem(&d.name);
        let rel_path = args.textures_dir.join(format!("{stem}_decal.png"));
        let existing_image = attr_path(d.node, "image");
        let existing_on_disk = existing_image
            .as_deref()
            .map(|p| base_dir.join(p).is_file())
            .unwrap_or(false);
        let stale = existing_image.is_some() && !existing_on_disk;
        let (action, prompt) = if existing_image.is_some() && !args.force && !stale {
            (PlanAction::Skip("already has image"), String::new())
        } else if !args.force && base_dir.join(&rel_path).is_file() {
            (PlanAction::UseExisting, String::new())
        } else {
            (PlanAction::Generate, build_decal_prompt(&d, &args.style))
        };
        plans.push(Plan {
            material: format!("__decal_{}", d.name),
            span: d.node.span,
            kind: SlotKind::Albedo,
            action,
            rel_path,
            prompt,
            existing_albedo_path: existing_image,
            is_mask: false,
            is_decal: true,
        });
    }

    plans
}

pub(super) fn attr_path(node: &Node, key: &str) -> Option<PathBuf> {
    match node.attr(key)? {
        Value::String(s) | Value::Ident(s) => Some(PathBuf::from(s)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogen_dsl::parse;

    fn parse_or_panic(src: &str) -> Vec<Node> {
        parse(src).expect("parse")
    }

    #[test]
    fn build_plan_produces_slot_per_material_by_default() {
        let src = r#"material "a" (color=[1,0,0])"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(PathBuf::from("x.mog"));
        let plans = build_plan(&ast, &args);
        assert_eq!(plans.len(), 4);
        assert_eq!(plans[0].kind, SlotKind::Albedo);
        assert!(matches!(plans[0].action, PlanAction::Generate));
        assert!(matches!(plans[1].action, PlanAction::Derive));
        assert!(matches!(plans[2].action, PlanAction::Derive));
        assert!(matches!(plans[3].action, PlanAction::Derive));
    }

    #[test]
    fn build_plan_no_pbr_yields_only_albedo() {
        let src = r#"material "a" (color=[1,0,0])"#;
        let ast = parse_or_panic(src);
        let mut args = TexturesArgs::with_defaults(PathBuf::from("x.mog"));
        args.no_pbr = true;
        let plans = build_plan(&ast, &args);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].kind, SlotKind::Albedo);
    }

    #[test]
    fn build_plan_skips_already_textured_slots() {
        // Attr present AND the PNG is on disk at that path → Skip the API.
        // Plan resolves attr paths against the .mog's parent directory, so
        // place the PNG there before asserting Skip.
        let dir = fresh_tempdir("skip-textured");
        let mog = dir.join("x.mog");
        std::fs::write(&mog, "").unwrap();
        std::fs::write(dir.join("existing.png"), b"png").unwrap();

        let src = r#"material "a" (color=[1,0,0], base_color_texture="existing.png")"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(mog);
        let plans = build_plan(&ast, &args);
        // Albedo skipped but captures existing path for derivation.
        let albedo = &plans[0];
        assert!(matches!(albedo.action, PlanAction::Skip(_)));
        assert_eq!(
            albedo.existing_albedo_path.as_deref(),
            Some(std::path::Path::new("existing.png"))
        );
        // Derived slots are still scheduled.
        assert!(matches!(plans[1].action, PlanAction::Derive));
    }

    #[test]
    fn build_plan_heals_stale_attr_when_png_missing() {
        // `base_color_texture=` points at a PNG that doesn't exist on disk
        // (left over from a failed prior run). Plan should treat this as
        // "needs Generate" so the next run actually produces the missing
        // file instead of silently no-op'ing.
        let dir = fresh_tempdir("heal-stale");
        let mog = dir.join("x.mog");
        std::fs::write(&mog, "").unwrap();

        let src = r#"material "a" (color=[1,0,0], base_color_texture="textures/x/a_albedo.png", normal_texture="textures/x/a_normal.png")"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(mog);
        let plans = build_plan(&ast, &args);
        // Albedo: stale attr + no PNG on disk → Generate.
        assert!(
            matches!(plans[0].action, PlanAction::Generate),
            "stale base_color_texture attr should re-Generate, got {:?}",
            std::mem::discriminant(&plans[0].action),
        );
        // Normal slot: stale attr + no PNG → Derive (so it gets re-built
        // from the freshly-generated albedo).
        assert!(
            matches!(plans[1].action, PlanAction::Derive),
            "stale normal_texture attr should fall through to Derive, got {:?}",
            std::mem::discriminant(&plans[1].action),
        );
    }

    #[test]
    fn default_textures_dir_uses_mog_stem() {
        assert_eq!(
            default_textures_dir(Path::new("examples/furniture/chair.mog")),
            PathBuf::from("textures").join("chair"),
        );
        assert_eq!(
            default_textures_dir(Path::new("axe.mog")),
            PathBuf::from("textures").join("axe"),
        );
        // Non-ASCII / punctuation gets sanitized by safe_filename_stem.
        assert_eq!(
            default_textures_dir(Path::new("Wooden Crate.mog")),
            PathBuf::from("textures").join("wooden_crate"),
        );
        // No stem → bare "textures".
        assert_eq!(
            default_textures_dir(Path::new("")),
            PathBuf::from("textures"),
        );
    }

    #[test]
    fn build_plan_paths_land_under_default_subdir() {
        let src = r#"material "wood" (color=[1,0,0])"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(PathBuf::from("axe.mog"));
        let plans = build_plan(&ast, &args);
        assert_eq!(
            plans[0].rel_path,
            PathBuf::from("textures").join("axe").join("wood_albedo.png"),
        );
    }

    #[test]
    fn build_plan_force_retextures_existing() {
        let src = r#"material "a" (base_color_texture="old.png")"#;
        let ast = parse_or_panic(src);
        let mut args = TexturesArgs::with_defaults(PathBuf::from("x.mog"));
        args.force = true;
        let plans = build_plan(&ast, &args);
        assert!(matches!(plans[0].action, PlanAction::Generate));
    }

    fn fresh_tempdir(label: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("mogen-plan-{label}-{ns}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn build_plan_uses_existing_files_on_disk_without_force() {
        // Material has no `*_texture` attrs in the source, but every PNG it
        // would otherwise generate already lives at the planned rel_path on
        // disk. Without --force, plans should reflect that and skip the API.
        let dir = fresh_tempdir("use-existing");
        let mog = dir.join("x.mog");
        std::fs::write(&mog, "").unwrap();
        // Pre-create every PNG `build_plan` would target.
        let tex_dir = dir.join("textures").join("x");
        std::fs::create_dir_all(&tex_dir).unwrap();
        for suffix in [
            SlotKind::Albedo.suffix(),
            SlotKind::Normal.suffix(),
            SlotKind::MetallicRoughness.suffix(),
            SlotKind::Occlusion.suffix(),
        ] {
            std::fs::write(tex_dir.join(format!("a{suffix}")), b"png").unwrap();
        }

        let src = r#"material "a" (color=[1,0,0])"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(mog);
        let plans = build_plan(&ast, &args);
        assert_eq!(plans.len(), 4);
        for p in &plans {
            assert!(
                matches!(p.action, PlanAction::UseExisting),
                "{:?} slot expected UseExisting, got {:?}",
                p.kind,
                std::mem::discriminant(&p.action),
            );
        }
    }

    #[test]
    fn build_plan_force_overrides_on_disk_existence() {
        // Same setup as above but with --force: the on-disk PNGs should be
        // ignored and the plan should call Generate / Derive again.
        let dir = fresh_tempdir("force-overrides-disk");
        let mog = dir.join("x.mog");
        std::fs::write(&mog, "").unwrap();
        let tex_dir = dir.join("textures").join("x");
        std::fs::create_dir_all(&tex_dir).unwrap();
        std::fs::write(tex_dir.join("a_albedo.png"), b"png").unwrap();

        let src = r#"material "a" (color=[1,0,0])"#;
        let ast = parse_or_panic(src);
        let mut args = TexturesArgs::with_defaults(mog);
        args.force = true;
        let plans = build_plan(&ast, &args);
        assert!(matches!(plans[0].action, PlanAction::Generate));
    }

    #[test]
    fn build_plan_partial_disk_coverage_mixes_actions() {
        // Albedo PNG exists but derived maps don't: albedo should be
        // UseExisting (no API call), derived should still Derive from it.
        let dir = fresh_tempdir("partial-disk");
        let mog = dir.join("x.mog");
        std::fs::write(&mog, "").unwrap();
        let tex_dir = dir.join("textures").join("x");
        std::fs::create_dir_all(&tex_dir).unwrap();
        std::fs::write(tex_dir.join("a_albedo.png"), b"png").unwrap();

        let src = r#"material "a" (color=[1,0,0])"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(mog);
        let plans = build_plan(&ast, &args);
        assert!(matches!(plans[0].action, PlanAction::UseExisting));
        assert!(matches!(plans[1].action, PlanAction::Derive));
        assert!(matches!(plans[2].action, PlanAction::Derive));
        assert!(matches!(plans[3].action, PlanAction::Derive));
    }

    #[test]
    fn mask_material_skips_derived_pbr_maps() {
        // Foliage / cutout materials should produce only an albedo plan;
        // derived normal / MR / AO bakes turn each leaf-edge into a speckle.
        let src = r#"material "leaf" (color=[0.2,0.6,0.2], alpha_mode="mask")"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(PathBuf::from("x.mog"));
        let plans = build_plan(&ast, &args);
        assert_eq!(plans.len(), 1, "mask material yields only albedo");
        assert_eq!(plans[0].kind, SlotKind::Albedo);
        assert!(plans[0].is_mask);
    }

    #[test]
    fn opaque_material_still_gets_derived_pbr_maps() {
        // Sanity-check the negative case so the mask gate doesn't drift into
        // suppressing every material.
        let src = r#"material "wood" (color=[0.5,0.3,0.1])"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(PathBuf::from("x.mog"));
        let plans = build_plan(&ast, &args);
        assert_eq!(plans.len(), 4);
        assert!(plans.iter().all(|p| !p.is_mask));
    }

    #[test]
    fn decal_yields_one_albedo_plan_with_is_decal() {
        // A standalone decal with a `prompt=` should produce exactly one
        // albedo plan and zero derived PBR plans (no normal/MR/AO maps for
        // transparent overlays).
        let src = r#"scene { decal "logo" (prompt="a tiny logo", size=[0.2,0.1]) }"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(PathBuf::from("x.mog"));
        let plans = build_plan(&ast, &args);
        assert_eq!(plans.len(), 1, "decal should yield exactly one plan");
        let p = &plans[0];
        assert!(p.is_decal, "is_decal flag should be set");
        assert!(!p.is_mask, "decals are never mask materials");
        assert_eq!(p.kind, SlotKind::Albedo);
        assert!(matches!(p.action, PlanAction::Generate));
        assert!(p.prompt.contains("a tiny logo"), "prompt= should appear in body");
        assert!(p.prompt.contains("DECAL"), "decal prompt has DECAL framing");
        assert_eq!(
            p.rel_path,
            PathBuf::from("textures").join("x").join("logo_decal.png"),
        );
    }

    #[test]
    fn decal_with_existing_image_attr_is_skipped() {
        // `image="…"` already on the decal AND the PNG exists on disk: no
        // API call, no splicing. (If the PNG were missing, plan now heals
        // by switching to Generate.)
        let dir = fresh_tempdir("decal-skip");
        let mog = dir.join("x.mog");
        std::fs::write(&mog, "").unwrap();
        let tex_dir = dir.join("textures").join("x");
        std::fs::create_dir_all(&tex_dir).unwrap();
        std::fs::write(tex_dir.join("logo_decal.png"), b"png").unwrap();

        let src = r#"scene { decal "logo" (image="textures/x/logo_decal.png", size=[0.2,0.1]) }"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(mog);
        let plans = build_plan(&ast, &args);
        assert_eq!(plans.len(), 1);
        assert!(matches!(plans[0].action, PlanAction::Skip(_)));
        assert!(plans[0].is_decal);
        assert_eq!(
            plans[0].existing_albedo_path.as_deref(),
            Some(std::path::Path::new("textures/x/logo_decal.png")),
        );
    }

    #[test]
    fn decal_uses_existing_png_on_disk() {
        // No image=, but the planned PNG already lives on disk → UseExisting.
        let dir = fresh_tempdir("decal-on-disk");
        let mog = dir.join("x.mog");
        std::fs::write(&mog, "").unwrap();
        let tex_dir = dir.join("textures").join("x");
        std::fs::create_dir_all(&tex_dir).unwrap();
        std::fs::write(tex_dir.join("logo_decal.png"), b"png").unwrap();

        let src = r#"scene { decal "logo" (prompt="ignored", size=[0.2,0.1]) }"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(mog);
        let plans = build_plan(&ast, &args);
        assert_eq!(plans.len(), 1);
        assert!(plans[0].is_decal);
        assert!(matches!(plans[0].action, PlanAction::UseExisting));
    }

    #[test]
    fn decal_falls_back_to_node_name_when_prompt_absent() {
        // No prompt= and no image=: the decal's name acts as the prompt.
        let src = r#"scene { decal "embroidered logo" (size=[0.2,0.1]) }"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(PathBuf::from("x.mog"));
        let plans = build_plan(&ast, &args);
        assert_eq!(plans.len(), 1);
        assert!(matches!(plans[0].action, PlanAction::Generate));
        assert!(
            plans[0].prompt.contains("embroidered logo"),
            "decal name should appear when prompt= is absent: {}",
            plans[0].prompt
        );
    }

    #[test]
    fn decal_plan_lives_alongside_material_plans() {
        // Materials still produce 4 plans each (1 albedo + 3 derived); a
        // decal sibling adds exactly 1 more — no derived maps for decals.
        let src = r#"
            material "wood" (color=[0.5,0.3,0.1])
            scene {
              box "shelf" (size=[1,0.05,1], mat="wood")
              decal "logo" (prompt="a logo", size=[0.2,0.1])
            }
        "#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(PathBuf::from("x.mog"));
        let plans = build_plan(&ast, &args);
        assert_eq!(plans.len(), 5, "4 material slots + 1 decal albedo");
        let decal = plans.iter().find(|p| p.is_decal).expect("decal plan");
        assert_eq!(decal.kind, SlotKind::Albedo);
        let material_plans: Vec<_> = plans.iter().filter(|p| !p.is_decal).collect();
        assert_eq!(material_plans.len(), 4);
    }
}
