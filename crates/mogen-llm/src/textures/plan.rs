use std::path::{Path, PathBuf};

use mogen_core::Span;
use mogen_dsl::ast::{Node, Value};

#[cfg(test)]
use crate::image::DEFAULT_IMAGE_MODEL;
use crate::image_cache::ImageCache;
#[cfg(test)]
use crate::pbr_maps::PbrMapOptions;

use super::prompt::{build_prompt, collect_materials, parse_prompt_header};
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
    pub model: String,
    pub force: bool,
    pub dry_run: bool,
    pub no_build: bool,
    pub no_cache: bool,
    pub api_key: Option<String>,
    /// Disable every derived PBR map. Albedo still gets generated.
    pub no_pbr: bool,
    pub no_normal: bool,
    pub no_metallic_roughness: bool,
    pub no_occlusion: bool,
    pub normal_strength: f32,
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
            model: DEFAULT_IMAGE_MODEL.to_string(),
            force: false,
            dry_run: false,
            no_build: false,
            no_cache: false,
            api_key: None,
            no_pbr: false,
            no_normal: false,
            no_metallic_roughness: false,
            no_occlusion: false,
            normal_strength: PbrMapOptions::default().normal_strength,
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
}

pub enum PlanAction {
    /// Call the LLM (albedo only).
    Generate,
    /// Load a cached LLM PNG (albedo only).
    CacheHit,
    /// Derive locally from the albedo PNG (PBR maps only).
    Derive,
    /// Do nothing — either the attr is already present, or the user disabled
    /// this map kind via a flag.
    Skip(&'static str),
}

/// Build the plan without calling the API. Used by `--dry-run` and exposed
/// for testing.
pub fn build_plan(
    src: &str,
    ast: &[Node],
    args: &TexturesArgs,
    cache: Option<&ImageCache>,
) -> Vec<Plan> {
    let subject = parse_prompt_header(src);
    let hits = collect_materials(ast);
    let mut plans = Vec::new();

    for h in hits {
        let stem = safe_filename_stem(&h.name);

        // --- albedo slot ---
        let albedo_path = args.textures_dir.join(format!("{stem}{}", SlotKind::Albedo.suffix()));
        let existing_albedo = attr_path(h.node, SlotKind::Albedo.attr());
        let (albedo_action, albedo_prompt) = if existing_albedo.is_some() && !args.force {
            (PlanAction::Skip("already has base_color_texture"), String::new())
        } else {
            let prompt = build_prompt(&h, &args.style, subject.as_deref());
            let cached = cache
                .map(|c| c.lookup(&ImageCache::key(&args.model, &prompt)).is_some())
                .unwrap_or(false);
            let action = if cached {
                PlanAction::CacheHit
            } else {
                PlanAction::Generate
            };
            (action, prompt)
        };
        plans.push(Plan {
            material: h.name.clone(),
            span: h.node.span,
            kind: SlotKind::Albedo,
            action: albedo_action,
            rel_path: albedo_path,
            prompt: albedo_prompt,
            existing_albedo_path: existing_albedo.clone(),
        });

        // --- derived maps ---
        for (kind, disabled) in [
            (SlotKind::Normal, args.no_normal),
            (SlotKind::MetallicRoughness, args.no_metallic_roughness),
            (SlotKind::Occlusion, args.no_occlusion),
        ] {
            if args.no_pbr || disabled {
                continue;
            }
            let rel_path = args.textures_dir.join(format!("{stem}{}", kind.suffix()));
            let action = if attr_path(h.node, kind.attr()).is_some() && !args.force {
                PlanAction::Skip("already present")
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
            });
        }
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
        let plans = build_plan(src, &ast, &args, None);
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
        let plans = build_plan(src, &ast, &args, None);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].kind, SlotKind::Albedo);
    }

    #[test]
    fn build_plan_skips_already_textured_slots() {
        let src = r#"material "a" (color=[1,0,0], base_color_texture="existing.png")"#;
        let ast = parse_or_panic(src);
        let args = TexturesArgs::with_defaults(PathBuf::from("x.mog"));
        let plans = build_plan(src, &ast, &args, None);
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
    fn default_textures_dir_uses_mog_stem() {
        assert_eq!(
            default_textures_dir(Path::new("examples/chair.mog")),
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
        let plans = build_plan(src, &ast, &args, None);
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
        let plans = build_plan(src, &ast, &args, None);
        assert!(matches!(plans[0].action, PlanAction::Generate));
    }
}
