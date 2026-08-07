use anyhow::{anyhow, bail, Result};

use mogen_core::{
    AlphaMode, Gradient, GradientAxis, GradientKind, GradientStop, Material, NodeId, SceneGraph,
    TextureRef, UvMode,
};

use crate::ast::{GradientDef, Node, Value};
use crate::lower::shader::value_to_param;

pub(super) fn collect_materials(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    for n in ast {
        collect_materials_recursive(n, graph)?;
    }
    Ok(())
}

/// Walk the whole subtree so a `material` declared inside a wrapping `group`
/// (e.g. `group "humanoid" { use "humanoid_full" () }`, where the module body
/// inlines its own materials) is still discoverable. Dedupe by `(name, origin)`
/// in `register_material` keeps repeated walks idempotent.
fn collect_materials_recursive(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    if node.kind == "material" {
        register_material(node, graph)?;
    }
    for c in &node.children {
        collect_materials_recursive(c, graph)?;
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

    if let Some(g) = node.attr_gradient("gradient") {
        mat.gradient = Some(build_gradient(g)?);
    }

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

    // Rasterization controls for `.svg` texture slots. Both are inert on
    // raster textures — the exporter only consults them when a slot's path
    // actually has an `.svg` extension. Range is enforced at export, where the
    // material name is available for the message.
    if let Some(s) = node.attr_number("texture_size") {
        if s < 1.0 || s.fract() != 0.0 {
            bail!("texture_size must be a positive whole number of pixels; got {s}");
        }
        mat.texture_size = Some(s as u32);
    }
    if let Some(w) = node.attr_number("texture_wrap") {
        mat.texture_wrap = w != 0.0;
    }

    // `shader="<name>"` names a preview shader (built-in `water` or a user
    // `shader "<name>"` declaration). `standard`/`pbr` are the implicit PBR
    // path — recorded as `None`. Unknown names are *not* rejected here; the
    // validator resolves references against declared + built-in shaders and
    // emits a spanned diagnostic (E0106) so the error points at the material.
    if let Some(kind) = node.attr_string("shader") {
        mat.shader_name = match kind {
            "standard" | "pbr" => None,
            other => Some(other.to_string()),
        };
    }
    // `shader_params (speed=3.5, tint=[…])` — a child node whose attributes feed
    // the referenced shader's declared params. Values that can't populate a
    // uniform are dropped here; the validator warns on unknown/mistyped params.
    for c in &node.children {
        if c.kind != "shader_params" {
            continue;
        }
        for (k, v) in &c.attrs {
            if let Some(pv) = value_to_param(v) {
                mat.shader_params.insert(k.clone(), pv);
            }
        }
    }

    mat.origin = node.origin.clone();

    graph.add_material(mat);
    Ok(())
}

/// Stamp a set of named default materials onto the graph, skipping any a user
/// already declared with the same name + origin (their version wins via
/// `find_material_scoped`). Each factory is called only when the material is
/// missing, then the canonical name's `origin` is stamped on. Shared by the
/// procedural generators (building openings, cave rock/water) so they all honour
/// user overrides identically.
pub(super) fn ensure_named_defaults(
    graph: &mut SceneGraph,
    origin: Option<&std::path::Path>,
    defaults: &[(&str, fn() -> Material)],
) {
    for (name, factory) in defaults {
        if graph.find_material_scoped(name, origin).is_some() {
            continue;
        }
        let mut mat = factory();
        mat.origin = origin.map(|p| p.to_path_buf());
        graph.add_material(mat);
    }
}

/// Bind a node to the nearest ancestor `mat=` if one exists, else fall back to
/// the named default material on this origin. Used by procedural emitters that
/// want to inherit a user-supplied `mat=` on their wrapper node.
pub(super) fn bind_inherited_or_default(
    id: NodeId,
    default_name: &str,
    origin: Option<&std::path::Path>,
    graph: &mut SceneGraph,
) {
    let mut cur = graph.nodes[id.0 as usize].parent;
    while let Some(p) = cur {
        if let Some(m) = graph.nodes[p.0 as usize].material {
            graph.set_material(id, m);
            return;
        }
        cur = graph.nodes[p.0 as usize].parent;
    }
    if let Some(mid) = graph.find_material_scoped(default_name, origin) {
        graph.set_material(id, mid);
    }
}

fn texture_ref_attr(node: &Node, key: &str) -> Option<TextureRef> {
    let path = match node.attr(key)? {
        Value::String(s) | Value::Ident(s) => s.clone(),
        _ => return None,
    };
    Some(TextureRef::new(path))
}

/// Lower a parsed `GradientDef` surface form into the `Gradient` carried on
/// `Material`. Validates per-kind required attributes, normalises `vertical`
/// and `stops` down to either `Linear { axis }` or `Radial`, and rejects
/// malformed stop lists with a useful message.
fn build_gradient(g: &GradientDef) -> Result<Gradient> {
    match g.kind.as_str() {
        "linear" | "vertical" => {
            let axis = if g.kind == "vertical" {
                // `vertical` is sugar — an explicit `axis=` on it is ambiguous,
                // so reject it rather than silently ignoring or honouring one.
                if node_attr(&g.attrs, "axis").is_some() {
                    bail!(
                        "gradient `vertical(...)` does not accept `axis=` — use `linear(..., axis=…)` for non-Y axes"
                    );
                }
                GradientAxis::Y
            } else {
                attr_axis(g, "axis")?.unwrap_or(GradientAxis::Y)
            };
            let from = require_color(g, "from")?;
            let to = require_color(g, "to")?;
            Ok(Gradient {
                kind: GradientKind::Linear { axis },
                stops: vec![
                    GradientStop { t: 0.0, color: from },
                    GradientStop { t: 1.0, color: to },
                ],
            })
        }
        "radial" => {
            // Radial doesn't take an axis — sampling is distance from centre.
            if node_attr(&g.attrs, "axis").is_some() {
                bail!("gradient `radial(...)` does not accept `axis=` — radial sweeps are isotropic");
            }
            let center = require_color(g, "center")?;
            let edge = require_color(g, "edge")?;
            Ok(Gradient {
                kind: GradientKind::Radial,
                stops: vec![
                    GradientStop { t: 0.0, color: center },
                    GradientStop { t: 1.0, color: edge },
                ],
            })
        }
        "stops" => {
            let colors = node_attr(&g.attrs, "colors")
                .ok_or_else(|| anyhow!("gradient `stops(...)` requires `colors=[[…], …]`"))?;
            let color_list = match colors {
                Value::ListVec3(v) => v.clone(),
                _ => bail!("gradient `stops(...)` `colors=` must be a list of 3-component colours (e.g. `[[1,0,0], [0,1,0]]`)"),
            };
            if color_list.len() < 2 {
                bail!("gradient `stops(...)` needs at least two colours");
            }
            let positions: Vec<f32> = match node_attr(&g.attrs, "positions") {
                Some(Value::List(v)) => v.clone(),
                Some(_) => {
                    bail!("gradient `stops(...)` `positions=` must be a flat list of numbers in [0, 1]");
                }
                None => {
                    // Even spacing default keeps the common case ergonomic —
                    // `stops(colors=[a, b, c])` lands stops at 0, 0.5, 1.
                    let n = color_list.len();
                    (0..n).map(|i| i as f32 / (n - 1) as f32).collect()
                }
            };
            if positions.len() != color_list.len() {
                bail!(
                    "gradient `stops(...)` has {} colours but {} positions — they must match",
                    color_list.len(),
                    positions.len()
                );
            }
            let mut stops: Vec<GradientStop> = positions
                .iter()
                .zip(color_list.iter())
                .map(|(t, c)| GradientStop { t: *t, color: [c[0], c[1], c[2], 1.0] })
                .collect();
            stops.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
            let kind = match node_attr(&g.attrs, "kind") {
                None => GradientKind::Linear { axis: attr_axis(g, "axis")?.unwrap_or(GradientAxis::Y) },
                Some(Value::Ident(s)) | Some(Value::String(s)) => match s.as_str() {
                    "linear" => GradientKind::Linear { axis: attr_axis(g, "axis")?.unwrap_or(GradientAxis::Y) },
                    "radial" => {
                        if node_attr(&g.attrs, "axis").is_some() {
                            bail!("gradient `stops(kind=radial, …)` does not accept `axis=`");
                        }
                        GradientKind::Radial
                    }
                    other => bail!("gradient `stops(...)` kind must be `linear` or `radial`, got `{other}`"),
                },
                Some(_) => bail!("gradient `stops(...)` `kind=` must be `linear` or `radial`"),
            };
            Ok(Gradient { kind, stops })
        }
        other => bail!("unknown gradient kind `{other}` — expected `linear`, `vertical`, `radial`, or `stops`"),
    }
}

fn node_attr<'a>(attrs: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn attr_axis(g: &GradientDef, key: &str) -> Result<Option<GradientAxis>> {
    match node_attr(&g.attrs, key) {
        None => Ok(None),
        Some(Value::Ident(s)) | Some(Value::String(s)) => match s.as_str() {
            "x" | "X" => Ok(Some(GradientAxis::X)),
            "y" | "Y" => Ok(Some(GradientAxis::Y)),
            "z" | "Z" => Ok(Some(GradientAxis::Z)),
            other => bail!("gradient `axis=` must be `x`, `y`, or `z`, got `{other}`"),
        },
        Some(_) => bail!("gradient `axis=` must be an axis identifier (x/y/z)"),
    }
}

fn require_color(g: &GradientDef, key: &str) -> Result<[f32; 4]> {
    match node_attr(&g.attrs, key) {
        Some(Value::Vec3(v)) => Ok([v[0], v[1], v[2], 1.0]),
        Some(_) => bail!("gradient `{key}=` must be a vec3 colour like `[1, 0.5, 0]`"),
        None => bail!("gradient `{}(...)` requires `{key}=[r, g, b]`", g.kind),
    }
}
