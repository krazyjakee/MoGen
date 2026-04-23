//! `mogen textures` — walk a `.mog` AST, generate albedo PNGs for every
//! material via Gemini 2.5 Flash Image, derive the companion PBR maps
//! (normal / metallic-roughness / occlusion) locally via [`crate::pbr_maps`],
//! and splice the resulting `*_texture="…"` attrs into the source file.
//!
//! Splicing is a pure text edit driven by the parser's byte spans so we don't
//! lose formatting or comments. Prompt assembly and file naming live here too
//! because they're only useful in this command.

mod plan;
mod prompt;
mod run;
mod splice;

pub use plan::{
    build_plan, default_textures_dir, Plan, PlanAction, SlotKind, TexturesArgs,
    DEFAULT_TEXTURE_SIZE,
};
pub use prompt::{build_prompt, collect_materials, parse_prompt_header, MaterialHit};
pub use run::{
    generate_with_recitation_retry, maybe_cache, run_plan, TextureProgress, TextureStage,
};
pub use splice::{safe_filename_stem, splice_textures, Edit};
