//! Expand `use` invocations against a [`ModuleRegistry`], substituting
//! `$param` references and stripping `module` / `import` declarations from
//! the resulting AST.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

use crate::ast::{Expr, Node, Value};

use super::ModuleRegistry;

/// Map from each minted `use_id` to the `use_id` of the surrounding `use`
/// expansion (or `None` for the outermost). Lets attach/anim resolvers walk
/// "is this node reachable from this spec's frame?" — a spec at frame U sees
/// any node whose frame chain passes through U.
pub type UseParents = HashMap<u32, Option<u32>>;

/// Remove every top-level `module` and `import` node and expand every `use`
/// node against `reg`. Expansion is recursive: modules may invoke other modules.
///
/// Returns the expanded AST plus a map of every minted `use_id` to its parent
/// frame (or `None` if it was the outermost). Callers store this on the
/// `SceneGraph` so attach/anim resolution can walk frame ancestry — an attach
/// declared in module M with frame U must be able to see nodes brought in by
/// nested `use`s inside M (descendant frames of U).
pub fn expand_modules(ast: &[Node], reg: &ModuleRegistry) -> Result<(Vec<Node>, UseParents)> {
    let mut out = Vec::with_capacity(ast.len());
    let mut next_use_id: u32 = 1;
    let mut use_parents: UseParents = HashMap::new();
    for n in ast {
        if n.kind == "module" || n.kind == "import" {
            continue;
        }
        expand_node_into(
            n,
            reg,
            &Scope::default(),
            &mut Vec::new(),
            &mut out,
            None,
            &mut next_use_id,
            &mut use_parents,
        )?;
    }
    Ok((out, use_parents))
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
    current_use: Option<u32>,
    next_use: &mut u32,
    use_parents: &mut UseParents,
) -> Result<()> {
    if node.kind == "use" {
        expand_use(node, reg, scope, stack, out, current_use, next_use, use_parents)?;
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
        use_id: current_use,
        origin: node.origin.clone(),
    };
    for c in &node.children {
        expand_node_into(
            c,
            reg,
            scope,
            stack,
            &mut cloned.children,
            current_use,
            next_use,
            use_parents,
        )?;
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
    current_use: Option<u32>,
    next_use: &mut u32,
    use_parents: &mut UseParents,
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

    // Partition caller args. Anything matching a declared parameter binds into
    // the call scope (and must be a scalar); transform shortcuts (`pos`, `rot`,
    // `scale`, `x/y/z`, `rx/ry/rz`, `from/to`) that are *not* declared params
    // are pulled aside and applied as a wrapping group transform around the
    // expanded body — same effect as `group (pos=…) { use "m" () }` but
    // without the ceremony. Anything else is an unknown-parameter error.
    let param_names: Vec<&str> = def.params.iter().map(|p| p.name.as_str()).collect();
    let mut supplied: HashMap<String, f32> = HashMap::new();
    let mut wrapper_attrs: Vec<(String, Value)> = Vec::new();
    for (k, v) in &node.attrs {
        if param_names.contains(&k.as_str()) {
            let n = scalar_value(v, scope).ok_or_else(|| {
                anyhow!(
                    "argument `{k}` for module \"{module_name}\" must be a number or expression"
                )
            })?;
            supplied.insert(k.clone(), n);
        } else if is_wrapper_attr(k) {
            wrapper_attrs.push((k.clone(), substitute_value(v, scope)?));
        } else {
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

    // Every `use` mints its own frame so two instances of the same inner
    // module (e.g. five `use "pen"` calls inside `pen_pot`) keep their
    // internal attaches in separate buckets. The parent frame is recorded
    // in `use_parents` so attach/anim lookup can still see nodes brought in
    // by nested `use`s — an attach in frame U matches any node whose frame
    // chain passes through U.
    let id = *next_use;
    *next_use += 1;
    use_parents.insert(id, current_use);
    let body_use = Some(id);

    let mut wrapper_children: Vec<Node> = Vec::new();
    let target: &mut Vec<Node> = if wrapper_attrs.is_empty() {
        out
    } else {
        &mut wrapper_children
    };

    stack.push(module_name.clone());
    for body_node in &def.body {
        expand_node_into(
            body_node,
            reg,
            &call_scope,
            stack,
            target,
            body_use,
            next_use,
            use_parents,
        )?;
    }
    stack.pop();

    if !wrapper_attrs.is_empty() {
        out.push(Node {
            kind: "group".to_string(),
            name: Some(module_name),
            attrs: wrapper_attrs,
            children: wrapper_children,
            span: node.span,
            kind_span: node.kind_span,
            use_id: body_use,
            origin: node.origin.clone(),
        });
    }
    Ok(())
}

/// Caller-supplied attrs on `use` that are not module parameters but should
/// pass through to a synthesized wrapping `group`. Covers the transform
/// shortcuts already accepted by every other node kind (see
/// [`crate::lower::helpers::transform_from_attrs`]) plus the `collider`
/// attribute, so `use "desk" (pos=…, collider="aabb")` works the same as
/// wrapping the use in a group.
fn is_wrapper_attr(k: &str) -> bool {
    matches!(
        k,
        "pos"
            | "rot"
            | "scale"
            | "x"
            | "y"
            | "z"
            | "rx"
            | "ry"
            | "rz"
            | "from"
            | "to"
            | "collider"
    )
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
        | Value::ListQuad(_)
        | Value::ListString(_) => Ok(value.clone()),
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
