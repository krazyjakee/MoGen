//! WebAssembly bindings for the MoGen pipeline.
//!
//! Two entry points:
//!
//! - [`compile`] — single-file shim, takes a `&str` and runs the full
//!   parse → validate → lower → validate → export chain. Kept for the v0
//!   spike and any embedder that only ever ships one file.
//! - [`compile_files`] — multi-file entry. Takes a JS `Map<filename, source>`
//!   for the editor's open tabs and a JS `fetch_dep(spec) -> Promise<string>`
//!   callback for registry pins like `@user/foo@1.2.0`. The MoGHub frontend
//!   plugs `fetch_dep` into `/api/v/<model_version_id>/<filename>` keyed by
//!   the resolved pins from `mog.lock`. This is the editor's primary entry
//!   in MoGHub Phase 4b.
//!
//! Both share the same compilation pipeline through the [`mogen_dsl::Loader`]
//! abstraction: desktop CLI, axum upload validator, and the wasm editor all
//! drive the *same* import resolver against three different `Loader` impls,
//! which is the architectural promise that gates MoGHub Phase 4b on this
//! crate.
//!
//! ## Feature parity with desktop
//!
//! CSG (`union`/`difference`/`intersect`) and sibling-mesh merge are
//! supported here via `manifold-csg`'s `unstable-wasm-uu` feature, which
//! cross-compiles the same Manifold C++ library used on desktop through
//! `wasm-cxx-shim`. Output is byte-identical to the desktop build for the
//! same input — no BSP-vs-Manifold divergence. Build host requires LLVM
//! 20+ (see workspace README for setup).
//!
//! ## Binary asset support
//!
//! `compile_files` takes a second `Map<string, Uint8Array>` of asset bytes
//! keyed by the path the `.mog` source references (e.g.
//! `"textures/wood/albedo.png"`). It backs both `texture = "…"` and
//! `mesh (src="…")` — the latter through `JsLoader::load_binary`, because
//! the browser has no filesystem for that method's default impl to read.
//!
//! Texture bytes are embedded as-is: no `image` / `oxipng` linkage, so the
//! wasm artifact stays small and avoids C deps that don't cross-compile.
//! Desktop builds re-encode + oxipng-shrink via the `textures-optimize`
//! feature, but that's gated off here, so authors should publish PNG/JPEG
//! already sized for the web.
//!
//! ## Still disabled
//!
//! - **`mogen-llm`**: not linked into the wasm crate. Generation / repair
//!   loops stay on the desktop CLI.
//!
//! ## Caveats inherited from `unstable-wasm-uu`
//!
//! Compiled `-fno-exceptions`: implicit STL throws (e.g. `bad_alloc` on
//! out-of-memory) become unrecoverable wasm traps rather than panics the
//! JS host can catch. The browser tab can recover by reloading.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use js_sys::{Function, Map, Promise};
use mogen_core::{Diagnostic, Severity};
use mogen_dsl::{parse_registry_spec, LoadedFile, Loader, Node, RegistrySpec};
use mogen_export::{
    build_glb_with_options_and_source, ExportOptions, MapTextureSource,
};
use mogen_validate::{render_json, validate_ast, validate_graph};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Result of a single `compile()` / `compile_files()` call. JS reads the
/// `glb` getter (a `Uint8Array`, or `null` on failure) and parses
/// `diagnostics` as line-delimited JSON — same format the desktop CLI emits
/// with `mogen check --json`.
#[wasm_bindgen]
pub struct CompileOutcome {
    glb: Option<Vec<u8>>,
    diagnostics: String,
    stage: &'static str,
}

#[wasm_bindgen]
impl CompileOutcome {
    #[wasm_bindgen(getter)]
    pub fn ok(&self) -> bool {
        self.glb.is_some()
    }

