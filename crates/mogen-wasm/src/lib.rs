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
//! ## Still disabled
//!
//! - **Textures**: `mogen-export`'s `textures` feature pulls in `image` and
//!   `oxipng` (libdeflate-sys), which don't cross-compile to wasm32. The
//!   GLB ships with PBR factors only — no embedded baseColor / normal /
//!   metallic-roughness images.
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
use mogen_dsl::{LoadedFile, Loader};
use mogen_export::{build_glb_with_options, ExportOptions};
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
    compile_with_cache(entry, files)
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
/// Async because `fetch_dep` returns a `Promise`; the implementation walks
/// the import graph BFS-style and awaits each missing spec before recursing
/// so that the synchronous resolver inside `mogen-dsl` can read every
/// reachable file out of an in-memory cache.
#[wasm_bindgen]
pub async fn compile_files(
    entry: String,
    files: Map,
    fetch_dep: Function,
) -> Result<CompileOutcome, JsValue> {
    let mut cache = drain_files(&files)?;
    if !cache.contains_key(&entry) {
        return Err(JsValue::from_str(&format!(
            "compile_files: entry '{}' is not present in files map",
            entry
        )));
    }
    prefetch_imports(&entry, &mut cache, &fetch_dep).await?;
    Ok(compile_with_cache(&entry, cache))
}

/// Run the full compile pipeline against an already-populated file cache.
/// Shared by [`compile`] (single-file) and [`compile_files`] (multi-file
/// after async prefetch).
fn compile_with_cache(entry: &str, cache: HashMap<String, String>) -> CompileOutcome {
    let mut loader = JsLoader { cache: &cache };

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

    // Export. Textures still off (no fs, no oxipng); CSG-backed merge is on.
    let opts = ExportOptions {
        include_animations: true,
        include_textures: false,
        merge_sibling_meshes: true,
    };
    match build_glb_with_options(&scene, &opts, |_| {}) {
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
        // Parse to discover transitive imports. Errors here are silent —
        // the main pipeline will report them with proper file routing.
        if let Some(src) = cache.get(&spec) {
            if let Ok(ast) = mogen_dsl::parse(src) {
                for n in &ast {
                    if n.kind == "import" {
                        if let Some(child) = n.name.clone() {
                            if !seen.contains(&child) {
                                queue.push(child);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
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

