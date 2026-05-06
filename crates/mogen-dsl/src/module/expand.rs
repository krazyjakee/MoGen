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

/// Maximum number of iterations a single `for` loop may emit. Caps a
/// user-supplied `from`/`to`/`step` combination so a stray
/// `for (from=0, to=1000000000)` doesn't grind the lowering loop and the
/// host. 100k is well over any plausible authored fan-out (a fence with
/// 10k pickets is already extreme) while remaining well under the OOM
/// threshold for sibling-allocated `Node` values.
const FOR_LOOP_ITERATION_CAP: usize = 100_000;

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
    let filtered: Vec<&Node> = ast
        .iter()
        .filter(|n| n.kind != "module" && n.kind != "import")
        .collect();
    expand_children_into(
        &filtered,
        reg,
        &Scope::default(),
        &mut Vec::new(),
        &mut out,
        None,
        &mut next_use_id,
        &mut use_parents,
    )?;
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
        name: match &node.name {
            Some(s) => Some(interpolate_string(s, scope)?),
            None => None,
        },
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
    let child_refs: Vec<&Node> = node.children.iter().collect();
    expand_children_into(
        &child_refs,
        reg,
        scope,
        stack,
        &mut cloned.children,
        current_use,
        next_use,
        use_parents,
    )?;
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
    let body_refs: Vec<&Node> = def.body.iter().collect();
    expand_children_into(
        &body_refs,
        reg,
        &call_scope,
        stack,
        target,
        body_use,
        next_use,
        use_parents,
    )?;
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
/// wrapping the use in a group. `cast_shadow` rides along for the same
/// reason — toggling shadow casting on a single import shouldn't require
/// editing the imported file.
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
            | "cast_shadow"
            | "lod"
    )
}