    #[wasm_bindgen(getter)]
    pub fn glb(&self) -> Option<Vec<u8>> {
        self.glb.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn diagnostics(&self) -> String {
        self.diagnostics.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn stage(&self) -> String {
        self.stage.to_string()
    }
}

/// Compile a single in-memory source string to GLB. Thin shim around
/// [`compile_files`] for embedders that only ever ship one file — any
/// `import` directives in `source` will fail to resolve because the file
/// map is empty and there is no `fetch_dep` callback. Use `compile_files`
/// for multi-tab editing.
#[wasm_bindgen]
pub fn compile(source: &str) -> CompileOutcome {
    let entry = "scene.mog";
    let mut files: HashMap<String, String> = HashMap::new();
    files.insert(entry.to_string(), source.to_string());
    compile_with_cache(entry, files, HashMap::new())
}

/// Multi-file compile. Drives `parse → validate → lower → validate → export`
/// against an [`mogen_dsl::Loader`] backed by `files` (the editor's open
/// tabs, keyed by filename) with `fetch_dep` as the fallback for any spec
/// not present locally — typically a `@user/slug@version` registry pin.
///
/// `entry` names the file in `files` to compile as the scene root.
/// Diagnostics carry per-file routing so the editor can surface errors on
/// the tab that caused them rather than always against the entry tab.
///
/// `textures` is a `Map<string, Uint8Array>` of binary assets, keyed by
/// the path the `.mog` source uses (e.g. `"textures/wood/albedo.png"`).
/// Pass an empty `Map` (or `null`/`undefined` from JS) when the scene has
/// none. Bytes are embedded into the GLB as-is — `image` and `oxipng`
/// aren't linked into the wasm artifact.
///
/// Despite the name it is every binary asset, not just images: `mesh
/// (src="model.glb")` resolves out of the same map, under the same
/// convention that the key is whatever path the source writes.
///
/// Async because `fetch_dep` returns a `Promise`; the implementation walks
/// the import graph BFS-style and awaits each missing spec before recursing
/// so that the synchronous resolver inside `mogen-dsl` can read every
/// reachable file out of an in-memory cache.
#[wasm_bindgen]
pub async fn compile_files(
    entry: String,
    files: Map,
    fetch_dep: Function,
    textures: JsValue,
) -> Result<CompileOutcome, JsValue> {
    let mut cache = drain_files(&files)?;
    if !cache.contains_key(&entry) {
        return Err(JsValue::from_str(&format!(
            "compile_files: entry '{}' is not present in files map",
            entry
        )));
    }
    prefetch_imports(&entry, &mut cache, &fetch_dep).await?;
    let textures = drain_textures(&textures)?;
    Ok(compile_with_cache(&entry, cache, textures))
}

/// Run the full compile pipeline against an already-populated file cache.
/// Shared by [`compile`] (single-file) and [`compile_files`] (multi-file
/// after async prefetch). `textures` is keyed by the path each
/// `texture = "..."` attribute references; an empty map means the export
/// pipeline only emits PBR-factor materials.
fn compile_with_cache(
    entry: &str,
    cache: HashMap<String, String>,
    textures: HashMap<PathBuf, Vec<u8>>,
) -> CompileOutcome {
    let mut loader = JsLoader {
        cache: &cache,
        assets: &textures,
    };

    // Parse the entry.
    let source = match cache.get(entry) {
        Some(s) => s.as_str(),
        None => {
            return fail(
                entry,
                "parse",
                vec![Diagnostic::error(
                    "E0001",
                    format!("entry '{}' missing from cache (internal bug)", entry),
                )],
            );
        }
    };
    // Module-only files (e.g. a wizard per-object `.mog`) need a transient
    // `scene { use "X" () }` so the previewer has something to render. The
    // on-disk source is untouched — the rewrite lives only in this call.
    let preview_src;
    let source = match mogen_dsl::synthesise_standalone_module_use(source) {
        Some(rewritten) => {
            preview_src = rewritten;
            preview_src.as_str()
        }
        None => source,
    };
    let ast = match mogen_dsl::parse(source) {
        Ok(a) => a,
        Err(e) => {
            return fail(
                entry,
                "parse",
                vec![Diagnostic::error("E0001", format!("parse: {e}"))
                    .with_file(entry)],
            );
        }
    };

    // AST validation against the entry's own AST. Imported files are parsed
    // and validated lazily as part of `lower_with_loader`'s import walk —
    // any error there comes back as an `anyhow::Error` carrying the file
    // path in its message.
    let mut diags = validate_ast(&ast);
    if diags.iter().any(|d| matches!(d.severity, Severity::Error)) {
        for d in &mut diags {
            if d.file.is_none() {
                d.file = Some(entry.to_string());
            }
        }
        return fail(entry, "validate_ast", diags);
    }

    // Lower with the JsLoader so multi-file imports resolve out of `cache`.
    // `base_dir = None` since the wasm side has no real filesystem; relative
    // imports keyed off the file map work because JsLoader's `load` uses the
    // spec directly as the cache key.
    let scene = match mogen_dsl::lower_with_loader(&ast, None, &mut loader) {
        Ok(s) => s,
        Err(e) => {
            diags.push(
                Diagnostic::error("E0701", format!("lowering: {e}")).with_file(entry),
            );
            return fail(entry, "lower", diags);
        }
    };

    // Graph validation — diagnostics here have spans relative to the file
    // each Node was lifted from, but they carry no `file` of their own (the
    // graph validators don't see Node.origin). Default to the entry tab so
    // the user at least sees the error on the active surface.
    let graph_diags = validate_graph(&scene);
    diags.extend(graph_diags);
    for d in &mut diags {
        if d.file.is_none() {
            d.file = Some(entry.to_string());
        }
    }
    if diags.iter().any(|d| matches!(d.severity, Severity::Error)) {
        return fail(entry, "validate_graph", diags);
    }

    // Export. Textures embed bytes from the in-memory map (oxipng/image
    // stay off in the wasm build); CSG-backed merge is on.
    let opts = ExportOptions {
        include_animations: true,
        include_textures: true,
        merge_sibling_meshes: true,
        // wasm builds disable both the `lod` and `imposter` features, so
        // this flag is a no-op even if some path through the bridge ever
        // sets it.
        bundle_lods_and_imposter: false,
    };
    let texture_source = MapTextureSource::new(textures);
    match build_glb_with_options_and_source(&scene, &opts, &texture_source, |_| {}) {
        Ok(bytes) => CompileOutcome {
            glb: Some(bytes),
            diagnostics: render_json(entry, &diags),
            stage: "ok",
        },
        Err(e) => {
            diags.push(
                Diagnostic::error("E0900", format!("export: {e}")).with_file(entry),
            );
            fail(entry, "export", diags)
        }
    }
}

fn fail(entry: &str, stage: &'static str, diags: Vec<Diagnostic>) -> CompileOutcome {
    CompileOutcome {
        glb: None,
        diagnostics: render_json(entry, &diags),
        stage,
    }
}

/// In-memory [`Loader`] backed by a string cache. Populated either with a
/// single entry (the [`compile`] shim) or with the editor's open tabs plus
/// the pre-fetched bodies of every reachable registry pin (the
/// [`compile_files`] entry). Cycle detection / dedup happens inside
/// `mogen-dsl`'s import walker against the [`LoadedFile::canonical`] we
/// emit — using the spec string directly is fine because each spec maps to
/// exactly one source in the cache.
struct JsLoader<'a> {
    cache: &'a HashMap<String, String>,
    /// The same `Map<string, Uint8Array>` of binary assets the export step
    /// reads textures out of. `mesh (src="…")` resolves from here too: in the
    /// browser there is no filesystem for the default `Loader::load_binary` to
    /// fall back to, so without this a scene referencing an external mesh
    /// could not be lowered at all.
    assets: &'a HashMap<PathBuf, Vec<u8>>,
}

impl<'a> Loader for JsLoader<'a> {
    fn load(&mut self, spec: &str, _base_dir: Option<&Path>) -> Result<LoadedFile> {
        match self.cache.get(spec) {
            Some(src) => Ok(LoadedFile {
                canonical: PathBuf::from(spec),
                source: src.clone(),
            }),
            None => Err(anyhow::anyhow!(
                "import \"{}\" — not present in the file map and \
                 fetch_dep didn't return it (this is a wasm prefetch bug)",
                spec
            )),
        }
    }

