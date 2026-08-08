mod anim;
pub mod arch;
mod blob;
mod branch;
mod building;
mod cave;
mod cfg;
pub(crate) mod connector;
mod csg;
mod deform;
mod dungeon;
mod faced_box;
mod geometry_identity;
mod gradient_bake;
mod helpers;
mod layout;
mod light;
mod lod;
mod material;
mod node;
mod physics;
mod poi;
mod primitive;
mod procedural;
mod rng;
mod shader;
mod terrain;

#[cfg(test)]
mod tests;

use anyhow::Result;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mogen_core::{subtree_local_aabb, NodeId, SceneGraph};

use crate::ast::Node;
use crate::attach::resolve_attaches;
use crate::conform::resolve_conforms;
use crate::module::{
    collect_modules, expand_modules, resolve_imports_with_loader,
    resolve_registry_uses_with_loader, FsLoader, Loader, ModuleRegistry,
};
use crate::skin_lower::{bind_meshes, lower_skeleton};

use anim::{is_anim_decl, lower_animations};
use geometry_identity::TessellationCacheGuard;
use lod::{collect_origin_lods, extract_lod_scale, LodByOriginGuard, LodRequestGuard};
use material::collect_materials;
use node::lower_into;
use physics::collect_physics;
use shader::{collect_shaders, ensure_builtin_shaders};

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
    // Combined module registry for the current `lower()` call. Set after
    // imports/registry refs have merged, read by `expand_building` so it can
    // instantiate the door/window/skylight modules referenced by a
    // `building` node. None outside a `lower()` call.
    pub(super) static MODULE_REGISTRY: RefCell<Option<ModuleRegistry>> =
        const { RefCell::new(None) };
    // Bytes a caller's `Loader::load_binary` supplied for each `mesh (src=…)`
    // in this lowering, keyed by the `src` attribute verbatim. Filled by
    // `collect_mesh_binaries` over the *expanded* AST; read by `primitive_mesh`,
    // which reads the file itself when a spec is absent.
    pub(super) static MESH_BYTES: RefCell<HashMap<String, Vec<u8>>> =
        RefCell::new(HashMap::new());
}

/// Returns the source directory currently set on the lowering thread, if any.
pub(super) fn source_dir() -> Option<PathBuf> {
    SOURCE_DIR.with(|s| s.borrow().clone())
}

/// The bytes the caller's [`Loader`] supplied for `mesh (src=spec)`, if any.
///
/// `None` means "nobody supplied these", **not** "this mesh has no bytes" —
/// the primitive then resolves the file itself, which is what every loader
/// written before [`Loader::load_binary`] existed relies on.
pub(super) fn mesh_bytes(spec: &str) -> Option<Vec<u8>> {
    MESH_BYTES.with(|m| m.borrow().get(spec).cloned())
}

/// Ask `loader` for the bytes behind every `mesh (src=…)` reachable in `ast`.
///
/// **Runs on the expanded AST**, after `expand_modules`, for two reasons that
/// both bite: a `src=` inside a `module` is `$param`-substituted only by
/// expansion, and a module nobody `use`s is gone by then — so pre-loading
/// before expansion would fetch a spec that is not a path yet, and fetch files
/// for geometry the scene never instantiates.
///
/// **Failures are dropped, not raised.** A spec the loader cannot serve is
/// simply absent from the map, and `primitive_mesh` falls back to reading it
/// from disk exactly as it always did — so this pass is strictly additive:
/// every existing caller, including every `Loader` written before
/// `load_binary` existed, produces precisely the lowering it produced before.
/// Raising here would also move *when* a missing-file error is reported, from
/// the node that names it to the whole scene.
fn collect_mesh_binaries(
    ast: &[Node],
    base_dir: Option<&Path>,
    loader: &mut dyn Loader,
) -> HashMap<String, Vec<u8>> {
    fn walk(nodes: &[Node], out: &mut Vec<String>) {
        for n in nodes {
            if n.kind == "mesh" {
                if let Some(src) = n.attr_string("src") {
                    out.push(src.to_string());
                }
            }
            walk(&n.children, out);
        }
    }
    let mut specs = Vec::new();
    walk(ast, &mut specs);
    specs.sort();
    specs.dedup();

    let mut map = HashMap::new();
    for spec in specs {
        if let Ok(bytes) = loader.load_binary(&spec, base_dir) {
            map.insert(spec, bytes);
        }
    }
    map
}

