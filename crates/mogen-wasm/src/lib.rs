//! WebAssembly bindings for the MoGen pipeline.
//!
//! Exposes a single `compile(source)` entry point that runs the full
//! parse → validate → lower → validate → export chain in the browser and
//! returns either GLB bytes or a JSON diagnostics array. The `mogen-export`
//! `merge` and `textures` features and the `mogen-geom` `csg` feature are
//! all disabled here (see the dependency tree in `Cargo.toml`) — none of the
//! C++/threading toolchains they need are available on
//! `wasm32-unknown-unknown`. CSG ops are caught up-front by walking the AST
//! and flagged as a clear diagnostic instead of reaching a panic stub.

use mogen_core::{Diagnostic, Severity, Span};
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

    // CSG check — the wasm build can't link the manifold C++ library, so we
    // intercept these node kinds before lowering would invoke a panic stub.
    if let Some(span) = find_csg_node(&ast) {
        let mut diag = Diagnostic::error(
            "EWASM01",
            "CSG operations (`union` / `difference` / `intersect`) are not supported in the web build. Use the desktop `mogen` CLI for CSG.".to_string(),
        );
        diag.span = Some(span);
        return fail("parse", vec![diag]);
    }

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

    // Export. Disable both options the wasm build can't honour: textures
    // (no fs to read source PNGs) and merge (no CSG to back the union).
    let opts = ExportOptions {
        include_animations: true,
        include_textures: false,
        merge_sibling_meshes: false,
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

/// Walk the AST looking for a CSG node. Returns the span of the first one,
/// for the diagnostic. Recurses into children since CSG can be nested under
/// `group`/`scene`/etc. Modules (`use`) are not expanded yet at this point;
/// users would only hit a stdlib CSG op via expansion which is rare and
/// would surface as a panic at lower time — acceptable given how unusual
/// that path is.
fn find_csg_node(nodes: &[mogen_dsl::Node]) -> Option<Span> {
    for n in nodes {
        if matches!(n.kind.as_str(), "union" | "difference" | "intersect") {
            return Some(n.span);
        }
        if let Some(s) = find_csg_node(&n.children) {
            return Some(s);
        }
    }
    None
}
