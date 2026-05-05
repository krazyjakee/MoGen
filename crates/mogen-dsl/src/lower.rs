mod anim;
mod branch;
mod connector;
mod csg;
mod deform;
mod helpers;
mod layout;
mod light;
mod lod;
mod material;
mod node;
mod primitive;

#[cfg(test)]
mod tests;

use anyhow::Result;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use mogen_core::{subtree_local_aabb, NodeId, SceneGraph};

use crate::ast::Node;
use crate::attach::resolve_attaches;
use crate::conform::resolve_conforms;
use crate::module::{
    collect_modules, expand_modules, resolve_imports_with_loader,
    resolve_registry_uses_with_loader, FsLoader, Loader,
};
use crate::skin_lower::{bind_meshes, lower_skeleton};

use anim::{is_anim_decl, lower_animations};
use lod::extract_lod_scale;
use material::collect_materials;
use node::lower_into;

thread_local! {
    // Build-pass-scoped LOD multiplier. Set by `lower()` before walking the
    // expanded AST and read by `primitive_mesh` so segment/ring defaults can be
    // scaled without threading an extra arg through every recursive call.
    pub(super) static LOD_SCALE: Cell<f32> = const { Cell::new(1.0) };
    // Directory of the `.mog` file being lowered. Used by the `mesh`
    // primitive to resolve relative `src` paths. None = no source path
    // available; the lowering will fail if the path isn't absolute.
    pub(super) static SOURCE_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    // Nodes whose source carried `collider="aabb"`. Filled in by `lower_into`
    // and drained by a post-pass in `lower_with_source` after attach/conform/
    // skin binding so the computed AABB reflects the *final* mesh state.
    pub(super) static COLLIDER_REQUESTS: RefCell<Vec<NodeId>> = const { RefCell::new(Vec::new()) };
}

/// Returns the source directory currently set on the lowering thread, if any.
pub(super) fn source_dir() -> Option<PathBuf> {
    SOURCE_DIR.with(|s| s.borrow().clone())
}

/// RAII guard that sets the source directory for the duration of a single
/// `lower(_with_source)` call and restores the previous value on drop.
struct SourceDirGuard {
    prev: Option<PathBuf>,
}

impl SourceDirGuard {
    fn set(dir: Option<PathBuf>) -> Self {
        let prev = SOURCE_DIR.with(|s| s.replace(dir));
        Self { prev }
    }
}

impl Drop for SourceDirGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        SOURCE_DIR.with(|s| s.replace(prev));
    }
}

/// RAII guard that clears the collider-request list for the duration of one
/// `lower()` call so requests from a previous (possibly failed) lowering can't
/// leak into this one's post-pass.
struct ColliderRequestsGuard {
    prev: Vec<NodeId>,
}

impl ColliderRequestsGuard {
    fn fresh() -> Self {
        let prev = COLLIDER_REQUESTS.with(|s| std::mem::take(&mut *s.borrow_mut()));
        Self { prev }
    }
}

impl Drop for ColliderRequestsGuard {
    fn drop(&mut self) {
        let prev = std::mem::take(&mut self.prev);
        COLLIDER_REQUESTS.with(|s| *s.borrow_mut() = prev);
    }
}

struct LodScaleGuard {
    prev: f32,
}

impl LodScaleGuard {
    fn set(scale: f32) -> Self {
        let prev = LOD_SCALE.with(|s| s.replace(scale));
        Self { prev }
    }
}

impl Drop for LodScaleGuard {
    fn drop(&mut self) {
        let prev = self.prev;
        LOD_SCALE.with(|s| s.set(prev));
    }
}

pub fn lower(ast: &[Node]) -> Result<SceneGraph> {
    lower_with_source(ast, None)
}

/// Like `lower`, but also sets the source directory used by the `mesh`
/// primitive to resolve relative `src=` paths. Pass the directory of the
/// `.mog` source — typically `path.parent()` for the file the user passed
/// to `mogen build`. Imports are resolved through an [`FsLoader`].
pub fn lower_with_source(ast: &[Node], source_dir: Option<&Path>) -> Result<SceneGraph> {
    let mut loader = FsLoader::new();
    lower_with_loader(ast, source_dir, &mut loader)
}

