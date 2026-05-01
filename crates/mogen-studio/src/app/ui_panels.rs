//! Inspector / editor / viewport-overlay panels for `MogenStudioApp`.
//!
//! Each submodule defines one or more `impl MogenStudioApp` methods grouped
//! by what they paint:
//! - `editor` — code editor TextEdit + gutter + autocomplete glue
//! - `diagnostics` — validator footer
//! - `selected` — single-node inspector (transform / light / delete / dup)
//! - `summary` — scene-level stats + LOD slider
//! - `materials` — per-material editor and unused-texture cleanup
//! - `animation` — clip playback + scrub
//! - `overlay` — floating viewport toolbar

mod animation;
mod diagnostics;
mod editor;
mod materials;
mod overlay;
mod selected;
mod summary;
