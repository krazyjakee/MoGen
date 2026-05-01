//! Modules + imports: parameterised sub-graphs (`module`/`use`) and
//! cross-file scene composition (`import`).
//!
//! Public surface:
//! - [`Param`], [`ModuleDef`], [`ModuleRegistry`] — registry data.
//! - [`collect_modules`] — lift `module` decls from the AST into a registry.
//! - [`expand_modules`] — strip `module`/`import` nodes and inline every `use`,
//!   substituting `$param` references and minting per-call frame ids.
//! - [`resolve_imports`] — load `import "x.mog"` files, lift their modules /
//!   materials / scenes, and rewrite texture paths to absolute.
//! - [`UseParents`] — frame ancestry returned alongside the expanded AST.
//!
//! The submodules carry the heavy lifting:
//! - [`collect`] — `module` lifting.
//! - [`expand`] — `use` expansion, `Scope`, expression substitution.
//! - [`imports`] — `import` resolution, texture rewriting, scene-as-module
//!   synthesis.

mod collect;
mod expand;
mod imports;

use std::collections::HashMap;

use mogen_core::Span;

use crate::ast::{Expr, Node};

pub use collect::collect_modules;
pub use expand::{expand_modules, UseParents};
pub use imports::resolve_imports;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Value;
    use crate::parser::parse;

    fn expand(src: &str) -> Vec<Node> {
        let ast = parse(src).unwrap();
        let reg = collect_modules(&ast).unwrap();
        expand_modules(&ast, &reg).unwrap().0
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
}