fn substitute_value(value: &Value, scope: &Scope) -> Result<Value> {
    match value {
        Value::Number(_)
        | Value::Vec3(_)
        | Value::List(_)
        | Value::ListVec3(_)
        | Value::ListPair(_)
        | Value::ListQuad(_) => Ok(value.clone()),
        // Strings and idents (and lists of strings) get scope-aware
        // `$ident` / `${ident}` interpolation so authors can write
        // `name "leg_$i"` inside a `for` body.
        Value::String(s) => Ok(Value::String(interpolate_string(s, scope)?)),
        Value::Ident(s) => Ok(Value::Ident(interpolate_string(s, scope)?)),
        Value::ListString(items) => Ok(Value::ListString(
            items
                .iter()
                .map(|s| interpolate_string(s, scope))
                .collect::<Result<_>>()?,
        )),
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

/// Walk a list of sibling nodes, expanding each one. Splits out from
/// `expand_node_into` so control-flow constructs (`if`/`else`, `for`) can
/// peek at the surrounding sibling list — `if` needs to consume a following
/// `else`, and `for` needs to expand its body N times. Plain non-control
/// nodes route to `expand_node_into` unchanged.
#[allow(clippy::too_many_arguments)]
fn expand_children_into(
    children: &[&Node],
    reg: &ModuleRegistry,
    scope: &Scope,
    stack: &mut Vec<String>,
    out: &mut Vec<Node>,
    current_use: Option<u32>,
    next_use: &mut u32,
    use_parents: &mut UseParents,
) -> Result<()> {
    let mut i = 0;
    while i < children.len() {
        let c = children[i];
        match c.kind.as_str() {
            "if" => {
                let cond = eval_cond(c, scope)?;
                let next_is_else = children
                    .get(i + 1)
                    .map(|n| n.kind == "else")
                    .unwrap_or(false);
                let chosen_children: &Vec<Node> = if cond {
                    &c.children
                } else if next_is_else {
                    &children[i + 1].children
                } else {
                    // No else and cond was false — emit nothing, advance.
                    i += 1;
                    continue;
                };
                let refs: Vec<&Node> = chosen_children.iter().collect();
                expand_children_into(
                    &refs,
                    reg,
                    scope,
                    stack,
                    out,
                    current_use,
                    next_use,
                    use_parents,
                )?;
                i += 1 + (next_is_else as usize);
            }
            "else" => {
                bail!(
                    "`else` must immediately follow an `if` block (got `else` at top level or after a non-`if` sibling)"
                );
            }
            "for" => {
                let (var, start, end, step) = parse_for_attrs(c, scope)?;
                // Iteration order matches Python's `range`: open-ended on
                // `end`, signed step. `step==0` was rejected at parse_for_attrs
                // so the loop always terminates. Iteration variable is
                // computed via `start + i * step` rather than accumulated, so
                // a fractional `step` doesn't drift across many iterations.
                let mut iter_count: usize = 0;
                loop {
                    let k = start + (iter_count as f32) * step;
                    let in_range = (step > 0.0 && k < end) || (step < 0.0 && k > end);
                    if !in_range {
                        break;
                    }
                    if iter_count >= FOR_LOOP_ITERATION_CAP {
                        bail!(
                            "`for` loop exceeded iteration cap of {} (var=`{}`, from={}, to={}, step={}). Lower the range or use nested modules to fan out work.",
                            FOR_LOOP_ITERATION_CAP, var, start, end, step
                        );
                    }
                    let mut child_scope = scope.clone();
                    child_scope.bindings.push((var.clone(), k));
                    let refs: Vec<&Node> = c.children.iter().collect();
                    expand_children_into(
                        &refs,
                        reg,
                        &child_scope,
                        stack,
                        out,
                        current_use,
                        next_use,
                        use_parents,
                    )?;
                    iter_count += 1;
                }
                i += 1;
            }
            _ => {
                expand_node_into(
                    c,
                    reg,
                    scope,
                    stack,
                    out,
                    current_use,
                    next_use,
                    use_parents,
                )?;
                i += 1;
            }
        }
    }
    Ok(())
}

/// Evaluate the `cond=` attribute of an `if` block in the given scope.
/// Truthy = non-zero. Missing/non-numeric/unbound `cond` is a hard error
/// — silent falsy interpretation would mask typos.
fn eval_cond(node: &Node, scope: &Scope) -> Result<bool> {
    let v = node
        .attr("cond")
        .ok_or_else(|| anyhow!("`if` block requires a `cond=` attribute"))?;
    let n = scalar_value(v, scope)
        .ok_or_else(|| anyhow!("`if` cond must evaluate to a number"))?;
    Ok(n != 0.0)
}

/// Read the four control attributes from a `for` block, validate, and
/// return `(var, start, end, step)`. `var` accepts both quoted strings
/// (`var="i"`) and bare idents (`var=i`); `from`/`to`/`step` must resolve
/// to numbers in the current scope.
fn parse_for_attrs(node: &Node, scope: &Scope) -> Result<(String, f32, f32, f32)> {
    let var = node
        .attr("var")
        .ok_or_else(|| anyhow!("`for` requires a `var=` attribute naming the loop binding"))?;
    let var = match var {
        Value::String(s) | Value::Ident(s) => s.clone(),
        _ => bail!("`for` var must be a string or identifier (e.g. `var=\"i\"` or `var=i`)"),
    };
    if var.is_empty() || var.contains(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        bail!("`for` var must be alphanumeric/underscore identifier characters (got `{var}`)");
    }
    let from = node
        .attr("from")
        .ok_or_else(|| anyhow!("`for` requires a `from=` attribute"))?;
    let from = scalar_value(from, scope)
        .ok_or_else(|| anyhow!("`for` from must evaluate to a number"))?;
    let to = node
        .attr("to")
        .ok_or_else(|| anyhow!("`for` requires a `to=` attribute"))?;
    let to = scalar_value(to, scope)
        .ok_or_else(|| anyhow!("`for` to must evaluate to a number"))?;
    let step = match node.attr("step") {
        Some(v) => scalar_value(v, scope)
            .ok_or_else(|| anyhow!("`for` step must evaluate to a number"))?,
        None => 1.0,
    };
    if step == 0.0 {
        bail!("`for` step must not be zero — would loop forever");
    }
    Ok((var, from, to, step))
}

/// Replace `$name` and `${name}` with their scope value formatted as a
/// short string. Integer-valued bindings render without a decimal point;
/// non-integer bindings use Rust's default float-to-string. Unrecognised
/// `$names` are left literal so authors can include the dollar sign in
/// names that don't reference scope variables.
fn interpolate_string(s: &str, scope: &Scope) -> Result<String> {
    if !s.contains('$') {
        return Ok(s.to_string());
    }
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'$' {
            out.push(b as char);
            i += 1;
            continue;
        }
        // Either `${name}` or `$name`.
        let (name, consumed) = if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // Walk to the matching `}`. Missing `}` is an error.
            let start = i + 2;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'}' {
                end += 1;
            }
            if end >= bytes.len() {
                bail!("unterminated `${{` in string literal: \"{s}\"");
            }
            (&s[start..end], end + 1 - i)
        } else {
            // Walk while the next byte is an ident-continuation char.
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() {
                let c = bytes[end];
                if c.is_ascii_alphanumeric() || c == b'_' {
                    end += 1;
                } else {
                    break;
                }
            }
            if end == start {
                // Bare `$` not followed by an identifier — leave it literal.
                out.push('$');
                i += 1;
                continue;
            }
            (&s[start..end], end - i)
        };
        match scope.lookup(name) {
            Some(v) => out.push_str(&format_scope_value(v)),
            None => {
                // Leave unrecognised refs literal so a `$` that doesn't
                // reference a binding (e.g. printing instructions) survives.
                out.push_str(&s[i..i + consumed]);
            }
        }
        i += consumed;
    }
    Ok(out)
}

fn format_scope_value(v: f32) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1.0e9 {
        // Integer-valued binding (the typical `for` loop var) — render
        // without trailing `.0` so `"leg_$i"` becomes `"leg_3"` not
        // `"leg_3.0"`.
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}
