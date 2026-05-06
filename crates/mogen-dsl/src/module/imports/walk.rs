//! Recursive walkers that descend into `import "path.mog"` directives and
//! `use "@user/slug[@v]"` registry references. Each walker collects the
//! lifted module / material / scene declarations into a shared `out` vec
//! and rides on shared `visited` / `stack` state for dedup + cycle
//! detection. The actual per-file lifting work is in [`super::lift`];
//! these functions just orchestrate traversal and dispatch to the loader.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

use crate::ast::Node;

use super::super::loader::{parse_registry_spec, Loader, RegistrySpec};
use super::helpers::import_alias;
use super::lift::lift_loaded_into;

pub(super) fn resolve_imports_into(
    ast: &[Node],
    base_dir: Option<&Path>,
    loader: &mut dyn Loader,
    visited: &mut HashSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
    out: &mut Vec<Node>,
    module_names: &mut HashMap<String, PathBuf>,
    material_names: &mut HashMap<String, PathBuf>,
) -> Result<()> {
    for n in ast {
        if n.kind != "import" {
            continue;
        }
        let raw = n.name.as_deref().ok_or_else(|| {
            anyhow!("`import` requires a quoted file path, e.g. `import \"shared.mog\"`")
        })?;
        let alias = import_alias(n)?;
        let loaded = loader.load(raw, base_dir)?;
        if stack.iter().any(|p| p == &loaded.canonical) {
            let chain: Vec<String> = stack
                .iter()
                .chain(std::iter::once(&loaded.canonical))
                .map(|p| p.display().to_string())
                .collect();
            bail!("recursive import: {}", chain.join(" -> "));
        }
        if !visited.insert(loaded.canonical.clone()) {
            // Already loaded by a prior import — skip.
            continue;
        }
        lift_loaded_into(
            loaded,
            alias,
            raw,
            loader,
            visited,
            stack,
            out,
            module_names,
            material_names,
        )?;
    }
    Ok(())
}

/// Walk `ast` recursively for `use "@user/slug[@v]"` registry references,
/// resolve each via [`Loader::load_registry`], parse the returned source,
/// and lift its contents the same way an imported file is lifted — except
/// the synthesised module name is the registry token itself
/// (`@alice/chairs@2`), so `expand_modules` finds it when the user's
/// `use "@alice/chairs@2"` runs.
///
/// Cycle detection rides on the same `stack` as file imports: registry
/// loaders return synthetic but stable `LoadedFile::canonical` values
/// (e.g. `registry/alice/chairs/2/main.mog`) so a chain like
/// `@a/x@1 -> @b/y@1 -> @a/x@1` surfaces here just like a recursive
/// `import` chain does.
pub(super) fn resolve_registry_uses_into(
    ast: &[Node],
    loader: &mut dyn Loader,
    visited: &mut HashSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
    out: &mut Vec<Node>,
    module_names: &mut HashMap<String, PathBuf>,
    material_names: &mut HashMap<String, PathBuf>,
) -> Result<()> {
    // Collect every unique registry ref reachable from `ast`. Walking
    // first instead of fetching inline keeps fetch order independent of
    // AST traversal order, which makes diagnostics deterministic.
    let mut seen: HashSet<String> = HashSet::new();
    let mut refs: Vec<RegistrySpec> = Vec::new();
    collect_registry_refs(ast, &mut seen, &mut refs);

    for spec in refs {
        let loaded = loader.load_registry(&spec)?;
        if stack.iter().any(|p| p == &loaded.canonical) {
            let chain: Vec<String> = stack
                .iter()
                .chain(std::iter::once(&loaded.canonical))
                .map(|p| p.display().to_string())
                .collect();
            bail!("recursive registry use: {}", chain.join(" -> "));
        }
        if !visited.insert(loaded.canonical.clone()) {
            continue;
        }
        lift_loaded_into(
            loaded,
            Some(spec.raw.clone()),
            &spec.raw,
            loader,
            visited,
            stack,
            out,
            module_names,
            material_names,
        )?;
    }
    Ok(())
}

/// Recursive walk that finds every `use` whose name parses as a registry
/// reference. Dedupes by the verbatim raw token so two identical refs
/// resolve once.
fn collect_registry_refs(
    nodes: &[Node],
    seen: &mut HashSet<String>,
    out: &mut Vec<RegistrySpec>,
) {
    for n in nodes {
        if n.kind == "use" {
            if let Some(name) = &n.name {
                if let Some(spec) = parse_registry_spec(name) {
                    if seen.insert(spec.raw.clone()) {
                        out.push(spec);
                    }
                }
            }
        }
        collect_registry_refs(&n.children, seen, out);
    }
}
