//! Resolve top-level `import "path.mog"` directives — load the referenced
//! files (via a [`Loader`]), lift their `module` and `material` declarations,
//! and synthesise a module from any `scene { … }` body so the importing file
//! can `use` it.
//!
//! The walker is loader-agnostic: the desktop CLI hands it an [`FsLoader`]
//! and reads from disk, while `mogen-wasm` plugs in a loader backed by an
//! in-memory file map plus a JS-supplied registry fetcher. Without this
//! split the two resolvers would drift, breaking the "validation lives where
//! the compiler does" promise in MoGHub's `PLAN.md`.
//!
//! Internally split across:
//! - [`walk`] — recursive walkers over `import` and `use "@…"` references.
//! - [`lift`] — per-file lift logic (the part that hoists declarations
//!   and synthesises a module from a `scene` block).
//! - [`helpers`] — pure node transforms (alias parsing, origin stamping,
//!   texture-path rewriting).
//! - [`publish`] — the publish-time bundler that gathers source for every
//!   transitively-imported sibling.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::ast::Node;

use super::loader::{FsLoader, Loader};

mod helpers;
mod lift;
mod publish;
mod walk;

#[cfg(test)]
mod tests;

pub use publish::collect_local_import_files;

/// Walk top-level `import "path.mog"` declarations, recursively load the
/// referenced files, and return the union of (a) every `module` declaration
/// they contain, (b) a synthesised `module` for each imported file that has a
/// top-level `scene { … }` body — named after the file stem, or after `(as=…)`
/// when supplied — and (c) every `material` declaration in the imported files,
/// with relative texture paths rewritten to absolute (rooted at the *defining*
/// file's directory) so each texture resolves regardless of where the
/// composing scene lives. The caller hands this slice to `collect_modules` to
/// register the modules and to `collect_materials` to register the materials.
///
/// Path resolution: relative paths are joined onto `base_dir` (typically the
/// importing file's parent directory); absolute paths are used as-is.
/// Canonical paths drive both deduplication (re-importing the same file is
/// a no-op) and cycle detection (`A imports B imports A` is a hard error).
///
/// Collisions between two imports — same synthesised module name, or same
/// material name — are hard errors. The user can shadow either by re-declaring
/// locally; user-declared modules and materials always win over imports.
pub fn resolve_imports(ast: &[Node], base_dir: Option<&Path>) -> Result<Vec<Node>> {
    let mut loader = FsLoader::new();
    resolve_imports_with_loader(ast, base_dir, &mut loader)
}

/// Like [`resolve_imports`] but with a caller-supplied [`Loader`]. The
/// in-process axum upload validator passes a registry-backed loader against
/// `model_file` rows; the wasm editor passes one backed by open tabs plus a
/// JS fetch callback. Desktop callers should prefer [`resolve_imports`].
pub fn resolve_imports_with_loader(
    ast: &[Node],
    base_dir: Option<&Path>,
    loader: &mut dyn Loader,
) -> Result<Vec<Node>> {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    let mut out: Vec<Node> = Vec::new();
    let mut module_names: HashMap<String, PathBuf> = HashMap::new();
    let mut material_names: HashMap<String, PathBuf> = HashMap::new();
    walk::resolve_imports_into(
        ast,
        base_dir,
        loader,
        &mut visited,
        &mut stack,
        &mut out,
        &mut module_names,
        &mut material_names,
    )?;
    Ok(out)
}

/// Resolve `use "@user/slug[@v]"` registry references. Walks the AST
/// recursively (registry refs can appear nested inside scene blocks),
/// dispatches each unique ref through [`Loader::load_registry`], and
/// returns the synthesised module declarations to merge into the
/// composing scene's registry. Kept separate from
/// [`resolve_imports_with_loader`] so callers that don't want to touch
/// the network — `mogen check`, `mogen-validate`, the wasm playground —
/// can use a default `FsLoader` without it erroring on `@`-prefixed
/// names.
pub fn resolve_registry_uses_with_loader(
    ast: &[Node],
    loader: &mut dyn Loader,
) -> Result<Vec<Node>> {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    let mut out: Vec<Node> = Vec::new();
    let mut module_names: HashMap<String, PathBuf> = HashMap::new();
    let mut material_names: HashMap<String, PathBuf> = HashMap::new();
    walk::resolve_registry_uses_into(
        ast,
        loader,
        &mut visited,
        &mut stack,
        &mut out,
        &mut module_names,
        &mut material_names,
    )?;
    Ok(out)
}
