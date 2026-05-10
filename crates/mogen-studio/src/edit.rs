//! Span-aware `.mog` text mutations used by the viewport editor. Operates on
//! raw source strings and byte ranges (the `Span` already carried on every
//! `SceneNode`) so diagnostics and formatting survive untouched — a full AST
//! round-trip would normalise whitespace and lose the user's formatting.
//!
//! Public ops are split by concern:
//! - `attr` — `set_attr`, `delete_attr` (gizmo + inspector hot path)
//! - `node` — `delete_node`, `duplicate_node`
//! - `lod` — top-level `lod_scale (value=…)` accessors for the LOD slider
//! - `imports` — `import "<path>"` line insertion for the import dialog
//! - `insert` — top-level scene appends + import enumeration for the
//!   viewport context menu's Add submenus
//!
//! All ops return the full new source. `set_attr` is exercised heavily by
//! the tests; bugs in any of these cause silent DSL corruption.

mod attr;
mod imports;
mod insert;
mod internals;
mod lod;
mod node;

pub use attr::{delete_attr, get_attr, set_attr};
pub use imports::insert_imports;
pub use insert::{append_to_scene, list_imports, suggest_primitive_name};
pub use lod::{get_lod_scale, set_lod_scale};
pub use node::{dedup_contained_spans, delete_node, duplicate_node, wrap_node_in_group};
