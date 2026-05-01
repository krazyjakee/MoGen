use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use mogen_core::Span;

use crate::ast::{Expr, Node, Value};
use crate::parser::parse;

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    /// `None` means required; the caller must supply a value.
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct ModuleDef {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Vec<Node>,
    pub span: Span,
    /// One-line description, populated by the stdlib loader from a leading
    /// `// summary:` comment. User-declared modules leave this `None`.
    pub doc: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ModuleRegistry {
    modules: HashMap<String, ModuleDef>,
}

impl ModuleRegistry {
    pub fn get(&self, name: &str) -> Option<&ModuleDef> {
        self.modules.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.modules.contains_key(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.modules.keys()
    }

    /// Insert or overwrite a single module definition.
    pub fn insert(&mut self, def: ModuleDef) {
        self.modules.insert(def.name.clone(), def);
    }

    /// Pop a module out of the registry by name.
    pub fn remove(&mut self, name: &str) -> Option<ModuleDef> {
        self.modules.remove(name)
    }

    /// Merge `other` into `self`. On name collision `other`'s definition
    /// wins — used to overlay user modules on top of the stdlib so a
    /// scene can shadow a stdlib name.
    pub fn extend_overlay(&mut self, other: ModuleRegistry) {
        for (name, def) in other.modules {
            self.modules.insert(name, def);
        }
    }
}

/// Walk the top level of `ast` and lift every `module` declaration into a registry.
pub fn collect_modules(ast: &[Node]) -> Result<ModuleRegistry> {
    let mut reg = ModuleRegistry::default();
    for n in ast {
        if n.kind != "module" {
            continue;
        }
        let name = n
            .name
            .clone()
            .ok_or_else(|| anyhow!("module declaration requires a name"))?;
        let mut params = Vec::new();
        for (k, v) in &n.attrs {
            let default = match v {
                Value::Number(n) => Some(Expr::Num(*n)),
                Value::Expr(e) => Some(e.clone()),
                Value::Vec3(_) | Value::Vec3Expr(_) => {
                    bail!(
                        "module parameter `{}` must be a scalar default, not a vec3",
                        k
                    );
                }
                Value::List(_) | Value::ListExpr(_)
                | Value::ListVec3(_) | Value::ListPair(_) | Value::ListQuad(_) => {
                    bail!(
                        "module parameter `{}` must be a scalar default, not a list",
                        k
                    );
                }
                // String/ident defaults aren't meaningful for numeric `$param` substitution.
                Value::String(_) | Value::Ident(_) => {
                    bail!(
                        "module parameter `{}` default must be a number or expression",
                        k
                    );
                }
            };
            params.push(Param { name: k.clone(), default });
        }
        if reg.modules.contains_key(&name) {
            bail!("duplicate module declaration \"{name}\"");
        }
        reg.modules.insert(
            name.clone(),
            ModuleDef { name, params, body: n.children.clone(), span: n.span, doc: None },
        );
    }
    Ok(reg)
}

/// Remove every top-level `module` and `import` node and expand every `use`
/// node against `reg`. Expansion is recursive: modules may invoke other modules.
pub fn expand_modules(ast: &[Node], reg: &ModuleRegistry) -> Result<Vec<Node>> {
    let mut out = Vec::with_capacity(ast.len());
    let mut next_use_id: u32 = 1;
    for n in ast {
        if n.kind == "module" || n.kind == "import" {
            continue;
        }
        expand_node_into(n, reg, &Scope::default(), &mut Vec::new(), &mut out, None, &mut next_use_id)?;
    }
    Ok(out)
}

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

#[derive(Default, Clone)]
struct Scope {
    bindings: Vec<(String, f32)>,
}

impl Scope {
    fn lookup(&self, name: &str) -> Option<f32> {
        self.bindings.iter().rfind(|(k, _)| k == name).map(|(_, v)| *v)
    }
}

fn expand_node_into(
    node: &Node,
    reg: &ModuleRegistry,
    scope: &Scope,
    stack: &mut Vec<String>,
    out: &mut Vec<Node>,
    current_use: Option<u32>,
    next_use: &mut u32,
) -> Result<()> {
    if node.kind == "use" {
        expand_use(node, reg, scope, stack, out, current_use, next_use)?;
        return Ok(());
    }

    // Non-use: deep-clone with $-ref substitution, then recurse into children.
    let mut cloned = Node {
        kind: node.kind.clone(),
        name: node.name.clone(),
        attrs: node
            .attrs
            .iter()
            .map(|(k, v)| Ok((k.clone(), substitute_value(v, scope)?)))
            .collect::<Result<Vec<_>>>()?,
        children: Vec::new(),
        span: node.span,
        kind_span: node.kind_span,
        use_id: current_use,
        origin: node.origin.clone(),
    };
    for c in &node.children {
        expand_node_into(c, reg, scope, stack, &mut cloned.children, current_use, next_use)?;
    }
    out.push(cloned);
    Ok(())
}

fn expand_use(
    node: &Node,
    reg: &ModuleRegistry,
    scope: &Scope,
    stack: &mut Vec<String>,
    out: &mut Vec<Node>,
    current_use: Option<u32>,
    next_use: &mut u32,
) -> Result<()> {
    let module_name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("`use` requires a module name, e.g. `use \"leg\" (...)`"))?;
    let def = reg
        .get(&module_name)
        .ok_or_else(|| anyhow!("unknown module \"{}\"", module_name))?;

    if stack.iter().any(|n| n == &module_name) {
        bail!(
            "recursive module expansion: {} -> {}",
            stack.join(" -> "),
            module_name
        );
    }

    // Resolve caller args (in the caller's scope) first, then fill in defaults.
    let mut supplied: HashMap<String, f32> = HashMap::new();
    for (k, v) in &node.attrs {
        let n = scalar_value(v, scope).ok_or_else(|| {
            anyhow!(
                "argument `{k}` for module \"{module_name}\" must be a number or expression"
            )
        })?;
        supplied.insert(k.clone(), n);
    }

    // Reject unknown args early so typos don't silently no-op.
    let param_names: Vec<&str> = def.params.iter().map(|p| p.name.as_str()).collect();
    for k in supplied.keys() {
        if !param_names.contains(&k.as_str()) {
            bail!(
                "module \"{}\" has no parameter `{}` (known: {:?})",
                module_name,
                k,
                param_names
            );
        }
    }

    let mut call_scope = Scope::default();
    for p in &def.params {
        let value = match supplied.get(&p.name) {
            Some(v) => *v,
            None => match &p.default {
                Some(e) => e
                    .eval(&|n| call_scope.lookup(n))
                    .ok_or_else(|| {
                        anyhow!(
                            "default for `{}` in module \"{}\" references an unbound parameter",
                            p.name,
                            module_name
                        )
                    })?,
                None => bail!(
                    "module \"{}\" missing required parameter `{}`",
                    module_name,
                    p.name
                ),
            },
        };
        call_scope.bindings.push((p.name.clone(), value));
    }

    // Outermost `use` mints a fresh id; nested `use`s inherit so an attach
    // authored in the outer module still sees nodes brought in by an inner
    // sub-`use`. (Inner module attaches reference outer-module names like
    // `humanoid_full` referencing the `torso` from `humanoid_torso`.)
    let body_use = match current_use {
        Some(id) => Some(id),
        None => {
            let id = *next_use;
            *next_use += 1;
            Some(id)
        }
    };

    stack.push(module_name);
    for body_node in &def.body {
        expand_node_into(body_node, reg, &call_scope, stack, out, body_use, next_use)?;
    }
    stack.pop();
    Ok(())
}

fn substitute_value(value: &Value, scope: &Scope) -> Result<Value> {
    match value {
        Value::Number(_)
        | Value::Vec3(_)
        | Value::String(_)
        | Value::Ident(_)
        | Value::List(_)
        | Value::ListVec3(_)
        | Value::ListPair(_)
        | Value::ListQuad(_) => Ok(value.clone()),
        Value::Expr(e) => Ok(substitute_expr(e, scope)?),
        Value::Vec3Expr(components) => {
            let resolved: Vec<Value> = components
                .iter()
                .map(|c| substitute_expr(c, scope))
                .collect::<Result<_>>()?;
            // If all three resolved to concrete Numbers, collapse to Vec3.
            if resolved.iter().all(|v| matches!(v, Value::Number(_))) {
                let mut arr = [0.0f32; 3];
                for (i, v) in resolved.iter().enumerate() {
                    if let Value::Number(n) = v {
                        arr[i] = *n;
                    }
                }
                Ok(Value::Vec3(arr))
            } else {
                // Some component still references an unbound $param. Keep as Vec3Expr.
                let mut out = [Expr::Num(0.0), Expr::Num(0.0), Expr::Num(0.0)];
                for (i, v) in resolved.iter().enumerate() {
                    out[i] = match v {
                        Value::Number(n) => Expr::Num(*n),
                        Value::Expr(e) => e.clone(),
                        _ => unreachable!(),
                    };
                }
                Ok(Value::Vec3Expr(out))
            }
        }
        Value::ListExpr(components) => {
            let resolved: Vec<Value> = components
                .iter()
                .map(|c| substitute_expr(c, scope))
                .collect::<Result<_>>()?;
            if resolved.iter().all(|v| matches!(v, Value::Number(_))) {
                Ok(Value::List(
                    resolved
                        .iter()
                        .map(|v| if let Value::Number(n) = v { *n } else { 0.0 })
                        .collect(),
                ))
            } else {
                Ok(Value::ListExpr(
                    resolved
                        .into_iter()
                        .map(|v| match v {
                            Value::Number(n) => Expr::Num(n),
                            Value::Expr(e) => e,
                            _ => unreachable!(),
                        })
                        .collect(),
                ))
            }
        }
    }
}

/// Substitute known `$param`s into an expression, folding to Number if fully resolved.
fn substitute_expr(e: &Expr, scope: &Scope) -> Result<Value> {
    match e.eval(&|n| scope.lookup(n)) {
        Some(n) => Ok(Value::Number(n)),
        None => Ok(Value::Expr(rewrite_expr(e, scope))),
    }
}

fn rewrite_expr(e: &Expr, scope: &Scope) -> Expr {
    match e {
        Expr::Num(_) => e.clone(),
        Expr::Param(n) => match scope.lookup(n) {
            Some(v) => Expr::Num(v),
            None => e.clone(),
        },
        Expr::Bin(a, op, b) => Expr::Bin(
            Box::new(rewrite_expr(a, scope)),
            *op,
            Box::new(rewrite_expr(b, scope)),
        ),
    }
}

/// Evaluate a Value to an f32 scalar (used for module arguments).
/// Accepts Number, Expr, or a 1-component vec3 trivially? — no, only scalars.
fn scalar_value(v: &Value, scope: &Scope) -> Option<f32> {
    match v {
        Value::Number(n) => Some(*n),
        Value::Expr(e) => e.eval(&|n| scope.lookup(n)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn expand(src: &str) -> Vec<Node> {
        let ast = parse(src).unwrap();
        let reg = collect_modules(&ast).unwrap();
        expand_modules(&ast, &reg).unwrap()
    }

    fn first_attr<'a>(n: &'a Node, key: &str) -> &'a Value {
        n.attr(key).unwrap_or_else(|| panic!("missing attr {key}"))
    }

    #[test]
    fn use_substitutes_scalar_param() {
        let ast = expand(
            r#"
            module "leg" (height=0.5) {
              cylinder "leg" (height=$height)
            }
            scene { use "leg" (height=0.9) }
        "#,
        );
        // Modules stripped; scene survives with expanded body inlined.
        let scene = &ast[0];
        assert_eq!(scene.kind, "scene");
        assert_eq!(scene.children.len(), 1);
        let leg = &scene.children[0];
        assert_eq!(leg.kind, "cylinder");
        match first_attr(leg, "height") {
            Value::Number(n) => assert!((n - 0.9).abs() < 1e-6),
            other => panic!("expected Number, got {:?}", other),
        }
    }

    #[test]
    fn use_substitutes_vec3_and_arithmetic() {
        let ast = expand(
            r#"
            module "leg" (height=0.5, radius=0.05) {
              cylinder "leg" (pos=[0, $height * 0.5, 0], radius=$radius)
            }
            scene { use "leg" (height=1.0, radius=0.1) }
        "#,
        );
        let leg = &ast[0].children[0];
        match first_attr(leg, "pos") {
            Value::Vec3([x, y, z]) => {
                assert_eq!(*x, 0.0);
                assert!((*y - 0.5).abs() < 1e-6);
                assert_eq!(*z, 0.0);
            }
            other => panic!("expected Vec3, got {:?}", other),
        }
        match first_attr(leg, "radius") {
            Value::Number(n) => assert!((n - 0.1).abs() < 1e-6),
            other => panic!("expected Number, got {:?}", other),
        }
    }

    #[test]
    fn defaults_apply_when_caller_omits() {
        let ast = expand(
            r#"
            module "leg" (height=0.5, radius=0.05) {
              cylinder "leg" (height=$height, radius=$radius)
            }
            scene { use "leg" () }
        "#,
        );
        let leg = &ast[0].children[0];
        assert!(matches!(first_attr(leg, "height"), Value::Number(n) if (n - 0.5).abs() < 1e-6));
        assert!(matches!(first_attr(leg, "radius"), Value::Number(n) if (n - 0.05).abs() < 1e-6));
    }

    #[test]
    fn non_scalar_default_rejected() {
        // Vec3 defaults make no sense for `$param` arithmetic substitution.
        let src = r#"
            module "leg" (dims=[1, 1, 1]) { box "b" (size=$dims) }
        "#;
        let ast = parse(src).unwrap();
        let err = collect_modules(&ast).unwrap_err().to_string();
        assert!(err.contains("must be a scalar"), "got: {err}");
    }

    #[test]
    fn unknown_module_errors() {
        let src = r#"
            scene { use "ghost" () }
        "#;
        let ast = parse(src).unwrap();
        let reg = collect_modules(&ast).unwrap();
        let err = expand_modules(&ast, &reg).unwrap_err().to_string();
        assert!(err.contains("unknown module"), "got: {err}");
    }

    #[test]
    fn unknown_arg_errors() {
        let src = r#"
            module "leg" (height=0.5) { cylinder "leg" (height=$height) }
            scene { use "leg" (color=1.0) }
        "#;
        let ast = parse(src).unwrap();
        let reg = collect_modules(&ast).unwrap();
        let err = expand_modules(&ast, &reg).unwrap_err().to_string();
        assert!(err.contains("has no parameter"), "got: {err}");
    }

    #[test]
    fn recursive_module_errors() {
        let src = r#"
            module "a" () { use "b" () }
            module "b" () { use "a" () }
            scene { use "a" () }
        "#;
        let ast = parse(src).unwrap();
        let reg = collect_modules(&ast).unwrap();
        let err = expand_modules(&ast, &reg).unwrap_err().to_string();
        assert!(err.contains("recursive module expansion"), "got: {err}");
    }

    #[test]
    fn nested_use_resolves() {
        let ast = expand(
            r#"
            module "inner" (s=1.0) { box "b" (size=[$s, $s, $s]) }
            module "outer" (k=2.0) { use "inner" (s=$k) }
            scene { use "outer" (k=3.0) }
        "#,
        );
        let b = &ast[0].children[0];
        assert_eq!(b.kind, "box");
        match first_attr(b, "size") {
            Value::Vec3([x, y, z]) => {
                assert_eq!(*x, 3.0);
                assert_eq!(*y, 3.0);
                assert_eq!(*z, 3.0);
            }
            other => panic!("expected Vec3, got {:?}", other),
        }
    }

    #[test]
    fn csg_lowers_to_single_mesh_node() {
        // This exercises the full path: parse → expand_modules → lower.
        // A `difference` should collapse its children into one mesh on itself.
        let src = r#"
            scene {
              difference "wall_with_door" {
                box "wall" (size=[4, 3, 0.2])
                box "doorway" (pos=[0, -0.5, 0], size=[0.9, 2.0, 0.5])
              }
            }
        "#;
        let ast = parse(src).unwrap();
        let scene = crate::lower(&ast).unwrap();
        // One root node, one mesh, no orphan box nodes.
        assert_eq!(scene.roots.len(), 1);
        let root = &scene.nodes[scene.roots[0].0 as usize];
        assert_eq!(root.kind, "difference");
        assert!(root.mesh.is_some());
        assert!(root.children.is_empty(), "CSG operand children must be consumed");
    }

    #[test]
    fn module_nodes_stripped_from_output() {
        let ast = expand(
            r#"
            module "m" () { box "b" (size=[1,1,1]) }
            scene { use "m" () }
        "#,
        );
        assert_eq!(ast.len(), 1);
        assert_eq!(ast[0].kind, "scene");
    }

    // ---- Imports --------------------------------------------------------

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
        let imported_reg = collect_modules(&imported).unwrap();
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
    fn imported_material_collision_is_first_wins() {
        // Cross-file material name duplicates aren't fatal — `find_material`
        // returns the first match by index, which is the first import (or the
        // user's own declaration if they added one). Composing two third-party
        // objects that happen to share a material name shouldn't block the
        // build.
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
        // a.mog's material registers first, so its dark-grey colour is what
        // `find_material("wood")` resolves to.
        let wood = scene
            .materials
            .iter()
            .find(|m| m.name == "wood")
            .expect("imported material should be registered");
        assert!((wood.base_color[0] - 0.1).abs() < 1e-6, "got {wood:?}");
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
