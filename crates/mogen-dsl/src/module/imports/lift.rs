//! Per-file lift logic: parse one loaded file, recursively chase its own
//! `import` and registry-`use` references, and hoist its `module` /
//! `material` / `scene` / animation declarations into the composing
//! scene's pool. Used by both file imports and registry refs — the only
//! difference is what name the synthesised module gets.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use mogen_core::Span;

use crate::ast::Node;
use crate::parser::parse;

use super::super::loader::{LoadedFile, Loader};
use super::helpers::{module_name_from_path, rewrite_texture_paths, set_origin_recursive};
use super::walk::{resolve_imports_into, resolve_registry_uses_into};

/// Shared lift logic: parse `loaded.source`, recursively chase its
/// `import` and registry-`use` refs, then hoist the loaded source's
/// `module` / `material` / `scene` / animation declarations into `out`
/// the same way `resolve_imports_into` does for an imported file. Used
/// for both file imports (`explicit_module_name` = the `(as=…)` alias or
/// `None` to default to the file stem) and registry refs
/// (`explicit_module_name` = the `@user/slug[@v]` token).
pub(super) fn lift_loaded_into(
    loaded: LoadedFile,
    explicit_module_name: Option<String>,
    raw_for_errors: &str,
    loader: &mut dyn Loader,
    visited: &mut HashSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
    out: &mut Vec<Node>,
    module_names: &mut HashMap<String, PathBuf>,
    material_names: &mut HashMap<String, PathBuf>,
) -> Result<()> {
    let canonical = loaded.canonical;
    let inner_ast = parse(&loaded.source)
        .map_err(|e| anyhow!("parsing imported file {}: {}", canonical.display(), e))?;
    let inner_dir = canonical.parent().map(|p| p.to_path_buf());

    // Resolve transitive imports + registry refs first so deepest
    // dependencies land in `out` ahead of the file that pulled them in.
    // Both passes are run regardless of how `lift_loaded_into` was
    // entered: a registry-fetched source can `import` siblings, and an
    // imported file can `use "@user/slug"`. Cycle detection on the
    // shared `visited`/`stack` keeps both halves bounded.
    stack.push(canonical.clone());
    resolve_imports_into(
        &inner_ast,
        inner_dir.as_deref(),
        loader,
        visited,
        stack,
        out,
        module_names,
        material_names,
    )?;
    resolve_registry_uses_into(
        &inner_ast,
        loader,
        visited,
        stack,
        out,
        module_names,
        material_names,
    )?;
    stack.pop();

    let base_for_textures = inner_dir.as_deref();
    let mut scene_body: Vec<Node> = Vec::new();
    let mut scene_span: Option<Span> = None;
    let mut anim_decls: Vec<Node> = Vec::new();
    for inner_node in inner_ast {
        match inner_node.kind.as_str() {
            "import" => {}
            "module" => {
                let mut m = inner_node;
                rewrite_texture_paths(&mut m, base_for_textures);
                set_origin_recursive(&mut m, &canonical);
                let name = m
                    .name
                    .clone()
                    .ok_or_else(|| anyhow!("module declaration requires a name"))?;
                if let Some(prev) = module_names.get(&name) {
                    bail!(
                        "module \"{name}\" is declared in two imported files: {} and {}",
                        prev.display(),
                        canonical.display()
                    );
                }
                module_names.insert(name, canonical.clone());
                out.push(m);
            }
            "material" => {
                let mut mat = inner_node;
                rewrite_texture_paths(&mut mat, base_for_textures);
                set_origin_recursive(&mut mat, &canonical);
                if let Some(name) = mat.name.clone() {
                    material_names
                        .entry(name)
                        .or_insert_with(|| canonical.clone());
                }
                out.push(mat);
            }
            "scene" => {
                if scene_span.is_some() {
                    bail!(
                        "imported file {} declares more than one top-level `scene` block",
                        canonical.display()
                    );
                }
                scene_span = Some(inner_node.span);
                for c in inner_node.children {
                    if c.kind == "material" {
                        let mut mat = c;
                        rewrite_texture_paths(&mut mat, base_for_textures);
                        set_origin_recursive(&mut mat, &canonical);
                        if let Some(name) = mat.name.clone() {
                            material_names
                                .entry(name)
                                .or_insert_with(|| canonical.clone());
                        }
                        out.push(mat);
                    } else {
                        let mut child = c;
                        rewrite_texture_paths(&mut child, base_for_textures);
                        set_origin_recursive(&mut child, &canonical);
                        scene_body.push(child);
                    }
                }
            }
            "lod_scale" => {
                // Preserve the imported file's `lod_scale` so lowering can
                // honour it for that file's geometry. Stamp the origin so
                // `lower()` keys it onto the imported file's path; the lower
                // pass then matches it against each node's `origin`. Without
                // this, an imported file's `lod_scale (value=0.5)` would be
                // silently overridden by the importing file's setting (or by
                // the default 1.0).
                let mut lod = inner_node;
                set_origin_recursive(&mut lod, &canonical);
                out.push(lod);
            }
            "meta" => {
                // `meta` is per-file provenance (name, version, seed,
                // thinking budget, original prompt). It belongs to the
                // file that authored it; merging it into the composing
                // scene would clobber that scene's own meta. Drop it —
                // the importing file keeps its own meta block.
            }
            "joint" | "clip" | "track" | "skeleton" | "spin" | "open_close" | "wave" | "flap"
            | "idle" => {
                let mut anim = inner_node;
                rewrite_texture_paths(&mut anim, base_for_textures);
                set_origin_recursive(&mut anim, &canonical);
                anim_decls.push(anim);
            }
            _ => {
                bail!(
                    "imported file {} has top-level `{}` — only `module`, \
                     `material`, `scene`, `import`, and animation / \
                     skeleton declarations are supported in imports",
                    canonical.display(),
                    inner_node.kind
                );
            }
        }
    }
    if scene_span.is_none() && !anim_decls.is_empty() {
        bail!(
            "imported file {} has top-level animation/skeleton declarations \
             but no `scene` block — wrap the animated geometry in a scene \
             so the animations travel with it",
            canonical.display()
        );
    }
    scene_body.extend(anim_decls);
    if let Some(span) = scene_span {
        let module_name = explicit_module_name
            .clone()
            .or_else(|| module_name_from_path(&canonical))
            .ok_or_else(|| {
                anyhow!(
                    "import \"{}\" — could not derive a module name from the file stem; \
                     supply one with `(as=<ident>)`",
                    raw_for_errors
                )
            })?;
        if let Some(prev) = module_names.get(&module_name) {
            bail!(
                "import \"{}\" — synthesised module name \"{}\" collides with another \
                 module declared in {}; rename with `(as=<ident>)`",
                raw_for_errors,
                module_name,
                prev.display()
            );
        }
        module_names.insert(module_name.clone(), canonical.clone());
        out.push(Node {
            kind: "module".to_string(),
            name: Some(module_name),
            attrs: Vec::new(),
            children: scene_body,
            span,
            kind_span: span,
            use_id: None,
            origin: Some(canonical.clone()),
        });
    } else if let Some(alias) = explicit_module_name {
        bail!(
            "import \"{}\" specified `(as={})`, but the imported file has no \
             top-level `scene` block to alias",
            raw_for_errors,
            alias
        );
    }
    Ok(())
}
