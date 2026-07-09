//! Two passes over the AST that gather the names visible to references
//! elsewhere in the file: declared materials (so `mat="..."` references can
//! be checked), declared modules (so `use "..."` references can be
//! checked), and the contents of any top-level `import` declarations.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use mogen_core::Diagnostic;
use mogen_dsl::ast::Node;

pub(super) enum ImportResolution {
    Resolved,
    Skipped,
}

pub(super) fn merge_imported_names(
    ast: &[Node],
    base_dir: Option<&Path>,
    modules: &mut HashSet<String>,
    materials: &mut HashSet<String>,
    physics: &mut HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) -> ImportResolution {
    if base_dir.is_none() {
        return ImportResolution::Skipped;
    }
    match mogen_dsl::resolve_imports(ast, base_dir) {
        Ok(decls) => {
            for n in &decls {
                let Some(name) = &n.name else { continue };
                match n.kind.as_str() {
                    "module" => {
                        modules.insert(name.clone());
                    }
                    "material" => {
                        materials.insert(name.clone());
                    }
                    "physics" => {
                        physics.insert(name.clone());
                    }
                    _ => {}
                }
            }
            ImportResolution::Resolved
        }
        Err(e) => {
            // Surface the failure on whichever `import` node is in the AST.
            // Fall back to a no-span diagnostic if no top-level import is found.
            let span = ast
                .iter()
                .find(|n| n.kind == "import")
                .map(|n| n.span);
            let mut diag =
                Diagnostic::error("E0306", format!("import resolution failed: {e}"));
            if let Some(s) = span {
                diag = diag.with_span(s);
            }
            diags.push(diag);
            ImportResolution::Resolved
        }
    }
}

pub(super) fn collect_material_names(ast: &[Node], diags: &mut Vec<Diagnostic>) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut names = HashSet::new();
    let mut visit = |n: &Node, diags: &mut Vec<Diagnostic>| {
        if n.kind != "material" {
            return;
        }
        match &n.name {
            None => {
                diags.push(
                    Diagnostic::error("E0201", "material declaration requires a name")
                        .with_span(n.span),
                );
            }
            Some(name) => {
                if seen.contains_key(name) {
                    diags.push(
                        Diagnostic::warning(
                            "W0202",
                            format!("duplicate material name \"{name}\""),
                        )
                        .with_span(n.span),
                    );
                } else {
                    seen.insert(name.clone(), 0);
                    names.insert(name.clone());
                }
            }
        }
    };
    for n in ast {
        visit(n, diags);
        if n.kind == "scene" {
            for c in &n.children {
                visit(c, diags);
            }
        }
    }
    names
}

/// Gather declared `physics` substance names so `phys="..."` references can be
/// checked. Missing-name and duplicate diagnostics are emitted by `rules`
/// (E0210) and lowering respectively, so this pass only collects.
pub(super) fn collect_physics_names(ast: &[Node]) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut visit = |n: &Node| {
        if n.kind == "physics" {
            if let Some(name) = &n.name {
                names.insert(name.clone());
            }
        }
    };
    for n in ast {
        visit(n);
        if n.kind == "scene" {
            for c in &n.children {
                visit(c);
            }
        }
    }
    names
}

pub(super) fn collect_module_names(ast: &[Node], diags: &mut Vec<Diagnostic>) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut names = HashSet::new();
    // Seed with stdlib modules so `use "humanoid_torso" (...)` validates
    // without the user needing to redeclare them. lower() merges these
    // into the live registry too — keep the two in sync.
    for name in mogen_dsl::stdlib_registry().names() {
        names.insert(name.clone());
    }
    for n in ast {
        if n.kind != "module" {
            continue;
        }
        match &n.name {
            None => {
                diags.push(
                    Diagnostic::error("E0301", "module declaration requires a name")
                        .with_span(n.span),
                );
            }
            Some(name) => {
                if seen.contains_key(name) {
                    diags.push(
                        Diagnostic::error(
                            "E0302",
                            format!("duplicate module declaration \"{name}\""),
                        )
                        .with_span(n.span),
                    );
                } else {
                    seen.insert(name.clone(), 0);
                    names.insert(name.clone());
                }
            }
        }
    }
    names
}
