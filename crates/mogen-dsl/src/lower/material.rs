use anyhow::{anyhow, bail, Result};

use mogen_core::{AlphaMode, Material, MaterialShader, SceneGraph, TextureRef, UvMode};

use crate::ast::{Node, Value};

pub(super) fn collect_materials(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    for n in ast {
        if n.kind == "material" {
            register_material(n, graph)?;
        }
        if n.kind == "scene" {
            for c in &n.children {
                if c.kind == "material" {
                    register_material(c, graph)?;
                }
            }
        }
    }
    Ok(())
}

fn register_material(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    let name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("material requires a name, e.g. `material \"wood\" (...)`"))?;
    // Dedupe by `(name, origin)`: two declarations of `wood` inside the same
    // file collapse to the first (the validator already warns on this), but
    // a `wood` declared in `photo_frame.mog` and another `wood` declared in
    // `bookshelf.mog` are distinct — they're separately addressable via
    // `SceneGraph::find_material_scoped(name, origin)` so each file's
    // geometry binds to its own material rather than racing for the global
    // name.
    if graph
        .materials
        .iter()
        .any(|m| m.name == name && m.origin == node.origin)
    {
        return Ok(());
    }
    let mut mat = Material::new(&name);
    if let Some(c) = node.attr_vec3("color") {
        mat.base_color = [c.x, c.y, c.z, 1.0];
    }
    let alpha_set = node.attr_number("alpha").is_some();
    if let Some(a) = node.attr_number("alpha") {
        mat.base_color[3] = a;
    }
    if let Some(m) = node.attr_number("metallic") {
        mat.metallic = m;
    }
    if let Some(r) = node.attr_number("roughness") {
        mat.roughness = r;
    }
    if let Some(n) = node.attr_number("normal_strength") {
        mat.normal_strength = n;
    }
    if let Some(o) = node.attr_number("occlusion_strength") {
        mat.occlusion_strength = o;
    }

    let mode_attr = node
        .attr("alpha_mode")
        .and_then(|v| match v {
            Value::String(s) | Value::Ident(s) => Some(s.as_str()),
            _ => None,
        });
    if let Some(mode) = mode_attr {
        mat.alpha_mode = match mode {
            "opaque" => AlphaMode::Opaque,
            "mask" => AlphaMode::Mask,
            "blend" => AlphaMode::Blend,
            other => bail!("unknown alpha_mode \"{other}\" — expected opaque, mask, or blend"),
        };
    } else if alpha_set && mat.base_color[3] < 1.0 {
        // Convenience: authoring `alpha=0.3` without an explicit mode is the
        // textbook "I want this translucent" case, so default to Blend.
        mat.alpha_mode = AlphaMode::Blend;
    }
    if let Some(c) = node.attr_number("alpha_cutoff") {
        mat.alpha_cutoff = c;
    }
    if let Some(e) = node.attr_vec3("emissive") {
        mat.emissive = [e.x, e.y, e.z];
    }
    if let Some(s) = node.attr_number("emissive_strength") {
        mat.emissive_strength = s;
    }
    if let Some(t) = node.attr_number("transmission") {
        mat.transmission = t;
    }
    if let Some(d) = node.attr_number("double_sided") {
        mat.double_sided = d != 0.0;
    }

    mat.base_color_texture = texture_ref_attr(node, "base_color_texture");
    mat.metallic_roughness_texture = texture_ref_attr(node, "metallic_roughness_texture");
    mat.normal_texture = texture_ref_attr(node, "normal_texture");
    mat.occlusion_texture = texture_ref_attr(node, "occlusion_texture");
    mat.emissive_texture = texture_ref_attr(node, "emissive_texture");

    let uv_mode_attr = node
        .attr("uv_mode")
        .and_then(|v| match v {
            Value::String(s) | Value::Ident(s) => Some(s.as_str()),
            _ => None,
        });
    if let Some(mode) = uv_mode_attr {
        mat.uv_mode = match mode {
            "tile" => UvMode::Tile,
            "fit" => UvMode::Fit,
            other => bail!("unknown uv_mode \"{other}\" — expected tile or fit"),
        };
    }
    if let Some(s) = node.attr_number("uv_scale") {
        mat.uv_scale = [s, s];
    } else if let Some(pair) = node.attr_pair("uv_scale") {
        mat.uv_scale = pair;
    }

    let shader_attr = node
        .attr("shader")
        .and_then(|v| match v {
            Value::String(s) | Value::Ident(s) => Some(s.as_str()),
            _ => None,
        });
    if let Some(kind) = shader_attr {
        mat.shader = match kind {
            "standard" | "pbr" => MaterialShader::Standard,
            "water" => MaterialShader::Water,
            other => bail!("unknown shader \"{other}\" — expected standard or water"),
        };
    }

    mat.origin = node.origin.clone();

    graph.add_material(mat);
    Ok(())
}

fn texture_ref_attr(node: &Node, key: &str) -> Option<TextureRef> {
    let path = match node.attr(key)? {
        Value::String(s) | Value::Ident(s) => s.clone(),
        _ => return None,
    };
    Some(TextureRef::new(path))
}
