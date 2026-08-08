//! Hoist `shader "<name>" { … }` declarations into the graph.
//!
//! Mirrors [`super::material`]: a recursive collector dedupes by `(name, origin)`
//! and each declaration becomes a [`ShaderDecl`] on the graph. Materials refer to
//! shaders by name via `shader="<name>"`; resolution happens later (export /
//! Studio), so this pass only records declarations — it does not check that a
//! referencing material exists.

use anyhow::{anyhow, bail, Result};

use mogen_core::{
    shader, SceneGraph, ShaderDecl, ShaderParamDef, ShaderParamType, ShaderParamValue,
};

use crate::ast::{Node, Value};

pub(super) fn collect_shaders(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    for n in ast {
        collect_shaders_recursive(n, graph)?;
    }
    Ok(())
}

/// Walk the whole subtree so a `shader` declared inside a wrapping `group`
/// (e.g. an inlined module body) is still discoverable. Dedupe by
/// `(name, origin)` keeps repeated walks idempotent.
fn collect_shaders_recursive(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    if node.kind == "shader" {
        register_shader(node, graph)?;
    }
    for c in &node.children {
        collect_shaders_recursive(c, graph)?;
    }
    Ok(())
}

fn register_shader(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    let name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("shader requires a name, e.g. `shader \"ripple\" {{ … }}`"))?;
    // Dedupe by `(name, origin)` on the same rules as materials: two decls of the
    // same name in one file collapse to the first; same name across imported
    // files stay distinct and are addressable via `find_shader_scoped`.
    if graph
        .shaders
        .iter()
        .any(|s| s.name == name && s.origin.as_deref() == node.origin.as_deref())
    {
        return Ok(());
    }

    let source = node.attr_string("source").ok_or_else(|| {
        anyhow!("shader \"{name}\" requires a `source=` path, e.g. `source=\"shaders/ripple.glsl\"`")
    })?;
    let mut decl = ShaderDecl::new(&name, source);

    for c in &node.children {
        if c.kind != "param" {
            continue;
        }
        let pname = c.name.clone().ok_or_else(|| {
            anyhow!("param in shader \"{name}\" requires a name, e.g. `param \"speed\" (type=float)`")
        })?;
        let ty_str = c.attr_string("type").ok_or_else(|| {
            anyhow!("param \"{pname}\" in shader \"{name}\" requires a `type=` (float, vec2, vec3, vec4, or color)")
        })?;
        let ty = ShaderParamType::parse(ty_str).ok_or_else(|| {
            anyhow!("param \"{pname}\" in shader \"{name}\" has unknown type \"{ty_str}\" — expected float, vec2, vec3, vec4, or color")
        })?;
        let default = match c.attr("default") {
            Some(v) => {
                let pv = value_to_param(v).ok_or_else(|| {
                    anyhow!("param \"{pname}\" default in shader \"{name}\" is not a valid value")
                })?;
                if !pv.matches(ty) {
                    bail!("param \"{pname}\" default in shader \"{name}\" does not match its `type={ty_str}`");
                }
                Some(pv)
            }
            None => None,
        };
        decl.params.push(ShaderParamDef {
            name: pname,
            ty,
            default,
        });
    }

    decl.origin = node.origin.clone();
    graph.add_shader(decl);
    Ok(())
}

/// Seed the built-in shader presets (currently just `water`) unless the user
/// declared their own shader of the same name, which shadows the built-in.
/// Mirrors `material::ensure_named_defaults`: user decl wins.
pub(super) fn ensure_builtin_shaders(graph: &mut SceneGraph) {
    if !graph.shaders.iter().any(|s| s.name == shader::WATER) {
        // The source is a `stdlib:`-keyed path resolved from bundled bytes by
        // MoGen Studio; export ignores the source entirely.
        let mut water = ShaderDecl::new(shader::WATER, "stdlib:shaders/water.glsl");
        water.params = shader::water_params();
        graph.add_shader(water);
    }
}

/// Convert a DSL attribute value into a shader parameter value. Used for both
/// `param` defaults and material `shader_params (…)` overrides. Returns `None`
/// for shapes that can't populate a GLSL uniform.
pub(super) fn value_to_param(v: &Value) -> Option<ShaderParamValue> {
    match v {
        Value::Number(n) => Some(ShaderParamValue::Float(*n)),
        Value::Vec3(a) => Some(ShaderParamValue::Vec3(*a)),
        Value::List(xs) if xs.len() == 2 => Some(ShaderParamValue::Vec2([xs[0], xs[1]])),
        Value::List(xs) if xs.len() == 4 => {
            Some(ShaderParamValue::Vec4([xs[0], xs[1], xs[2], xs[3]]))
        }
        _ => None,
    }
}
