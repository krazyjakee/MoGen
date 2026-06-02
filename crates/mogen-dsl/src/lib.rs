pub mod anim_lower;
pub mod ast;
pub mod attach;
pub mod conform;
pub mod lower;
pub mod meta;
pub mod module;
pub mod parser;
pub mod proc_schema;
pub mod skin_lower;
pub mod stdlib;

pub use ast::{BinOp, Expr, Node, Value};
pub use lower::{lower, lower_with_loader, lower_with_source};
pub use meta::{
    extract_meta, read_meta_attr, stamp_mogen_version, strip_legacy_seed_comments,
    upsert_meta_attr, upsert_meta_list_attr,
};
pub use module::{
    collect_local_import_files, collect_modules, expand_modules, parse_registry_spec,
    resolve_imports, resolve_imports_with_loader, FsLoader, LoadedFile, Loader, ModuleDef,
    ModuleRegistry, Param, ParamDefault, RegistrySpec,
};
pub use parser::parse;
pub use stdlib::stdlib_registry;

/// If `src` is a module-only file (e.g. a wizard per-object `.mog`), return a
/// rewritten source string that appends a transient `scene { use "X" () }`
/// so a previewer can render it standalone without writing back to disk.
///
/// Returns `None` when the heuristic doesn't match — caller falls through to
/// the unmodified source. The heuristic is intentionally narrow:
///
/// - exactly one top-level `module "X"` declaration
/// - no top-level `scene { … }` block
/// - no top-level `use "X" (...)` (already instantiated)
/// - the rest of the top level is only material / meta / lod_scale / import
///   (anything else means the user is doing something non-trivial — leave it
///   alone).
///
/// The synthesised `use` carries no parameters, so a module that requires
/// arguments without defaults will still fail to lower; that's intended —
/// the heuristic targets concrete, fully-defaulted modules.
pub fn synthesise_standalone_module_use(src: &str) -> Option<String> {
    let ast = parse(src).ok()?;
    let mut module_name: Option<String> = None;
    let mut module_count = 0usize;
    for node in &ast {
        match node.kind.as_str() {
            "module" => {
                module_count += 1;
                if module_count == 1 {
                    module_name = node.name.clone();
                }
            }
            "scene" => return None,
            "use" => return None,
            "material" | "meta" | "lod_scale" | "import" => {}
            _ => return None,
        }
    }
    if module_count != 1 {
        return None;
    }
    let name = module_name?;
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    let sep = if src.ends_with('\n') { "" } else { "\n" };
    Some(format!("{src}{sep}\nscene {{ use \"{escaped}\" () }}\n"))
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    #[test]
    fn single_module_synthesises_use() {
        let src = r#"module "chair" () { box "seat" (size=[1,0.1,1]) }"#;
        let rewritten = synthesise_standalone_module_use(src).expect("should rewrite");
        assert!(rewritten.contains(r#"scene { use "chair" () }"#));
        let scene = lower(&parse(&rewritten).unwrap()).unwrap();
        assert!(!scene.nodes.is_empty(), "module body should lower to nodes");
    }

    #[test]
    fn file_with_scene_is_left_alone() {
        let src = r#"module "chair" () { box "seat" (size=[1,0.1,1]) }
scene { use "chair" () }"#;
        assert!(synthesise_standalone_module_use(src).is_none());
    }

    #[test]
    fn file_with_top_level_use_is_left_alone() {
        let src = r#"module "chair" () { box "seat" (size=[1,0.1,1]) }
use "chair" ()"#;
        assert!(synthesise_standalone_module_use(src).is_none());
    }

    #[test]
    fn two_modules_left_alone() {
        let src = r#"module "a" () { box "x" (size=[1,1,1]) }
module "b" () { box "y" (size=[1,1,1]) }"#;
        assert!(synthesise_standalone_module_use(src).is_none());
    }

    #[test]
    fn module_with_top_level_material_still_rewritten() {
        let src = r#"material "wood" (color=[0.6,0.4,0.2])
module "chair" () { box "seat" (size=[1,0.1,1], mat="wood") }"#;
        let rewritten = synthesise_standalone_module_use(src).expect("should rewrite");
        assert!(rewritten.contains(r#"use "chair" ()"#));
    }

    #[test]
    fn top_level_geometry_left_alone() {
        let src = r#"module "chair" () { box "seat" (size=[1,0.1,1]) }
box "floor" (size=[4,0.05,4])"#;
        assert!(synthesise_standalone_module_use(src).is_none());
    }
}
