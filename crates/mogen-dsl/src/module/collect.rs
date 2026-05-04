//! Lift top-level `module` declarations into a [`ModuleRegistry`].

use anyhow::{anyhow, bail, Result};

use crate::ast::{Expr, Node, Value};

use super::{ModuleDef, ModuleRegistry, Param};

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
                | Value::ListVec3(_) | Value::ListPair(_) | Value::ListQuad(_)
                | Value::ListString(_) => {
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