    /// Resolve a `use "@user/slug[@v]"` registry reference from the
    /// pre-populated cache. The cache is keyed by the verbatim token
    /// (`spec.raw`) — the same string the BFS prefetch passed to
    /// `fetch_dep` — so a hit here means the JS host already returned
    /// the source. The synthesised `canonical` matches the convention
    /// documented on the default trait impl in mogen-dsl: a stable
    /// `registry/<user>/<slug>/<version-or-latest>` PathBuf that won't
    /// collide with any real filesystem path the desktop FsLoader emits.
    fn load_registry(&mut self, spec: &RegistrySpec) -> Result<LoadedFile> {
        match self.cache.get(&spec.raw) {
            Some(src) => {
                let canonical = match spec.version {
                    Some(v) => PathBuf::from(format!(
                        "registry/{}/{}/{}",
                        spec.user, spec.slug, v
                    )),
                    None => PathBuf::from(format!(
                        "registry/{}/{}/latest",
                        spec.user, spec.slug
                    )),
                };
                Ok(LoadedFile {
                    canonical,
                    source: src.clone(),
                })
            }
            None => Err(anyhow::anyhow!(
                "use \"{}\" — fetch_dep did not supply source for this \
                 registry reference (wasm prefetch bug)",
                spec.raw
            )),
        }
    }