/// RAII guard publishing [`MESH_BYTES`] for one `lower()` call, so a previous
/// (possibly failed) lowering's bytes cannot leak into this one — the reason
/// [`ColliderRequestsGuard`] exists, one map along.
struct MeshBytesGuard {
    prev: HashMap<String, Vec<u8>>,
}

impl MeshBytesGuard {
    fn set(map: HashMap<String, Vec<u8>>) -> Self {
        let prev = MESH_BYTES.with(|m| std::mem::replace(&mut *m.borrow_mut(), map));
        Self { prev }
    }
}

impl Drop for MeshBytesGuard {
    fn drop(&mut self) {
        let prev = std::mem::take(&mut self.prev);
        MESH_BYTES.with(|m| *m.borrow_mut() = prev);
    }
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

/// RAII guard that publishes the combined module registry for the duration
/// of one `lower()` call. `building` lowering reads it to expand the user-
/// supplied door / window / skylight module references.
struct ModuleRegistryGuard {
    prev: Option<ModuleRegistry>,
}

impl ModuleRegistryGuard {
    fn set(reg: ModuleRegistry) -> Self {
        let prev = MODULE_REGISTRY.with(|s| s.replace(Some(reg)));
        Self { prev }
    }
}

impl Drop for ModuleRegistryGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        MODULE_REGISTRY.with(|s| s.replace(prev));
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
    lower_with_loader_lod(ast, base_dir, loader, 1.0)
}

