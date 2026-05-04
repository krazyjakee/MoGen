//! WebAssembly bindings for the MoGen pipeline.
//!
//! Exposes a single `compile(source)` entry point that runs the full
//! parse → validate → lower → validate → export chain in the browser and
//! returns either GLB bytes or a JSON diagnostics array.
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
//! - **External `use "file.mog"`**: no `std::fs`, so top-level imports of
//!   on-disk DSL files don't resolve.
//!
//! ## Caveats inherited from `unstable-wasm-uu`
//!
//! Compiled `-fno-exceptions`: implicit STL throws (e.g. `bad_alloc` on
//! out-of-memory) become unrecoverable wasm traps rather than panics the
//! JS host can catch. The browser tab can recover by reloading.

use mogen_core::{Diagnostic, Severity};
use mogen_export::{build_glb_with_options, ExportOptions};
use mogen_validate::{render_json, validate_ast, validate_graph};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Result of a single `compile()` call. JS reads the `glb` getter (a
/// `Uint8Array`, or `null` on failure) and parses `diagnostics` as
/// line-delimited JSON — same format the desktop CLI emits with
/// `mogen check --json`.
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

#[wasm_bindgen]
pub fn compile(source: &str) -> CompileOutcome {
    // Parse.
    let ast = match mogen_dsl::parse(source) {
        Ok(a) => a,
        Err(e) => {
            return fail(
                "parse",
                vec![Diagnostic::error("E0001", format!("parse: {e}"))],
            );
        }
    };

    // AST validation.
    let mut diags = validate_ast(&ast);
    if diags.iter().any(|d| matches!(d.severity, Severity::Error)) {
        return fail("validate_ast", diags);
    }

    // Lower.
    let scene = match mogen_dsl::lower(&ast) {
        Ok(s) => s,
        Err(e) => {
            diags.push(Diagnostic::error("E0701", format!("lowering: {e}")));
            return fail("lower", diags);
        }
    };

    // Graph validation.
    diags.extend(validate_graph(&scene));
    if diags.iter().any(|d| matches!(d.severity, Severity::Error)) {
        return fail("validate_graph", diags);
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
            diagnostics: render_json("scene.mog", &diags),
            stage: "ok",
        },
        Err(e) => {
            diags.push(Diagnostic::error("E0900", format!("export: {e}")));
            fail("export", diags)
        }
    }
}

fn fail(stage: &'static str, diags: Vec<Diagnostic>) -> CompileOutcome {
    CompileOutcome {
        glb: None,
        diagnostics: render_json("scene.mog", &diags),
        stage,
    }
}
