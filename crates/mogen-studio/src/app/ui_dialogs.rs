//! Modal dialog panels for `MogenStudioApp`.
//!
//! Each submodule defines one or more `impl MogenStudioApp` methods grouped
//! by which modal they paint:
//! - `prefs` — Options window (LLM / Appearance / Privacy tabs) + `PrefsTab`
//! - `quit` — unsaved-changes confirmations on quit / tab-close
//! - `about` — Help → About
//! - `ask` — Ask MoGen modal
//! - `new_prompt` — New from Prompt (text + optional reference image)
//! - `external` — on-disk-conflict resolver
//! - `export` — Build GLB
//! - `video` — Render MP4 options + capture-progress scrim

mod about;
mod ask;
mod export;
mod external;
mod new_prompt;
mod prefs;
mod quit;
mod update;
mod video;

pub use prefs::PrefsTab;