/// [`lower_with_loader`] at a caller-chosen **tessellation density**.
///
/// `lod_scale` multiplies every segment / ring / sample count the lowering
/// produces: `0.5` halves them, `2.0` doubles them, `1.0` is exactly what every
/// other entry point does. Non-positive and non-finite values fall back to
/// `1.0`, the rule the `lod_scale (value=N)` directive already follows.
///
/// **This is a bake-time parameter, not an authoring one.** The DSL already has
/// three ways for a *file* to state its density — the top-level
/// `lod_scale (value=N)`, each import's own, and a per-node `lod=N` — and this
/// changes none of them. It is the knob a **caller** needs to lower the same
/// AST more than once and get genuinely different geometry each time, which is
/// what building a LOD chain out of retained analytic parameters requires: a
/// coarse level here is the same sphere re-tessellated, not a decimated mesh,
/// so it is exact at every level rather than approximate.
///
/// The request **multiplies** the source's own scales rather than replacing
/// them, so an authored detail hierarchy survives: a part marked `lod=2` is
/// still twice its neighbours at every density anyone asks for.
///
/// Existing callers are untouched and produce byte-identical graphs —
/// `lower_with_loader` passes `1.0`, and a multiply by one changes nothing.
pub fn lower_with_loader_lod(
    ast: &[Node],
    base_dir: Option<&Path>,
    loader: &mut dyn Loader,
    lod_scale: f32,
) -> Result<SceneGraph> {
    let _lod_req = LodRequestGuard::set(lod_scale);
    let _src = SourceDirGuard::set(base_dir.map(|p| p.to_path_buf()));
    let _coll = ColliderRequestsGuard::fresh();
    let _tessellations = TessellationCacheGuard::fresh();
    // Top-level `lod_scale (value=N)` multiplies primitive default segment/
    // ring counts. Stash on a thread-local before lowering so `primitive_mesh`
    // can read it without threading an extra arg through every recursive call.
    // Explicit per-primitive `segments=`/`rings=` still win.
    let _lod = LodScaleGuard::set(extract_lod_scale(ast));
    // Per-origin LOD overrides: imported files carry their own
    // `lod_scale` directives that need to apply to that file's geometry,
    // not the importing file's. Reset for this lower call; the lifted
    // directives below populate the map.
    let _lod_by_origin = LodByOriginGuard::fresh();

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
    // Each imported file's top-level `lod_scale (value=N)` is lifted into
    // `imported_decls` with its origin stamped. Register them now so
    // `lower_into` can swap `LOD_SCALE` to the imported file's value
    // when descending into geometry that originated there.
    collect_origin_lods(&imported_decls);
    let imported_reg = collect_modules(&imported_decls)?;
    reg.extend_overlay(imported_reg);
    let user = collect_modules(ast)?;
    reg.extend_overlay(user);
    // Publish the combined registry for the rest of the lowering pass.
    // `expand_building` reads it to instantiate door/window/skylight modules.
    // Clone is unavoidable — `expand_modules` borrows `reg` for the duration
    // of the call below, and the guard needs an owned value.
    let _reg_guard = ModuleRegistryGuard::set(reg.clone());
    let (expanded, use_parents) = expand_modules(ast, &reg)?;

    // Give the caller's loader a chance to serve every external mesh, now that
    // module expansion has turned `src=$param` into a real spec and dropped the
    // modules nobody instantiated. A loader that declines leaves the primitive
    // reading the file itself, exactly as before.
    let _mesh_bytes = MeshBytesGuard::set(collect_mesh_binaries(&expanded, base_dir, loader));

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
    // Shader declarations hoist in the same pass and on the same dedupe rules;
    // materials reference them by name via `shader=`. Built-in presets (water)
    // are seeded last so a user `shader "water"` shadows them.
    collect_shaders(&expanded, &mut graph)?;
    collect_shaders(&imported_decls, &mut graph)?;
    ensure_builtin_shaders(&mut graph);
    // Physics substances hoist in the same pass, on the same dedupe rules, so
    // `phys=` references resolve during Pass 2 exactly like `mat=`.
    collect_physics(&expanded, &mut graph)?;
    collect_physics(&imported_decls, &mut graph)?;

    // Pass 2: build scene graph (skip anim declarations — they need nodes first).
    for n in &expanded {
        match n.kind.as_str() {
            "material" => {}           // already handled
            "physics" => {}            // hoisted in Pass 1
            "shader" => {}             // hoisted in Pass 1
            "lod_scale" => {}          // build-time setting, consumed above
            "meta" => {}               // already lifted onto graph.meta
            k if is_anim_decl(k) => {} // pass 3
            "skeleton" => {
                lower_skeleton(n, None, &mut graph)?;
            }
            "scene" => {
                for c in &n.children {
                    if c.kind == "material"
                        || c.kind == "physics"
                        || c.kind == "shader"
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
            "attach" => {}  // pass 2.4
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
    let pending: Vec<NodeId> = COLLIDER_REQUESTS.with(|r| std::mem::take(&mut *r.borrow_mut()));
    for id in pending {
        if let Some(aabb) = subtree_local_aabb(&graph, id) {
            graph.nodes[id.0 as usize].collider = Some(mogen_core::ColliderShape::Aabb { aabb });
        }
    }

    // Pass 2.65: auto-weigh physics bodies from the final geometry. Runs after
    // collider (so it sees the same post attach/conform/skin mesh).
    let worlds = graph.world_transforms();
    // (a) Leaves — nodes with their own mesh. Weight = substance density × world
    // volume (the world-transform determinant folds in this node's + ancestors'
    // scale, so `scale=2` weighs 8×); centre of gravity = the mesh's volume
    // centroid, in local space. An explicit `weight=` keeps its overridden mass
    // but still gets a real centre of gravity.
    for id in 0..graph.nodes.len() {
        let node = &graph.nodes[id];
        let Some(body) = &node.physics else { continue };
        let Some(mesh) = &node.mesh else { continue };
        let mass = if body.mass.is_some() {
            body.mass
        } else {
            // Group as `density × (volume × det)` — the same association the
            // golden GLBs were baked with; regrouping shifts the last f32 ULP.
            let world_volume = mesh.solid_volume() * worlds[id].determinant().abs();
            Some(body.weight_per_m3 * world_volume)
        };
        let cog = mesh.solid_centroid();
        let b = graph.nodes[id].physics.as_mut().unwrap();
        b.mass = mass;
        b.center_of_gravity = cog;
    }
    // (b) Compound bodies — a node that carries a physics body but has no mesh
    // of its own (a `group phys=…`, possibly inherited) reports the *combined*
    // mass and mass-weighted centre of gravity of every mesh-bearing descendant,
    // expressed in its own local frame. An engine can then treat the whole
    // assembly as one rigid body. Only own-mesh descendants contribute, so a
    // compound group nested above another never double-counts the shared
    // leaves. Runs after (a) so leaf masses already exist.
    for id in 0..graph.nodes.len() {
        if graph.nodes[id].physics.is_none() || graph.nodes[id].mesh.is_some() {
            continue;
        }
        let mut total = 0.0f32;
        let mut weighted = glam::Vec3::ZERO;
        collect_subtree_mass(
            &graph,
            NodeId(id as u32),
            &worlds,
            &mut total,
            &mut weighted,
        );
        if total > 0.0 {
            let local_com = worlds[id].inverse().transform_point3(weighted / total);
            let b = graph.nodes[id].physics.as_mut().unwrap();
            b.mass = Some(total);
            b.center_of_gravity = Some([local_com.x, local_com.y, local_com.z]);
        }
    }

    // Pass 2.7: propagate `cast_shadow=false` down the subtree so a `group`
    // (or wrapping `use`) can opt out an entire subassembly with one flag.
    // Walking root-first means an ancestor's `false` overrides any descendant
    // default; a descendant that authored its own `cast_shadow=0` already
    // landed on `false` during lowering, so no information is lost. The flag
    // is monotone — false stays false — which keeps the propagation order
    // insensitive to sibling traversal.
    let roots: Vec<NodeId> = graph.roots.clone();
    for r in roots {
        propagate_cast_shadow(&mut graph, r, true);
    }

    // Pass 3: joints first (clips may reference joint names), then clips,
    // then procedural templates (which can target either joints or nodes).
    // Imported animations live inside their synthesised module body, so they
    // arrive through `use` expansion and are already present in `expanded` —
    // no separate walk needed.
    lower_animations(&expanded, &mut graph)?;

    // Pass 4: bake material gradients into per-vertex `Mesh.colors`. Runs
    // last so every geometry-shaping pass above (CSG, conform, attach, skin
    // binding) has settled — the AABB the bake samples against matches what
    // the exporter will write.
    gradient_bake::bake_gradients(&mut graph);

    Ok(graph)
}

/// Accumulate the mass and mass-weighted *world-space* centre of gravity of
/// every mesh-bearing physics body strictly below `id`. Only own-mesh nodes
/// contribute, so a compound group above another compound group can't
/// double-count the leaves they share.
fn collect_subtree_mass(
    graph: &SceneGraph,
    id: NodeId,
    worlds: &[glam::Mat4],
    total: &mut f32,
    weighted: &mut glam::Vec3,
) {
    let children = graph.nodes[id.0 as usize].children.clone();
    for c in children {
        let node = &graph.nodes[c.0 as usize];
        if node.mesh.is_some() {
            if let Some(m) = node.physics.as_ref().and_then(|b| b.mass) {
                let cog = node
                    .physics
                    .as_ref()
                    .and_then(|b| b.center_of_gravity)
                    .unwrap_or([0.0, 0.0, 0.0]);
                let world_pt = worlds[c.0 as usize].transform_point3(glam::Vec3::from_array(cog));
                *total += m;
                *weighted += m * world_pt;
            }
        }
        collect_subtree_mass(graph, c, worlds, total, weighted);
    }
}

/// Walk the subtree rooted at `id` and clear `cast_shadow` on every node
/// whose ancestor chain contains a node already opted out. `inherited` is the
/// effective flag from the parent (`true` for root calls).
fn propagate_cast_shadow(graph: &mut SceneGraph, id: NodeId, inherited: bool) {
    let effective = inherited && graph.nodes[id.0 as usize].cast_shadow;
    graph.nodes[id.0 as usize].cast_shadow = effective;
    let children = graph.nodes[id.0 as usize].children.clone();
    for c in children {
        propagate_cast_shadow(graph, c, effective);
    }
}
