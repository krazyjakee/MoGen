use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use mogen_core::Span;

use crate::ast::{Expr, Node, Value};

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
}

/// Walk the top level of `ast` and lift every `module` declaration into a registry.
pub fn collect_modules(ast: &[Node]) -> Result<ModuleRegistry> {
    let mut reg = ModuleRegistry::default();
    for n in ast {
        if n.kind != "module" {
            continue;
        }
        let name = n
            .name
            .clone()
            .ok_or_else(|| anyhow!("module declaration requires a name"))?;
        let mut params = Vec::new();
        for (k, v) in &n.attrs {
            let default = match v {
                Value::Number(n) => Some(Expr::Num(*n)),
                Value::Expr(e) => Some(e.clone()),
                Value::Vec3(_) | Value::Vec3Expr(_) => {
                    bail!(
                        "module parameter `{}` must be a scalar default, not a vec3",
                        k
                    );
                }
                Value::List(_) | Value::ListExpr(_)
                | Value::ListVec3(_) | Value::ListPair(_) | Value::ListQuad(_) => {
                    bail!(
                        "module parameter `{}` must be a scalar default, not a list",
                        k
                    );
                }
                // String/ident defaults aren't meaningful for numeric `$param` substitution.
                Value::String(_) | Value::Ident(_) => {
                    bail!(
                        "module parameter `{}` default must be a number or expression",
                        k
                    );
                }
            };
            params.push(Param { name: k.clone(), default });
        }
        if reg.modules.contains_key(&name) {
            bail!("duplicate module declaration \"{name}\"");
        }
        reg.modules.insert(
            name.clone(),
            ModuleDef { name, params, body: n.children.clone(), span: n.span },
        );
    }
    Ok(reg)
}

/// Remove every top-level `module` node and expand every `use` node against `reg`.
/// Expansion is recursive: modules may invoke other modules.
pub fn expand_modules(ast: &[Node], reg: &ModuleRegistry) -> Result<Vec<Node>> {
    let mut out = Vec::with_capacity(ast.len());
    for n in ast {
        if n.kind == "module" {
            continue;
        }
        expand_node_into(n, reg, &Scope::default(), &mut Vec::new(), &mut out)?;
    }
    Ok(out)
}

#[derive(Default, Clone)]
struct Scope {
    bindings: Vec<(String, f32)>,
}

impl Scope {
    fn lookup(&self, name: &str) -> Option<f32> {
        self.bindings.iter().rfind(|(k, _)| k == name).map(|(_, v)| *v)
    }
}

fn expand_node_into(
    node: &Node,
    reg: &ModuleRegistry,
    scope: &Scope,
    stack: &mut Vec<String>,
    out: &mut Vec<Node>,
) -> Result<()> {
    if node.kind == "use" {
        expand_use(node, reg, scope, stack, out)?;
        return Ok(());
    }

    // Non-use: deep-clone with $-ref substitution, then recurse into children.
    let mut cloned = Node {
        kind: node.kind.clone(),
        name: node.name.clone(),
        attrs: node
            .attrs
            .iter()
            .map(|(k, v)| Ok((k.clone(), substitute_value(v, scope)?)))
            .collect::<Result<Vec<_>>>()?,
        children: Vec::new(),
        span: node.span,
        kind_span: node.kind_span,
    };
    for c in &node.children {
        expand_node_into(c, reg, scope, stack, &mut cloned.children)?;
    }
    out.push(cloned);
    Ok(())
}

fn expand_use(
    node: &Node,
    reg: &ModuleRegistry,
    scope: &Scope,
    stack: &mut Vec<String>,
    out: &mut Vec<Node>,
) -> Result<()> {
    let module_name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("`use` requires a module name, e.g. `use \"leg\" (...)`"))?;
    let def = reg
        .get(&module_name)
        .ok_or_else(|| anyhow!("unknown module \"{}\"", module_name))?;

    if stack.iter().any(|n| n == &module_name) {
        bail!(
            "recursive module expansion: {} -> {}",
            stack.join(" -> "),
            module_name
        );
    }

    // Resolve caller args (in the caller's scope) first, then fill in defaults.
    let mut supplied: HashMap<String, f32> = HashMap::new();
    for (k, v) in &node.attrs {
        let n = scalar_value(v, scope).ok_or_else(|| {
            anyhow!(
                "argument `{k}` for module \"{module_name}\" must be a number or expression"
            )
        })?;
        supplied.insert(k.clone(), n);
    }

    // Reject unknown args early so typos don't silently no-op.
    let param_names: Vec<&str> = def.params.iter().map(|p| p.name.as_str()).collect();
    for k in supplied.keys() {
        if !param_names.contains(&k.as_str()) {
            bail!(
                "module \"{}\" has no parameter `{}` (known: {:?})",
                module_name,
                k,
                param_names
            );
        }
    }

    let mut call_scope = Scope::default();
    for p in &def.params {
        let value = match supplied.get(&p.name) {
            Some(v) => *v,
            None => match &p.default {
                Some(e) => e
                    .eval(&|n| call_scope.lookup(n))
                    .ok_or_else(|| {
                        anyhow!(
                            "default for `{}` in module \"{}\" references an unbound parameter",
                            p.name,
                            module_name
                        )
                    })?,
                None => bail!(
                    "module \"{}\" missing required parameter `{}`",
                    module_name,
                    p.name
                ),
            },
        };
        call_scope.bindings.push((p.name.clone(), value));
    }

    stack.push(module_name);
    for body_node in &def.body {
        expand_node_into(body_node, reg, &call_scope, stack, out)?;
    }
    stack.pop();
    Ok(())
}

