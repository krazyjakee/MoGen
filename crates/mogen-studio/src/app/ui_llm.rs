//! Right-hand "LLM" inspector panel for `MogenStudioApp`.
//!
//! Each submodule defines one or more `impl MogenStudioApp` methods grouped
//! by what they paint:
//! - `main` — the panel scaffold (Modify / Animate / Repair / Textures)
//! - `thinking` — per-file reasoning-budget dropdown
//! - `progress` — bordered "in flight" card with timeline + cancel
//! - `enhance` — small ✨ Enhance button shown under prompt fields
//! - `error_banner` — classified failure banner with Retry / Settings
//! - `textures` — generated-texture thumbnail strip + per-thumb actions
//! - `session` — footer meter showing total tokens / estimated cost

mod enhance;
mod error_banner;
mod main;
mod progress;
mod session;
mod textures;
mod thinking;
