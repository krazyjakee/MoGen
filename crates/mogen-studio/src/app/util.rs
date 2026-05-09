//! Shared helpers used across the app module — split into focused submodules
//! and re-exported here so existing `super::util::*` callers keep working.
//!
//! - `paths` — CWD/workspace lookup and small path/string formatters.
//! - `visibility` — origin-based filtering for the right sidebar.
//! - `spans` — locate authored DSL spans for materials / clips.
//! - `textures` — gather/scan/delete texture files (and source-attr cleanup).
//! - `build` — the GLB write worker (`run_build`).
//! - `llm` — text DSL generate/modify/animate/repair worker (`run_llm`,
//!   `LlmRunConfig`, `pick_default_seed`, `build_provider_client`).
//! - `textures_run` — image-pipeline worker (`run_llm_textures`).
//! - `enhance` — prompt rewriter (`run_prompt_enhance`).

mod build;
mod enhance;
mod llm;
mod paths;
mod spans;
mod textures;
mod textures_run;
mod visibility;

pub(super) use build::run_build;
pub(super) use enhance::run_prompt_enhance;
pub(super) use llm::{
    build_provider_client, pick_default_seed, run_llm, run_llm_refine, Credential,
    LlmRunConfig,
};
pub(super) use paths::{
    ellipsize_path, format_inspector_scalar, locate_project_root, offset_to_line_col,
    resolve_for_check,
};
pub(super) use spans::{find_clip_source_span, find_material_source_span};
pub(super) use textures::{
    delete_material_textures, delete_texture_group, gather_texture_refs, scan_unused_textures,
};
pub(super) use textures_run::run_llm_textures;
pub(super) use visibility::{
    materials_referenced_by_visible_nodes, origin_in_visible_set, visible_origins,
};