fn substitute_value(value: &Value, scope: &Scope) -> Result<Value> {
    match value {
        Value::Number(_)
        | Value::Vec3(_)
        | Value::String(_)
        | Value::Ident(_)
        | Value::List(_)
        | Value::ListVec3(_)
        | Value::ListPair(_)
        | Value::ListQuad(_) => Ok(value.clone()),
        Value::Expr(e) => Ok(substitute_expr(e, scope)?),
        Value::Vec3Expr(components) => {
            let resolved: Vec<Value> = components
                .iter()
                .map(|c| substitute_expr(c, scope))
                .collect::<Result<_>>()?;
            // If all three resolved to concrete Numbers, collapse to Vec3.
            if resolved.iter().all(|v| matches!(v, Value::Number(_))) {
                let mut arr = [0.0f32; 3];
                for (i, v) in resolved.iter().enumerate() {
                    if let Value::Number(n) = v {
                        arr[i] = *n;
                    }
                }
                Ok(Value::Vec3(arr))
            } else {
                // Some component still references an unbound $param. Keep as Vec3Expr.
                let mut out = [Expr::Num(0.0), Expr::Num(0.0), Expr::Num(0.0)];
                for (i, v) in resolved.iter().enumerate() {
                    out[i] = match v {
                        Value::Number(n) => Expr::Num(*n),
                        Value::Expr(e) => e.clone(),
                        _ => unreachable!(),
                    };
                }
                Ok(Value::Vec3Expr(out))
            }
        }
        Value::ListExpr(components) => {
            let resolved: Vec<Value> = components
                .iter()
                .map(|c| substitute_expr(c, scope))
                .collect::<Result<_>>()?;
            if resolved.iter().all(|v| matches!(v, Value::Number(_))) {
                Ok(Value::List(
                    resolved
                        .iter()
                        .map(|v| if let Value::Number(n) = v { *n } else { 0.0 })
                        .collect(),
                ))
            } else {
                Ok(Value::ListExpr(
                    resolved
                        .into_iter()
                        .map(|v| match v {
                            Value::Number(n) => Expr::Num(n),
                            Value::Expr(e) => e,
                            _ => unreachable!(),
                        })
                        .collect(),
                ))
            }
        }
    }
}

/// Substitute known `$param`s into an expression, folding to Number if fully resolved.
fn substitute_expr(e: &Expr, scope: &Scope) -> Result<Value> {
    match e.eval(&|n| scope.lookup(n)) {
        Some(n) => Ok(Value::Number(n)),
        None => Ok(Value::Expr(rewrite_expr(e, scope))),
    }
}

fn rewrite_expr(e: &Expr, scope: &Scope) -> Expr {
    match e {
        Expr::Num(_) => e.clone(),
        Expr::Param(n) => match scope.lookup(n) {
            Some(v) => Expr::Num(v),
            None => e.clone(),
        },
        Expr::Bin(a, op, b) => Expr::Bin(
            Box::new(rewrite_expr(a, scope)),
            *op,
            Box::new(rewrite_expr(b, scope)),
        ),
    }
}

/// Evaluate a Value to an f32 scalar (used for module arguments).
/// Accepts Number, Expr, or a 1-component vec3 trivially? — no, only scalars.
fn scalar_value(v: &Value, scope: &Scope) -> Option<f32> {
    match v {
        Value::Number(n) => Some(*n),
        Value::Expr(e) => e.eval(&|n| scope.lookup(n)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn expand(src: &str) -> Vec<Node> {
        let ast = parse(src).unwrap();
        let reg = collect_modules(&ast).unwrap();
        expand_modules(&ast, &reg).unwrap()
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
