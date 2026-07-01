//! Lift top-level `module` declarations into a [`ModuleRegistry`].

use anyhow::{anyhow, bail, Result};

use crate::ast::{Expr, Node, Value};

use super::{ModuleDef, ModuleRegistry, Param, ParamDefault};

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
                Value::Number(n) => Some(ParamDefault::Scalar(Expr::Num(*n))),
                Value::Expr(e) => Some(ParamDefault::Scalar(e.clone())),
                Value::Vec3(arr) => Some(ParamDefault::Vec3(*arr)),
                Value::Vec3Expr(components) => {
                    // Only accept fully-constant vec3 defaults; expressions in
                    // a default would need a binding scope that doesn't exist
                    // at collect time. Authors who need dynamic defaults can
                    // pass the value at the `use` site.
                    let mut arr = [0.0f32; 3];
                    for (i, c) in components.iter().enumerate() {
                        match c.eval_const() {
                            Some(n) => arr[i] = n,
                            None => bail!(
                                "module parameter `{}` vec3 default must be a constant (component {} references a parameter)",
                                k,
                                i
                            ),
                        }
                    }
                    Some(ParamDefault::Vec3(arr))
                }
                Value::List(_) | Value::ListExpr(_)
                | Value::ListVec3(_) | Value::ListPair(_) | Value::ListQuad(_)
                | Value::ListString(_) | Value::FaceList(_) => {
                    bail!(
                        "module parameter `{}` default must be a number, expression, or vec3 (lists are not supported as parameter defaults)",
                        k
                    );
                }
                // String/ident defaults aren't meaningful for `$param` substitution.
                Value::String(_) | Value::Ident(_) => {
                    bail!(
                        "module parameter `{}` default must be a number, expression, or vec3",
                        k
                    );
                }
                Value::Gradient(_) => {
                    bail!(
                        "module parameter `{}` default cannot be a gradient — gradients can only appear directly on `material` attributes",
                        k
                    );
                }
            };
            params.push(Param { name: k.clone(), default });
        }
        if reg.contains(&name) {
            bail!("duplicate module declaration \"{name}\"");
        }
        reg.insert(ModuleDef {
            name: name.clone(),
            params,
            body: n.children.clone(),
            span: n.span,
            doc: None,
        });
    }
    Ok(reg)
}
