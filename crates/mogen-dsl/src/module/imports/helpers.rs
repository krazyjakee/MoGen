//! Pure node transforms used by the import lift pass: parsing the optional
//! `as=<ident>` attribute, deriving a module name from a path stem, stamping
//! `origin` recursively, and rewriting texture paths to absolute. None of
//! these touch the loader or recurse into other files — they're scoped to
//! one node tree at a time.

use std::path::Path;

use anyhow::{bail, Result};

use crate::ast::{Node, Value};

/// Read the optional `as=<ident>` attribute on an `import` node. Returns the
/// alias string when present, `None` when no alias was supplied. Any other
/// attribute on `import` is an error — keeps the surface narrow.
pub(super) fn import_alias(n: &Node) -> Result<Option<String>> {
    let mut alias: Option<String> = None;
    for (k, v) in &n.attrs {
        if k != "as" {
            bail!(
                "`import` accepts only `(as=<ident>)`; unknown attribute `{}`",
                k
            );
        }
        match v {
            Value::Ident(s) | Value::String(s) => alias = Some(s.clone()),
            _ => bail!("`import (as=…)` expects an identifier, e.g. `(as=chair)`"),
        }
    }
    Ok(alias)
}

/// Sanitize a path stem into a usable module identifier. The grammar allows
/// any quoted module name (`use "My Chair" ()`) so we keep most characters,
/// but reject empty stems.
pub(super) fn module_name_from_path(p: &Path) -> Option<String> {
    let stem = p.file_stem()?.to_string_lossy().to_string();
    if stem.is_empty() {
        None
    } else {
        Some(stem)
    }
}

/// Stamp `origin` onto `node` and every descendant. Called on every node
/// hoisted out of an imported file so that, after `expand_modules` clones
/// these nodes into the active scene, lowering can copy `origin` onto each
/// `SceneNode` / `Material` / `Clip` / `Skin`. Drives MoGen Studio's
/// per-import sidebar scoping. A node that already carries an `origin` —
/// e.g. one re-imported through a transitive chain — keeps its first
/// (deepest) source so collisions surface against the file that introduced
/// the conflict, not the intermediate one.
pub(super) fn set_origin_recursive(node: &mut Node, origin: &Path) {
    if node.origin.is_none() {
        node.origin = Some(origin.to_path_buf());
    }
    for c in &mut node.children {
        set_origin_recursive(c, origin);
    }
}

/// Rewrite every texture-path attribute on `node` (and its descendants) so
/// relative paths become absolute against `base`. Texture refs only appear on
/// `material` nodes, but we walk descendants anyway so a `material` nested
/// inside a synthesised module body is still resolved correctly.
pub(super) fn rewrite_texture_paths(node: &mut Node, base: Option<&Path>) {
    const KEYS: &[&str] = &[
        "base_color_texture",
        "metallic_roughness_texture",
        "normal_texture",
        "occlusion_texture",
        "emissive_texture",
    ];
    if node.kind == "material" {
        if let Some(base) = base {
            for (k, v) in &mut node.attrs {
                if !KEYS.contains(&k.as_str()) {
                    continue;
                }
                let path = match v {
                    Value::String(s) | Value::Ident(s) => s.clone(),
                    _ => continue,
                };
                let p = Path::new(&path);
                if p.is_absolute() {
                    continue;
                }
                let joined = base.join(p);
                *v = Value::String(joined.to_string_lossy().into_owned());
            }
        }
    }
    for c in &mut node.children {
        rewrite_texture_paths(c, base);
    }
}