/// Like [`lower_with_source`] but with a caller-supplied [`Loader`]. Used by
/// `mogen-wasm` (in-memory file map + JS fetch callback) and by the MoGHub
/// upload validator (registry-backed); desktop callers should prefer
/// [`lower_with_source`].
///
/// `base_dir` still drives the `mesh` primitive's `src=` resolution and the
/// initial relative-path base for FS-style imports; loaders that ignore
/// disk paths can pass `None`.
pub fn lower_with_loader(
    ast: &[Node],
    base_dir: Option<&Path>,
    loader: &mut dyn Loader,
) -> Result<SceneGraph> {
    let _src = SourceDirGuard::set(base_dir.map(|p| p.to_path_buf()));
    let _coll = ColliderRequestsGuard::fresh();
    // Top-level `lod_scale (value=N)` multiplies primitive default segment/
    // ring counts. Stash on a thread-local before lowering so `primitive_mesh`
    // can read it without threading an extra arg through every recursive call.
    // Explicit per-primitive `segments=`/`rings=` still win.
    let _lod = LodScaleGuard::set(extract_lod_scale(ast));

    // Expand modules first: collect every `module` declaration, then substitute
    // `use` calls into concrete node trees. The result has no `module`/`use`/
    // `import` nodes and no `$param` references. Order of overlay is stdlib
    // < imports < user, so a user module shadows an imported one and an
    // imported module shadows a stdlib one.
    let mut reg = crate::stdlib::stdlib_registry().clone();
    let mut imported_decls = resolve_imports_with_loader(ast, base_dir, loader)?;
    // Cross-author registry refs (`use "@user/slug[@v]"`) flow through
    // `Loader::load_registry`. Walking them as a separate pass keeps
    // local-only callers (mogen-validate, the wasm playground, plain
    // `mogen check`) from triggering registry resolution.
    imported_decls.extend(resolve_registry_uses_with_loader(ast, loader)?);
    let imported_reg = collect_modules(&imported_decls)?;
    reg.extend_overlay(imported_reg);
    let user = collect_modules(ast)?;
    reg.extend_overlay(user);
    let (expanded, use_parents) = expand_modules(ast, &reg)?;

    let mut graph = SceneGraph::new();
    graph.use_parents = use_parents;
    // Lift the optional top-level `meta(...)` block. Pulled from the
    // pre-expansion AST so import-introduced nodes can't smuggle a `meta`
    // block into a downstream file.
    graph.meta = crate::meta::extract_meta(ast);

    // Pass 1: hoist every top-level and scene-level `material` declaration.
    // User materials register first so their MaterialId is lower than any
    // imported material with the same name; `find_material` returns the first
    // match, which is how user-declared materials shadow imported ones.
    collect_materials(&expanded, &mut graph)?;
    collect_materials(&imported_decls, &mut graph)?;

    // Pass 2: build scene graph (skip anim declarations — they need nodes first).
    for n in &expanded {
        match n.kind.as_str() {
            "material" => {} // already handled
            "lod_scale" => {} // build-time setting, consumed above
            "meta" => {} // already lifted onto graph.meta
            k if is_anim_decl(k) => {} // pass 3
            "skeleton" => {
                lower_skeleton(n, None, &mut graph)?;
            }
            "scene" => {
                for c in &n.children {
                    if c.kind == "material"
                        || c.kind == "attach"
                        || c.kind == "conform"
                        || is_anim_decl(&c.kind)
                    {
                        continue;
                    }
                    if c.kind == "skeleton" {
                        lower_skeleton(c, None, &mut graph)?;
                        continue;
                    }
                    lower_into(c, None, &mut graph)?;
                }
            }
            "attach" => {} // pass 2.4
            "conform" => {} // pass 2.45
            _ => {
                lower_into(n, None, &mut graph)?;
            }
        }
    }

    // Pass 2.4: resolve `attach` specs. Runs before skin binding so bind-pose
    // world matrices reflect final part positions.
    resolve_attaches(&expanded, &mut graph)?;

    // Pass 2.45: resolve `conform` specs. Runs after attach so an attached
    // child can also be conformed; runs before skin binding so bind-pose
    // world matrices reflect the deformed geometry.
    resolve_conforms(&expanded, &mut graph)?;

    // Pass 2.5: bind mesh nodes carrying `skin="<name>"` to their skeleton.
    // Runs after every mesh exists and before animations so weights are
    // computed against bind-pose world transforms.
    bind_meshes(&expanded, &mut graph)?;

    // Pass 2.6: resolve every `collider="aabb"` request into a node-local
    // `Aabb`. Runs after attach/conform/skin so the AABB matches the final
    // mesh state (a conformed mesh's bent vertices, an attached child's
    // re-positioned subtree). Nodes whose subtree carries no mesh leave
    // `collider = None` silently — the user wrote the attribute but had
    // nothing to enclose; the studio inspector / glTF extras simply omit it.
    let pending: Vec<NodeId> =
        COLLIDER_REQUESTS.with(|r| std::mem::take(&mut *r.borrow_mut()));
    for id in pending {
        if let Some(aabb) = subtree_local_aabb(&graph, id) {
            graph.nodes[id.0 as usize].collider = Some(aabb);
        }
    }

    // Pass 3: joints first (clips may reference joint names), then clips,
    // then procedural templates (which can target either joints or nodes).
    // Imported animations live inside their synthesised module body, so they
    // arrive through `use` expansion and are already present in `expanded` —
    // no separate walk needed.
    lower_animations(&expanded, &mut graph)?;
    Ok(graph)
}