    /// Serve `mesh (src="….glb")` out of the binary asset map. The default
    /// impl resolves against the filesystem, which in a browser means the call
    /// fails no matter what the host supplied — so overriding here is what
    /// actually makes the seam worth having on this side.
    ///
    /// Keyed by the spec verbatim, matching the convention the export step
    /// already uses for `texture = "…"`: whatever path the `.mog` source
    /// writes is the key the host must supply.
    fn load_binary(&mut self, spec: &str, _base_dir: Option<&Path>) -> Result<Vec<u8>> {
        match self.assets.get(Path::new(spec)) {
            Some(bytes) => Ok(bytes.clone()),
            None => Err(anyhow::anyhow!(
                "mesh (src=\"{}\") — not present in the binary asset map; the \
                 browser has no filesystem to fall back to, so the host must \
                 pass these bytes to compile_files alongside its textures",
                spec
            )),
        }
    }
}

/// Drain a JS `Map<string, Uint8Array>` of texture bytes into a Rust
/// `HashMap<PathBuf, Vec<u8>>`. Accepts `null`/`undefined` (no textures)
/// for callers that want to skip the argument; otherwise the value must
/// be a `Map`, with string keys and `Uint8Array`/`ArrayBuffer` values.
fn drain_textures(value: &JsValue) -> Result<HashMap<PathBuf, Vec<u8>>, JsValue> {
    let mut out = HashMap::new();
    if value.is_null() || value.is_undefined() {
        return Ok(out);
    }
    let map: &Map = value.dyn_ref::<Map>().ok_or_else(|| {
        JsValue::from_str(
            "compile_files: textures must be a Map<string, Uint8Array> (or null)",
        )
    })?;
    let entries = map.entries();
    loop {
        let next = entries.next()?;
        if next.done() {
            break;
        }
        let pair: js_sys::Array = next.value().dyn_into()?;
        let k = pair.get(0).as_string().ok_or_else(|| {
            JsValue::from_str("compile_files: texture map keys must be strings")
        })?;
        let raw = pair.get(1);
        let bytes = if let Some(arr) = raw.dyn_ref::<js_sys::Uint8Array>() {
            arr.to_vec()
        } else if let Some(buf) = raw.dyn_ref::<js_sys::ArrayBuffer>() {
            js_sys::Uint8Array::new(buf).to_vec()
        } else {
            return Err(JsValue::from_str(&format!(
                "compile_files: texture '{}' value must be a Uint8Array or ArrayBuffer",
                k
            )));
        };
        out.insert(PathBuf::from(k), bytes);
    }
    Ok(out)
}

/// Drain a JS `Map<string, string>` into a Rust `HashMap`. Non-string keys
/// or values are a hard error — the JS caller is contracted to pass a
/// Map of filename → source pairs.
fn drain_files(map: &Map) -> Result<HashMap<String, String>, JsValue> {
    let mut out = HashMap::new();
    let entries = map.entries();
    loop {
        let next = entries.next()?;
        if next.done() {
            break;
        }
        let pair: js_sys::Array = next.value().dyn_into()?;
        let k = pair.get(0).as_string().ok_or_else(|| {
            JsValue::from_str("compile_files: file map keys must be strings")
        })?;
        let v = pair.get(1).as_string().ok_or_else(|| {
            JsValue::from_str("compile_files: file map values must be strings")
        })?;
        out.insert(k, v);
    }
    Ok(out)
}

/// BFS-walk every `import "..."` reachable from `entry`, awaiting
/// `fetch_dep` for any spec not already in `cache`, and inserting the
/// fetched source. Files that fail to parse during prefetch are passed
/// through unchanged — the main pipeline will surface the parse error
/// against the right tab.
async fn prefetch_imports(
    entry: &str,
    cache: &mut HashMap<String, String>,
    fetch_dep: &Function,
) -> Result<(), JsValue> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = vec![entry.to_string()];

