//! AST validation: walks the parsed AST and emits `Diagnostic`s for unknown
//! kinds, unknown/typo'd attributes, type mismatches, missing required
//! attrs, unknown references, and structural rules. Entry points are
//! [`validate_ast`] and [`validate_ast_with_source`]; the bulk of the
//! schema lives in [`schema`], with per-kind structural rules in [`rules`]
//! and name collection / import resolution in [`collect`].

mod collect;
mod rules;
mod schema;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::Path;

use mogen_core::Diagnostic;
use mogen_dsl::ast::Node;

pub use schema::{
    attrs_for_kind, common_attrs_for_kind, DECAL_COMMON_ATTRS, GEOMETRY_COMMON_ATTRS, KNOWN_KINDS,
    LIGHT_COMMON_ATTRS, TRANSFORM_COMMON_ATTRS,
};

use collect::{collect_material_names, collect_module_names, merge_imported_names, ImportResolution};
use rules::check_anim_required;
use schema::{as_string_or_ident, attr_type, value_kind, value_matches};

pub fn validate_ast(ast: &[Node]) -> Vec<Diagnostic> {
    validate_ast_with_source(ast, None)
}

/// Like `validate_ast`, but also resolves top-level `import` declarations
/// relative to `base_dir` so `use "<imported_module>"` references validate
/// without false-positive "unknown module" diagnostics. Pass the directory
/// of the `.mog` file being validated (typically `path.parent()` for the
/// file passed to `mogen check`/`mogen build`). If `base_dir` is `None`
/// and the file contains imports, the unknown-module check is skipped to
/// avoid spurious diagnostics.
pub fn validate_ast_with_source(ast: &[Node], base_dir: Option<&Path>) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut materials = collect_material_names(ast, &mut diags);
    let mut modules = collect_module_names(ast, &mut diags);
    let has_imports = ast.iter().any(|n| n.kind == "import");
    let suppress_unknown_module = match merge_imported_names(
        ast,
        base_dir,
        &mut modules,
        &mut materials,
        &mut diags,
    ) {
        ImportResolution::Resolved => false,
        ImportResolution::Skipped => has_imports,
    };
    for n in ast {
        walk(n, &materials, &modules, suppress_unknown_module, &mut diags);
    }
    diags
}

fn walk(
    n: &Node,
    materials: &HashSet<String>,
    modules: &HashSet<String>,
    suppress_unknown_module: bool,
    diags: &mut Vec<Diagnostic>,
) {
    check_kind(n, diags);

    match n.kind.as_str() {
        // `module` and `use` carry user-defined attr names (params/args); skip the
        // closed attr vocabulary check. We still validate specific constraints below.
        "module" => {
            // Parameter defaults must be numeric scalars; the collector enforces this,
            // but we also reject e.g. `module "leg" (color="red")` here with a clearer
            // diagnostic than lowering would produce.
            for (k, v) in &n.attrs {
                if !matches!(v, mogen_dsl::ast::Value::Number(_) | mogen_dsl::ast::Value::Expr(_)) {
                    diags.push(
                        Diagnostic::error(
                            "E0303",
                            format!(
                                "module parameter `{}` default must be a number or expression",
                                k
                            ),
                        )
                        .with_span(n.span),
                    );
                }
            }
        }
        "use" => {
            if let Some(name) = &n.name {
                if !modules.contains(name) && !suppress_unknown_module {
                    diags.push(
                        Diagnostic::error(
                            "E0304",
                            format!("unknown module \"{}\"", name),
                        )
                        .with_span(n.span),
                    );
                }
            } else {
                diags.push(
                    Diagnostic::error(
                        "E0305",
                        "`use` requires a module name, e.g. `use \"leg\" (...)`",
                    )
                    .with_span(n.span),
                );
            }
        }
        "import" => {
            if n.name.is_none() {
                diags.push(
                    Diagnostic::error(
                        "E0307",
                        "`import` requires a quoted file path, e.g. `import \"shared.mog\"`",
                    )
                    .with_span(n.span),
                );
            }
            for (k, v) in &n.attrs {
                if k != "as" {
                    diags.push(
                        Diagnostic::error(
                            "E0308",
                            format!("`import` accepts only `(as=<ident>)`; unknown attribute `{k}`"),
                        )
                        .with_span(n.span),
                    );
                    continue;
                }
                if !matches!(v, mogen_dsl::ast::Value::Ident(_) | mogen_dsl::ast::Value::String(_)) {
                    diags.push(
                        Diagnostic::error(
                            "E0308",
                            "`import (as=…)` expects an identifier, e.g. `(as=chair)`",
                        )
                        .with_span(n.span),
                    );
                }
            }
            if !n.children.is_empty() {
                diags.push(
                    Diagnostic::error(
                        "E0309",
                        "`import` does not accept a body block",
                    )
                    .with_span(n.span),
                );
            }
        }
        _ if KNOWN_KINDS.contains(&n.kind.as_str()) => {
            check_attrs(n, materials, diags);
            check_anim_required(n, diags);
        }
        _ => {}
    }

    for c in &n.children {
        walk(c, materials, modules, suppress_unknown_module, diags);
    }
}

fn check_kind(n: &Node, diags: &mut Vec<Diagnostic>) {
    if !KNOWN_KINDS.contains(&n.kind.as_str()) {
        diags.push(
            Diagnostic::error(
                "E0101",
                format!("unknown node kind \"{}\"", n.kind),
            )
            .with_span(n.kind_span),
        );
    }
}

fn check_attrs(n: &Node, materials: &HashSet<String>, diags: &mut Vec<Diagnostic>) {
    let allowed = attrs_for_kind(&n.kind);
    let common = common_attrs_for_kind(&n.kind);
    for (k, v) in &n.attrs {
        if !allowed.contains(&k.as_str()) && !common.contains(&k.as_str()) {
            diags.push(
                Diagnostic::warning(
                    "W0102",
                    format!("attribute \"{}\" is not used by `{}`", k, n.kind),
                )
                .with_span(n.span),
            );
            continue;
        }
        if let Some(expected) = attr_type(&n.kind, k) {
            if !value_matches(v, expected) {
                diags.push(
                    Diagnostic::error(
                        "E0103",
                        format!(
                            "attribute \"{}\" on `{}` expects {}, got {}",
                            k,
                            n.kind,
                            expected,
                            value_kind(v)
                        ),
                    )
                    .with_span(n.span),
                );
            }
        }
        if k == "mat" {
            if let Some(name) = as_string_or_ident(v) {
                if !materials.contains(name) {
                    diags.push(
                        Diagnostic::error(
                            "E0104",
                            format!("unknown material \"{}\"", name),
                        )
                        .with_span(n.span),
                    );
                }
            }
        }
        if k == "bind" && n.attr("skin").is_none() {
            diags.push(
                Diagnostic::warning(
                    "W0105",
                    "`bind=\"...\"` has no effect without a `skin=\"...\"` attribute",
                )
                .with_span(n.span),
            );
        }
    }
}
