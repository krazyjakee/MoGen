//! Resolve top-level `import "path.mog"` directives — load the referenced
//! files, lift their `module` and `material` declarations, and synthesise a
//! module from any `scene { … }` body so the importing file can `use` it.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use mogen_core::Span;

use crate::ast::{Node, Value};
use crate::parser::parse;

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
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    let mut out: Vec<Node> = Vec::new();
    let mut module_names: HashMap<String, PathBuf> = HashMap::new();
    let mut material_names: HashMap<String, PathBuf> = HashMap::new();
    resolve_imports_into(
        ast,
        base_dir,
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
        let resolved = resolve_import_path(raw, base_dir)?;
        let canonical = fs::canonicalize(&resolved).with_context(|| {
            format!("import \"{}\" — could not open {}", raw, resolved.display())
        })?;
        if stack.iter().any(|p| p == &canonical) {
            let chain: Vec<String> = stack
                .iter()
                .chain(std::iter::once(&canonical))
                .map(|p| p.display().to_string())
                .collect();
            bail!("recursive import: {}", chain.join(" -> "));
        }
        if !visited.insert(canonical.clone()) {
            // Already loaded by a prior import — skip.
            continue;
        }
        let src = fs::read_to_string(&canonical)
            .with_context(|| format!("reading imported file {}", canonical.display()))?;
        let inner_ast = parse(&src)
            .with_context(|| format!("parsing imported file {}", canonical.display()))?;
        let inner_dir = canonical.parent().map(|p| p.to_path_buf());

        // Resolve transitive imports first so the deepest dependencies land in
        // `out` ahead of the file that imported them.
        stack.push(canonical.clone());
        resolve_imports_into(
            &inner_ast,
            inner_dir.as_deref(),
            visited,
            stack,
            out,
            module_names,
            material_names,
        )?;
        stack.pop();

        // Now lift this file's own contributions: modules, the implicit
        // scene-as-module (if any), and materials. Texture paths are rewritten
        // to absolute against `inner_dir` so they survive composition into a
        // scene that lives in a different directory.
        let base_for_textures = inner_dir.as_deref();
        let mut scene_body: Vec<Node> = Vec::new();
        let mut scene_span: Option<Span> = None;
        // Animation / skeleton declarations buffer until we know whether the
        // file has a scene block. They get appended into the synthesised
        // module body so they only fire when the user `use`s the object —
        // lifting them to top-level instead would orphan them whenever the
        // composing scene imports an object but doesn't instantiate it.
        let mut anim_decls: Vec<Node> = Vec::new();
        for inner_node in inner_ast {
            match inner_node.kind.as_str() {
                "import" => {} // already handled above
                "module" => {
                    let mut m = inner_node;
                    rewrite_texture_paths(&mut m, base_for_textures);
                    set_origin_recursive(&mut m, &canonical);
                    let name = m.name.clone().ok_or_else(|| {
                        anyhow!("module declaration requires a name")
                    })?;
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
                    // Cross-file material name duplicates aren't fatal:
                    // `find_material` returns the first match by index, and
                    // user-declared materials register before imported ones,
                    // so the user's definition (or the first import) wins.
                    // Collisions are tracked just so the importing file can
                    // surface a diagnostic if it cares.
                    if let Some(name) = mat.name.clone() {
                        material_names.entry(name).or_insert_with(|| canonical.clone());
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
                            // Hoist scene-nested materials to top level too —
                            // `collect_materials` only looks at depth ≤ 1, so a
                            // material left inside the synthesised module body
                            // would be invisible after `use`.
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
                    // `lod_scale` is a per-file build setting (it scales
                    // primitive segment counts during lowering). Lifting an
                    // imported file's setting into the composing scene would
                    // silently change every primitive's tessellation, which
                    // the user almost never wants. Drop it; the imported
                    // geometry was already tessellated against the import's
                    // own setting, and the composing scene's setting governs
                    // anything authored locally.
                }
                "joint" | "clip" | "track" | "skeleton" | "spin" | "open_close"
                | "wave" | "flap" | "idle" => {
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
            // Animations need a scene to attach to. Without one we can't tell
            // whether the user meant them to fire globally or to belong to a
            // particular module; rather than guess, ask them to wrap the
            // animated geometry in `scene { … }`.
            bail!(
                "imported file {} has top-level animation/skeleton declarations \
                 but no `scene` block — wrap the animated geometry in a scene \
                 so the animations travel with it",
                canonical.display()
            );
        }
        // Animations live inside the synthesised module body so a `use
        // "<stem>"` instantiation expands them into the composing scene
        // alongside the geometry they target. An imported file whose
        // scene-as-module is never invoked therefore contributes neither
        // geometry nor orphan animation tracks.
        scene_body.extend(anim_decls);
        if let Some(span) = scene_span {
            let module_name = alias
                .clone()
                .or_else(|| module_name_from_path(&canonical))
                .ok_or_else(|| {
                    anyhow!(
                        "import \"{}\" — could not derive a module name from the file stem; \
                         supply one with `(as=<ident>)`",
                        raw
                    )
                })?;
            if let Some(prev) = module_names.get(&module_name) {
                bail!(
                    "import \"{}\" — synthesised module name \"{}\" collides with another \
                     module declared in {}; rename with `(as=<ident>)`",
                    raw,
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
        } else if let Some(alias) = alias {
            // The user explicitly asked for an alias but the file has no
            // scene to bind it to — that's almost certainly a mistake.
            bail!(
                "import \"{}\" specified `(as={})`, but the imported file has no \
                 top-level `scene` block to alias",
                raw,
                alias
            );
        }
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

fn resolve_import_path(raw: &str, base_dir: Option<&Path>) -> Result<PathBuf> {
    let p = Path::new(raw);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    let base = base_dir.ok_or_else(|| {
        anyhow!(
            "import \"{}\" is relative but no source directory is set; \
             pass an absolute path or call `lower_with_source` with the \
             importing file's directory",
            raw
        )
    })?;
    Ok(base.join(p))
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
}