    while let Some(spec) = queue.pop() {
        if !seen.insert(spec.clone()) {
            continue;
        }
        // Ensure the source is in cache.
        if !cache.contains_key(&spec) {
            let raw = fetch_dep
                .call1(&JsValue::NULL, &JsValue::from_str(&spec))
                .map_err(|e| {
                    JsValue::from_str(&format!(
                        "fetch_dep('{}') threw: {}",
                        spec,
                        js_err_to_string(&e)
                    ))
                })?;
            // Allow fetch_dep to return either a Promise or a string directly.
            let resolved = if JsCast::has_type::<Promise>(&raw) {
                JsFuture::from(raw.unchecked_into::<Promise>()).await?
            } else {
                raw
            };
            let src = resolved.as_string().ok_or_else(|| {
                JsValue::from_str(&format!(
                    "fetch_dep('{}') did not resolve to a string",
                    spec
                ))
            })?;
            cache.insert(spec.clone(), src);
        }
        // Parse to discover transitive imports + registry refs. Errors
        // here are silent — the main pipeline will report them with
        // proper file routing.
        if let Some(src) = cache.get(&spec) {
            if let Ok(ast) = mogen_dsl::parse(src) {
                enqueue_specs(&ast, &seen, &mut queue);
            }
        }
    }

    Ok(())
}

/// Walk `nodes` (and their children) and push every reachable import
/// spec or registry-use spec into `queue` if it isn't already seen.
///
/// Two kinds of nodes are interesting to the wasm prefetch:
///
/// - `import "path.mog"` — the literal path is the cache key
///   `JsLoader::load` will look up.
/// - `use "@user/slug[@v]"` — when the token parses as a registry spec
///   (via `parse_registry_spec`, the same parser the lowering walker
///   uses), the verbatim token is the cache key `JsLoader::load_registry`
///   will look up.
///
/// Other `use` nodes (local module instantiations like
/// `use "chair"`) are skipped: they reference a `module` declared in
/// the same compilation unit, so there's nothing to fetch.
///
/// The walk descends into `n.children` so a `use` nested inside a
/// `scene { ... }` body (or any other container) is also queued —
/// matching `mogen_dsl::module::imports::walk::collect_registry_refs`.
fn enqueue_specs(nodes: &[Node], seen: &HashSet<String>, queue: &mut Vec<String>) {
    for n in nodes {
        match n.kind.as_str() {
            "import" => {
                if let Some(child) = &n.name {
                    if !seen.contains(child) && !queue.contains(child) {
                        queue.push(child.clone());
                    }
                }
            }
            "use" => {
                if let Some(name) = &n.name {
                    if parse_registry_spec(name).is_some()
                        && !seen.contains(name)
                        && !queue.contains(name)
                    {
                        queue.push(name.clone());
                    }
                }
            }
            _ => {}
        }
        enqueue_specs(&n.children, seen, queue);
    }
}

