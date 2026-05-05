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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use mogen_core::Span;

use crate::ast::{Node, Value};
use crate::parser::parse;

use super::loader::{parse_registry_spec, FsLoader, LoadedFile, Loader, RegistrySpec};

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
    resolve_imports_into(
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
    resolve_registry_uses_into(
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

fn resolve_imports_into(
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
fn resolve_registry_uses_into(
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

/// Shared lift logic: parse `loaded.source`, recursively chase its
/// `import` and registry-`use` refs, then hoist the loaded source's
/// `module` / `material` / `scene` / animation declarations into `out`
/// the same way `resolve_imports_into` does for an imported file. Used
/// for both file imports (`explicit_module_name` = the `(as=…)` alias or
/// `None` to default to the file stem) and registry refs
/// (`explicit_module_name` = the `@user/slug[@v]` token).
fn lift_loaded_into(
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

/// Walk transitive `import "path.mog"` directives starting from `entry_source`
/// and return every reachable sibling `.mog` file as `(filename, source)` pairs.
/// Used by the publisher to bundle a scene with its local imports into a
/// single multi-file `PublishRequest`. Registry uses (`use "@user/slug[@v]"`)
/// are external dependencies and intentionally skipped — those resolve through
/// `mog.lock` on the consumer side, not as bundled bytes.
///
/// Each filename is the imported file's path relative to `entry_dir`, with
/// platform-native separators normalised to forward slashes so the same
/// filename round-trips through the moghub server (which stores
/// `model_files.filename` as a string and joins it back on the consumer side).
///
/// Errors:
/// - the entry source fails to parse;
/// - an `import` resolves to a path outside `entry_dir` (publishers can't
///   reach into a parent directory — the user must move the file or flatten
///   the layout);
/// - any imported file fails to load or parse.
///
/// `entry_dir` is canonicalised before traversal so symlink hops and
/// `..`/`.` segments resolve consistently with the cycle-detection used by
/// [`resolve_imports`].
pub fn collect_local_import_files(
    entry_dir: &Path,
    entry_source: &str,
) -> Result<Vec<(String, String)>> {
    let entry_dir_canonical = std::fs::canonicalize(entry_dir)
        .with_context(|| format!("canonicalising publish base dir {}", entry_dir.display()))?;
    let mut loader = FsLoader::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut out: Vec<(String, String)> = Vec::new();
    collect_local_imports_into(
        entry_source,
        Some(entry_dir_canonical.as_path()),
        &entry_dir_canonical,
        &mut loader,
        &mut visited,
        &mut out,
    )?;
    Ok(out)
}

fn collect_local_imports_into(
    source: &str,
    base_dir: Option<&Path>,
    entry_dir_canonical: &Path,
    loader: &mut FsLoader,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<(String, String)>,
) -> Result<()> {
    let ast = parse(source).map_err(|e| anyhow!("parsing source for publish bundling: {e}"))?;
    for n in &ast {
        if n.kind != "import" {
            continue;
        }
        let raw = n.name.as_deref().ok_or_else(|| {
            anyhow!("`import` requires a quoted file path, e.g. `import \"shared.mog\"`")
        })?;
        let loaded = loader.load(raw, base_dir)?;
        if !visited.insert(loaded.canonical.clone()) {
            continue;
        }
        let rel = loaded
            .canonical
            .strip_prefix(entry_dir_canonical)
            .map_err(|_| {
                anyhow!(
                    "import \"{raw}\" resolves to {} which is outside the entry's directory \
                     {} — publish-bundling can't reach into a parent dir; move the file \
                     beside the entry or flatten the layout",
                    loaded.canonical.display(),
                    entry_dir_canonical.display()
                )
            })?;
        let filename = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let inner_dir = loaded.canonical.parent().map(|p| p.to_path_buf());
        out.push((filename, loaded.source.clone()));
        collect_local_imports_into(
            &loaded.source,
            inner_dir.as_deref(),
            entry_dir_canonical,
            loader,
            visited,
            out,
        )?;
    }
    Ok(())
}

/// Read the optional `as=<ident>` attribute on an `import` node. Returns the
/// alias string when present, `None` when no alias was supplied. Any other
/// attribute on `import` is an error — keeps the surface narrow.
fn import_alias(n: &Node) -> Result<Option<String>> {
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
fn module_name_from_path(p: &Path) -> Option<String> {
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
fn set_origin_recursive(node: &mut Node, origin: &Path) {
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
fn rewrite_texture_paths(node: &mut Node, base: Option<&Path>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    /// Per-test scratch directory under `std::env::temp_dir()`. Cleans up
    /// on Drop so successive tests don't interfere. The directory name
    /// embeds the test name and a process-unique counter.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mogen-dsl-imports-{}-{}-{}",
                std::process::id(),
                id,
                label
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let p = self.path.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("create parent");
            }
            std::fs::write(&p, contents).expect("write tmp file");
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn import_loads_modules_from_other_file() {
        let tmp = TempDir::new("loads");
        tmp.write(
            "lib.mog",
            r#"module "leg" (h=0.5) { cylinder "leg" (height=$h, radius=0.05) }"#,
        );
        let main_src = r#"
            import "lib.mog"
            scene { use "leg" (h=0.9) }
        "#;
        let ast = parse(main_src).unwrap();
        let imported = resolve_imports(&ast, Some(tmp.path.as_path())).unwrap();
        let imported_reg = super::super::collect_modules(&imported).unwrap();
        assert!(imported_reg.contains("leg"), "imported module not registered");
        // Full pipeline: lower with source dir set should expand the use.
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        let leg = scene
            .nodes
            .iter()
            .find(|n| n.name == "leg")
            .expect("expanded leg node");
        assert!(leg.mesh.is_some());
    }

    #[test]
    fn imports_dedupe_by_canonical_path() {
        let tmp = TempDir::new("dedupe");
        tmp.write(
            "shared.mog",
            r#"module "leg" (h=1.0) { cylinder "leg" (height=$h) }"#,
        );
        let main_src = r#"
            import "shared.mog"
            import "shared.mog"
            scene { use "leg" (h=2.0) }
        "#;
        let ast = parse(main_src).unwrap();
        // Importing the same file twice must not produce duplicate module decls.
        let imported = resolve_imports(&ast, Some(tmp.path.as_path())).unwrap();
        assert_eq!(imported.len(), 1, "duplicate imports should dedupe");
    }

    #[test]
    fn import_chain_resolves_transitive_modules() {
        let tmp = TempDir::new("chain");
        tmp.write(
            "leaf.mog",
            r#"module "leaflet" (s=0.1) { box "l" (size=[$s, $s, $s]) }"#,
        );
        tmp.write(
            "branch.mog",
            r#"
            import "leaf.mog"
            module "twig" (s=0.5) { use "leaflet" (s=$s) }
            "#,
        );
        let main_src = r#"
            import "branch.mog"
            scene { use "twig" (s=0.3) }
        "#;
        let ast = parse(main_src).unwrap();
        let imported = resolve_imports(&ast, Some(tmp.path.as_path())).unwrap();
        let names: Vec<_> = imported
            .iter()
            .filter_map(|n| n.name.clone())
            .collect();
        assert!(names.contains(&"twig".to_string()));
        assert!(names.contains(&"leaflet".to_string()));
    }

    #[test]
    fn import_cycle_is_rejected() {
        let tmp = TempDir::new("cycle");
        tmp.write("a.mog", r#"import "b.mog""#);
        tmp.write("b.mog", r#"import "a.mog""#);
        let main_src = r#"import "a.mog""#;
        let ast = parse(main_src).unwrap();
        let err = resolve_imports(&ast, Some(tmp.path.as_path()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("recursive import"), "got: {err}");
    }

    #[test]
    fn import_missing_file_errors_clearly() {
        let tmp = TempDir::new("missing");
        let main_src = r#"import "does_not_exist.mog""#;
        let ast = parse(main_src).unwrap();
        let err = resolve_imports(&ast, Some(tmp.path.as_path()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("does_not_exist.mog") && err.contains("could not open"),
            "got: {err}"
        );
    }

    #[test]
    fn imported_file_with_scene_synthesises_module_named_after_stem() {
        let tmp = TempDir::new("scene_module");
        tmp.write(
            "chair.mog",
            r#"
            scene { box "seat" (size=[1, 0.1, 1]) }
            "#,
        );
        let main_src = r#"
            import "chair.mog"
            scene { use "chair" () }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        assert!(
            scene.nodes.iter().any(|n| n.name == "seat"),
            "expected the chair's `seat` to land in the composed scene"
        );
    }

    #[test]
    fn imported_scene_and_explicit_modules_coexist() {
        let tmp = TempDir::new("scene_and_modules");
        tmp.write(
            "chair.mog",
            r#"
            module "leg" (h=0.5) { cylinder "leg" (height=$h, radius=0.05) }
            scene {
              box "seat" (size=[1, 0.1, 1])
              use "leg" (h=0.4)
            }
            "#,
        );
        let main_src = r#"
            import "chair.mog"
            scene { use "chair" () }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        assert!(scene.nodes.iter().any(|n| n.name == "seat"));
        assert!(scene.nodes.iter().any(|n| n.name == "leg"));
    }

    #[test]
    fn imported_top_level_material_is_visible_to_user_scene() {
        let tmp = TempDir::new("imported_material");
        tmp.write(
            "chair.mog",
            r#"
            material "wood" (color=[0.5, 0.3, 0.1])
            scene { box "seat" (size=[1, 0.1, 1], mat="wood") }
            "#,
        );
        let main_src = r#"
            import "chair.mog"
            scene {
              use "chair" ()
              cylinder "post" (radius=0.05, height=1, mat="wood")
            }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        assert!(
            scene.materials.iter().any(|m| m.name == "wood"),
            "imported material should be registered on the composed scene"
        );
    }

    #[test]
    fn synthesised_module_collision_is_hard_error() {
        let tmp = TempDir::new("collision");
        tmp.write("a/chair.mog", r#"scene { box "a" (size=[1,1,1]) }"#);
        tmp.write("b/chair.mog", r#"scene { box "b" (size=[1,1,1]) }"#);
        let main_src = r#"
            import "a/chair.mog"
            import "b/chair.mog"
        "#;
        let ast = parse(main_src).unwrap();
        let err = resolve_imports(&ast, Some(tmp.path.as_path()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("collides") && err.contains("chair"),
            "got: {err}"
        );
    }

    #[test]
    fn import_as_renames_synthesised_module() {
        let tmp = TempDir::new("import_as");
        tmp.write("a/chair.mog", r#"scene { box "a" (size=[1,1,1]) }"#);
        tmp.write("b/chair.mog", r#"scene { box "b" (size=[1,1,1]) }"#);
        let main_src = r#"
            import "a/chair.mog" (as=chair_a)
            import "b/chair.mog" (as=chair_b)
            scene { use "chair_a" () use "chair_b" () }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        assert!(scene.nodes.iter().any(|n| n.name == "a"));
        assert!(scene.nodes.iter().any(|n| n.name == "b"));
    }

    #[test]
    fn imported_material_collision_binds_per_origin() {
        // Two imports declare a `wood` material with different colours. With
        // origin-scoped lookup, each import's geometry binds to its own
        // `wood` — the first-wins race that used to apply globally is gone.
        let tmp = TempDir::new("mat_collision");
        tmp.write(
            "a.mog",
            r#"material "wood" (color=[0.1, 0.1, 0.1])
               scene { box "a" (size=[1,1,1], mat="wood") }"#,
        );
        tmp.write(
            "b.mog",
            r#"material "wood" (color=[0.9, 0.9, 0.9])
               scene { box "b" (size=[1,1,1], mat="wood") }"#,
        );
        let main_src = r#"
            import "a.mog"
            import "b.mog"
            scene { use "a" () use "b" () }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        // Both `wood`s should be registered (one per origin), and each box
        // should bind to its own file's version.
        let woods: Vec<_> = scene.materials.iter().filter(|m| m.name == "wood").collect();
        assert_eq!(woods.len(), 2, "expected one wood per origin: {woods:?}");
        let box_a = scene.nodes.iter().find(|n| n.name == "a").expect("box a");
        let box_b = scene.nodes.iter().find(|n| n.name == "b").expect("box b");
        let mat_a = &scene.materials[box_a.material.unwrap().0 as usize];
        let mat_b = &scene.materials[box_b.material.unwrap().0 as usize];
        assert!((mat_a.base_color[0] - 0.1).abs() < 1e-6, "a should bind a.mog wood, got {mat_a:?}");
        assert!((mat_b.base_color[0] - 0.9).abs() < 1e-6, "b should bind b.mog wood, got {mat_b:?}");
    }

    #[test]
    fn imported_material_textures_survive_user_redeclaration() {
        // Regression for the photo_frame scenario: scene.mog declared a
        // plain `wall_mat` that shadowed photo_frame.mog's textured one,
        // silently stripping the photo frame's textures. Origin-scoped
        // lookup makes each file see its own materials first.
        let tmp = TempDir::new("user_redecl_textures");
        tmp.write(
            "frame.mog",
            r#"material "wall_mat" (color=[0.9, 0.9, 0.9],
                                    base_color_texture="textures/wall_albedo.png")
               scene { box "frame_wall" (size=[1,1,1], mat="wall_mat") }"#,
        );
        let main_src = r#"
            import "frame.mog"
            material "wall_mat" (color=[0.5, 0.5, 0.5])
            scene {
              box "user_wall" (size=[1,1,1], mat="wall_mat")
              use "frame" ()
            }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        let user_wall = scene.nodes.iter().find(|n| n.name == "user_wall").expect("user_wall");
        let frame_wall = scene.nodes.iter().find(|n| n.name == "frame_wall").expect("frame_wall");
        let user_mat = &scene.materials[user_wall.material.unwrap().0 as usize];
        let frame_mat = &scene.materials[frame_wall.material.unwrap().0 as usize];
        assert!(
            user_mat.base_color_texture.is_none(),
            "user-side wall_mat should be the plain user-declared one, got {user_mat:?}"
        );
        assert!(
            frame_mat.base_color_texture.is_some(),
            "frame-side wall_mat must keep its textures, got {frame_mat:?}"
        );
    }

    #[test]
    fn user_material_shadows_imported_material() {
        let tmp = TempDir::new("user_shadow_mat");
        tmp.write(
            "a.mog",
            r#"material "wood" (color=[0.1, 0.1, 0.1])
               scene { box "a" (size=[1,1,1], mat="wood") }"#,
        );
        let main_src = r#"
            import "a.mog"
            material "wood" (color=[0.9, 0.5, 0.2])
            scene { use "a" () }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        // User-declared material registers before imported ones, so its colour
        // wins.
        let mat_id = scene.find_material("wood").expect("wood should resolve");
        let wood = &scene.materials[mat_id.0 as usize];
        assert!((wood.base_color[0] - 0.9).abs() < 1e-6, "got {wood:?}");
    }

    #[test]
    fn imported_file_lod_scale_applies_to_its_own_geometry() {
        // Regression: the importing file's `lod_scale` (or default 1.0)
        // used to apply to imported geometry because `lift_loaded_into`
        // dropped imported `lod_scale` directives. Now each imported file
        // honours its own top-level `lod_scale` while the importing
        // file's geometry continues to use the importing file's setting.
        let tmp = TempDir::new("import_lod");
        tmp.write(
            "low.mog",
            r#"
            lod_scale (value=0.5)
            scene { sphere "s_imported" (radius=0.5) }
            "#,
        );
        let main_src = r#"
            import "low.mog"
            scene {
              use "low" ()
              sphere "s_main" (radius=0.5)
            }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        let imported = scene
            .nodes
            .iter()
            .find(|n| n.name == "s_imported")
            .expect("imported sphere");
        let main = scene
            .nodes
            .iter()
            .find(|n| n.name == "s_main")
            .expect("main sphere");
        let imported_verts = imported.mesh.as_ref().unwrap().positions.len();
        let main_verts = main.mesh.as_ref().unwrap().positions.len();
        assert!(
            imported_verts < main_verts,
            "imported file's `lod_scale (value=0.5)` should reduce its vert count \
             below the main scene's default (imported={imported_verts}, main={main_verts})"
        );
    }

    #[test]
    fn importing_file_lod_does_not_leak_into_imports() {
        // Symmetric guarantee: a generous `lod_scale` in the user's file
        // must not balloon the vert count of imported geometry. Each file
        // uses its own setting.
        let tmp = TempDir::new("import_lod_isolation");
        tmp.write(
            "default.mog",
            r#"scene { sphere "s_imported" (radius=0.5) }"#,
        );
        let main_src = r#"
            lod_scale (value=2)
            import "default.mog"
            scene {
              use "default" ()
              sphere "s_main" (radius=0.5)
            }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        // Baseline: a default-LOD sphere in a standalone scene.
        let baseline_ast = parse(r#"scene { sphere "s" (radius=0.5) }"#).unwrap();
        let baseline = crate::lower::lower(&baseline_ast).unwrap();
        let baseline_verts = baseline
            .nodes
            .iter()
            .find(|n| n.name == "s")
            .unwrap()
            .mesh
            .as_ref()
            .unwrap()
            .positions
            .len();
        let imported_verts = scene
            .nodes
            .iter()
            .find(|n| n.name == "s_imported")
            .unwrap()
            .mesh
            .as_ref()
            .unwrap()
            .positions
            .len();
        let main_verts = scene
            .nodes
            .iter()
            .find(|n| n.name == "s_main")
            .unwrap()
            .mesh
            .as_ref()
            .unwrap()
            .positions
            .len();
        assert_eq!(
            imported_verts, baseline_verts,
            "imported file with no `lod_scale` should keep default tessellation \
             (got {imported_verts}, expected default {baseline_verts}); the importing \
             file's `lod_scale=2` must not leak into imports"
        );
        assert!(
            main_verts > baseline_verts,
            "main scene with `lod_scale=2` should still upscale its own geometry \
             (got {main_verts}, baseline {baseline_verts})"
        );
    }

    #[test]
    fn imported_relative_texture_path_is_rooted_at_defining_file() {
        let tmp = TempDir::new("texture_rooting");
        tmp.write(
            "obj/chair.mog",
            r#"material "wood" (base_color_texture="textures/wood.png")
               scene { box "seat" (size=[1, 0.1, 1], mat="wood") }"#,
        );
        let main_src = r#"
            import "obj/chair.mog"
            scene { use "chair" () }
        "#;
        let ast = parse(main_src).unwrap();
        let imported = resolve_imports(&ast, Some(tmp.path.as_path())).unwrap();
        let mat = imported
            .iter()
            .find(|n| n.kind == "material" && n.name.as_deref() == Some("wood"))
            .expect("imported material should have been lifted");
        let path = match mat.attr("base_color_texture") {
            Some(Value::String(s) | Value::Ident(s)) => s.clone(),
            other => panic!("expected texture path string, got {other:?}"),
        };
        assert!(
            path.contains("/obj/textures/wood.png") || path.contains("\\obj\\textures\\wood.png"),
            "texture path should be rooted at the defining file's dir, got: {path}"
        );
        assert!(
            std::path::Path::new(&path).is_absolute(),
            "rewritten texture path should be absolute, got: {path}"
        );
    }

    #[test]
    fn imported_animation_only_fires_when_scene_is_used() {
        // Regression: animations declared at top level of an imported object
        // file used to lift to the importer's top level, where they would
        // resolve their `target=` against the composing scene even when the
        // user never `use`d the importing file's synthesised module. That
        // produced "track target X is neither a joint nor a scene node" for
        // any imported object that happened to ship an animation but wasn't
        // instantiated. Now the animations live inside the synthesised
        // module body and only fire when the corresponding `use` runs.
        let tmp = TempDir::new("anim_only_on_use");
        tmp.write(
            "toy.mog",
            r#"
            scene {
              group "pen1" (pos=[0, 0.1, 0]) { box "p" (size=[0.01, 0.1, 0.01]) }
            }
            clip "swing" (seconds=1.0) {
              track "pen1" (prop=rotation, axis=[0, 0, 1], keys=[[0, 0], [1, 30]])
            }
            "#,
        );
        // The composing scene imports `toy.mog` but never `use`s it. The clip
        // must NOT fire, otherwise it errors looking for `pen1`.
        let main_src = r#"
            import "toy.mog"
            scene { box "placeholder" (size=[1, 1, 1]) }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path()))
            .expect("compose without instantiating the import should succeed");
        assert!(scene.clips.is_empty(), "unused clip should not fire");
    }

    #[test]
    fn imported_animation_fires_when_scene_is_used() {
        // Pair of the previous test: when the user does `use "toy"`, the
        // imported clip travels into the composing scene alongside the
        // geometry it targets, and the recursive anim walker picks it up
        // from inside the wrapping `group`.
        let tmp = TempDir::new("anim_fires_on_use");
        tmp.write(
            "toy.mog",
            r#"
            scene {
              group "pen1" (pos=[0, 0.1, 0]) { box "p" (size=[0.01, 0.1, 0.01]) }
            }
            clip "swing" (seconds=1.0) {
              track "pen1" (prop=rotation, axis=[0, 0, 1], keys=[[0, 0], [1, 30]])
            }
            "#,
        );
        let main_src = r#"
            import "toy.mog"
            scene { group (pos=[0, 0, 0]) { use "toy" () } }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        assert_eq!(scene.clips.len(), 1, "imported clip should fire after use");
    }

    #[test]
    fn import_as_without_scene_block_is_rejected() {
        let tmp = TempDir::new("as_without_scene");
        tmp.write(
            "lib.mog",
            r#"module "leg" (h=0.5) { cylinder "leg" (height=$h) }"#,
        );
        let main_src = r#"import "lib.mog" (as=foo)"#;
        let ast = parse(main_src).unwrap();
        let err = resolve_imports(&ast, Some(tmp.path.as_path()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no top-level `scene`") && err.contains("foo"),
            "got: {err}"
        );
    }

    #[test]
    fn relative_import_without_source_dir_errors() {
        let main_src = r#"import "shared.mog""#;
        let ast = parse(main_src).unwrap();
        let err = resolve_imports(&ast, None).unwrap_err().to_string();
        assert!(err.contains("no source directory is set"), "got: {err}");
    }

    #[test]
    fn user_module_shadows_imported_module() {
        let tmp = TempDir::new("shadow");
        tmp.write(
            "lib.mog",
            r#"module "leg" (h=0.5) { cylinder "from_lib" (height=$h, radius=0.1) }"#,
        );
        let main_src = r#"
            import "lib.mog"
            module "leg" (h=0.5) { cylinder "from_user" (height=$h, radius=0.1) }
            scene { use "leg" (h=1.0) }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        // The user-declared module should win; the cylinder name proves it.
        assert!(
            scene.nodes.iter().any(|n| n.name == "from_user"),
            "user module should shadow imported module"
        );
        assert!(
            scene.nodes.iter().all(|n| n.name != "from_lib"),
            "imported module body should not appear when shadowed"
        );
    }

    #[test]
    fn imported_replicator_kinds_propagate_origin_to_lowered_node() {
        // Regression: replicator/stack/grid/branch/csg lowerers used to copy
        // `use_id` from the AST onto the SceneNode but skip `origin`. After
        // import, an imported `stack`/`grid`/etc. would land in the scene
        // with `origin = None` even though every other node from the same
        // file had `origin = Some(imported.mog)`. That breaks Studio's
        // viewport edits: a click on the import would resolve to the
        // origin-less wrapper, whose `source_span` is bytes inside the
        // imported file — applied to the active source it splices garbage
        // into an unrelated line. (Real-world reproduction: the office
        // assetpack's `printer.mog` ships a top-level `stack "printer_body"
        // (...)`. Dragging the printer in Studio inserted ` (pos=...)` into
        // a different `import` line.)
        let tmp = TempDir::new("origin_replicator");
        tmp.write(
            "obj.mog",
            r#"
            scene {
              stack "stk" (axis=y) {
                box "a" (size=[1, 0.5, 1])
                box "b" (size=[0.8, 0.4, 0.8])
              }
              array "arr" (count=3, step=[1, 0, 0]) {
                box "tile" (size=[0.5, 0.5, 0.5])
              }
              grid "g" (count=[2, 1, 1], step=[1, 0, 0]) {
                box "cell" (size=[0.4, 0.4, 0.4])
              }
              difference "diff" {
                box "outer" (size=[1, 1, 1])
                box "inner" (size=[0.6, 0.6, 0.6])
              }
            }
            "#,
        );
        let main_src = r#"
            import "obj.mog"
            scene { use "obj" (pos=[1, 2, 3]) }
        "#;
        let ast = parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.path.as_path())).unwrap();
        let imported_path = tmp.path.join("obj.mog").canonicalize().unwrap();
        for kind in &["stack", "array", "grid", "difference"] {
            let node = scene
                .nodes
                .iter()
                .find(|n| n.kind == *kind)
                .unwrap_or_else(|| panic!("expected an imported `{kind}` node in the scene"));
            assert_eq!(
                node.origin.as_deref(),
                Some(imported_path.as_path()),
                "imported `{kind}` lost its origin (got {:?}). The viewport editor relies on \
                 `origin` to distinguish active-source nodes from imported ones — without it, \
                 set_attr resolves the node's span against the wrong file.",
                node.origin,
            );
        }
    }

    #[test]
    fn collect_local_import_files_returns_sibling() {
        let tmp = TempDir::new("publish-sibling");
        tmp.write(
            "shared.mog",
            r#"module "leg" (h=0.5) { cylinder "leg" (height=$h, radius=0.05) }"#,
        );
        let main_src = r#"
            import "shared.mog"
            scene { use "leg" (h=0.9) }
        "#;
        let files = collect_local_import_files(tmp.path.as_path(), main_src).unwrap();
        assert_eq!(files.len(), 1, "one import expected");
        assert_eq!(files[0].0, "shared.mog");
        assert!(files[0].1.contains("module \"leg\""));
    }

    #[test]
    fn collect_local_import_files_walks_transitive() {
        let tmp = TempDir::new("publish-transitive");
        tmp.write(
            "leaf.mog",
            r#"module "leaflet" (s=0.1) { box "l" (size=[$s, $s, $s]) }"#,
        );
        tmp.write(
            "branch.mog",
            r#"
            import "leaf.mog"
            module "twig" (s=0.5) { use "leaflet" (s=$s) }
            "#,
        );
        let main_src = r#"
            import "branch.mog"
            scene { use "twig" (s=0.5) }
        "#;
        let files = collect_local_import_files(tmp.path.as_path(), main_src).unwrap();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"branch.mog"), "branch missing: {names:?}");
        assert!(names.contains(&"leaf.mog"), "leaf missing: {names:?}");
        assert_eq!(files.len(), 2, "expected exactly two bundled files");
    }

    #[test]
    fn collect_local_import_files_dedupes_diamond() {
        let tmp = TempDir::new("publish-diamond");
        tmp.write(
            "shared.mog",
            r#"module "leg" (h=1.0) { cylinder "leg" (height=$h) }"#,
        );
        tmp.write(
            "left.mog",
            r#"
            import "shared.mog"
            module "left" () { use "leg" (h=1.0) }
            "#,
        );
        tmp.write(
            "right.mog",
            r#"
            import "shared.mog"
            module "right" () { use "leg" (h=1.0) }
            "#,
        );
        let main_src = r#"
            import "left.mog"
            import "right.mog"
            scene { use "left" () }
        "#;
        let files = collect_local_import_files(tmp.path.as_path(), main_src).unwrap();
        let mut names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["left.mog", "right.mog", "shared.mog"]);
    }

    #[test]
    fn collect_local_import_files_skips_registry_uses() {
        let tmp = TempDir::new("publish-registry-uses");
        let main_src = r#"
            scene { use "@alice/chairs@2" () }
        "#;
        let files = collect_local_import_files(tmp.path.as_path(), main_src).unwrap();
        assert!(
            files.is_empty(),
            "registry use should not be bundled (it's resolved via mog.lock): {files:?}"
        );
    }

    #[test]
    fn collect_local_import_files_rejects_escape() {
        let parent = TempDir::new("publish-escape-parent");
        // Create the entry's *subdirectory* and place a sibling above it
        // (i.e. in `parent.path`). Importing `../sibling.mog` from within
        // the entry's dir reaches outside it — should error.
        let entry_dir = parent.path.join("entry");
        std::fs::create_dir_all(&entry_dir).unwrap();
        std::fs::write(
            parent.path.join("sibling.mog"),
            r#"module "x" () { box "x" (size=[1, 1, 1]) }"#,
        )
        .unwrap();
        let main_src = r#"
            import "../sibling.mog"
            scene { use "x" () }
        "#;
        let err =
            collect_local_import_files(&entry_dir, main_src).expect_err("escape should error");
        let msg = err.to_string();
        assert!(
            msg.contains("outside the entry's directory") || msg.contains("outside"),
            "expected escape-error message, got: {msg}"
        );
    }
}