/// Best-effort conversion of a JsValue error to a string for surfacing in
/// our own error messages. JS errors stringify via `toString`; non-Error
/// values fall back to `format!("{:?}")`.
fn js_err_to_string(e: &JsValue) -> String {
    e.as_string()
        .or_else(|| {
            js_sys::Reflect::get(e, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{:?}", e))
}

#[cfg(test)]
mod tests {
    //! Host-side tests for the plain-Rust bits of this crate. The full
    //! `compile_files` entry point can't be exercised from `cargo test`
    //! because it crosses the wasm-bindgen boundary; `wasm-pack test`
    //! covers that. The two pieces tested here — `JsLoader::load_registry`
    //! and `enqueue_specs` — were both bug sites that caused E0701 to
    //! surface on MoGHub `/new?edit=` for any model containing a
    //! `use "@user/slug"` directive, so they're worth a sanity check.
    use super::*;
    use mogen_dsl::parse;

    fn loader_cache(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn load_registry_returns_cached_source_with_synthetic_canonical_for_pinned_version() {
        let cache = loader_cache(&[("@alice/chair@3", "module chair {}")]);
        let mut loader = JsLoader {
            cache: &cache,
            assets: &HashMap::new(),
        };
        let spec = parse_registry_spec("@alice/chair@3").unwrap();
        let loaded = loader.load_registry(&spec).expect("registry hit");
        assert_eq!(loaded.source, "module chair {}");
        assert_eq!(loaded.canonical, PathBuf::from("registry/alice/chair/3"));
    }

    #[test]
    fn load_registry_canonical_for_unpinned_ref_is_latest() {
        let cache = loader_cache(&[("@alice/chair", "module chair {}")]);
        let mut loader = JsLoader {
            cache: &cache,
            assets: &HashMap::new(),
        };
        let spec = parse_registry_spec("@alice/chair").unwrap();
        let loaded = loader.load_registry(&spec).expect("registry hit");
        assert_eq!(loaded.canonical, PathBuf::from("registry/alice/chair/latest"));
    }

    /// `mesh (src=…)` must resolve out of the asset map. The trait default
    /// reads the filesystem, which in a browser cannot succeed — so a fall
    /// through to it is the bug, not a fallback.
    #[test]
    fn load_binary_serves_meshes_from_the_asset_map() {
        let cache = loader_cache(&[]);
        let assets: HashMap<PathBuf, Vec<u8>> =
            [(PathBuf::from("models/rock.glb"), b"glTF-ish".to_vec())]
                .into_iter()
                .collect();
        let mut loader = JsLoader {
            cache: &cache,
            assets: &assets,
        };
        assert_eq!(
            loader.load_binary("models/rock.glb", None).unwrap(),
            b"glTF-ish".to_vec()
        );

        let err = loader
            .load_binary("models/missing.glb", None)
            .expect_err("a miss must not reach the filesystem default");
        let msg = err.to_string();
        assert!(
            msg.contains("binary asset map"),
            "miss must name the map the host has to populate: {msg}"
        );
    }

    #[test]
    fn load_registry_errors_when_prefetch_missed_the_spec() {
        let cache = loader_cache(&[]);
        let mut loader = JsLoader {
            cache: &cache,
            assets: &HashMap::new(),
        };
        let spec = parse_registry_spec("@alice/chair@3").unwrap();
        let err = loader.load_registry(&spec).expect_err("must miss");
        // Must NOT surface the mogen-dsl default's E0701 message — that
        // would mean `load_registry` fell through to the trait default,
        // which is exactly the bug this patch fixes.
        let msg = err.to_string();
        assert!(
            !msg.contains("no registry-aware loader is installed"),
            "miss must not fall through to the default E0701 message: {msg}"
        );
        assert!(msg.contains("fetch_dep"), "miss message should name fetch_dep: {msg}");
    }

    /// Mirrors what `prefetch_imports` does between fetches: BFS the
    /// AST, queue every interesting spec, refuse to re-queue.
    fn collect_queue(source: &str) -> Vec<String> {
        let ast = parse(source).expect("test source must parse");
        let seen = HashSet::new();
        let mut queue: Vec<String> = Vec::new();
        enqueue_specs(&ast, &seen, &mut queue);
        queue
    }

    #[test]
    fn enqueue_specs_picks_up_top_level_import() {
        let q = collect_queue("import \"parts.mog\"");
        assert_eq!(q, vec!["parts.mog".to_string()]);
    }

    #[test]
    fn enqueue_specs_picks_up_registry_use() {
        let q = collect_queue("use \"@alice/chair@3\"");
        assert_eq!(q, vec!["@alice/chair@3".to_string()]);
    }

    #[test]
    fn enqueue_specs_ignores_local_named_use() {
        // `use "chair"` references a local `module chair` — nothing to
        // fetch. The lowering pass binds it from the in-source modules.
        let q = collect_queue("module chair {}\nuse \"chair\"");
        assert!(q.is_empty(), "queue should not contain a local use: {q:?}");
    }

    #[test]
    fn enqueue_specs_dedupes_when_same_ref_appears_twice() {
        let q = collect_queue("use \"@alice/chair@3\"\nuse \"@alice/chair@3\"");
        assert_eq!(q, vec!["@alice/chair@3".to_string()]);
    }

    #[test]
    fn enqueue_specs_skips_already_seen_specs() {
        let ast = parse("use \"@alice/chair@3\"").unwrap();
        let mut seen = HashSet::new();
        seen.insert("@alice/chair@3".to_string());
        let mut queue: Vec<String> = Vec::new();
        enqueue_specs(&ast, &seen, &mut queue);
        assert!(queue.is_empty(), "already-seen spec must not re-queue: {queue:?}");
    }
}

