use std::f32::consts::TAU;

use anyhow::{anyhow, bail, Result};
use glam::{Quat, Vec3};

use mgen_core::{AlphaMode, Connector, Material, Mesh, NodeId, SceneGraph, Transform};
use mgen_geom::{
    box_mesh, capsule_mesh, clean_csg_output, cone_mesh, curved_plane_mesh, cylinder_mesh,
    difference_many, disc_mesh, ellipsoid_mesh, frustum_mesh, half_cylinder_mesh, hemisphere_mesh,
    icosphere_mesh, intersect_many, lathe_mesh, plane_mesh, prism_mesh, pyramid_mesh, quad_mesh,
    rounded_box_mesh, sphere_mesh, spline_tube_mesh, superellipsoid_mesh, torus_arc_mesh,
    torus_mesh, transform_mesh, tube_mesh, union_many, wedge_mesh,
};

use crate::anim_lower::{lower_clip, lower_joint, lower_template};
use crate::ast::{Node, Value};
use crate::attach::{resolve_attaches, resolve_attaches_in_scope};
use crate::module::{collect_modules, expand_modules};
use crate::skin_lower::{bind_meshes, lower_skeleton};

const ANIM_KINDS: &[&str] = &["joint", "clip", "spin", "open_close", "wave", "flap", "idle"];

fn is_anim_decl(kind: &str) -> bool {
    ANIM_KINDS.contains(&kind)
}

pub fn lower(ast: &[Node]) -> Result<SceneGraph> {
    // Expand modules first: collect every `module` declaration, then substitute
    // `use` calls into concrete node trees. The result has no `module`/`use`
    // nodes and no `$param` references.
    let reg = collect_modules(ast)?;
    let expanded = expand_modules(ast, &reg)?;

    let mut graph = SceneGraph::new();

    // Pass 1: hoist every top-level and scene-level `material` declaration.
    collect_materials(&expanded, &mut graph)?;

    // Pass 2: build scene graph (skip anim declarations — they need nodes first).
    for n in &expanded {
        match n.kind.as_str() {
            "material" => {} // already handled
            k if is_anim_decl(k) => {} // pass 3
            "skeleton" => {
                lower_skeleton(n, None, &mut graph)?;
            }
            "scene" => {
                for c in &n.children {
                    if c.kind == "material" || c.kind == "attach" || is_anim_decl(&c.kind) {
                        continue;
                    }
                    if c.kind == "skeleton" {
                        lower_skeleton(c, None, &mut graph)?;
                        continue;
                    }
                    lower_into(c, None, &mut graph)?;
                }
            }
            "attach" => {} // pass 2.4
            _ => {
                lower_into(n, None, &mut graph)?;
            }
        }
    }

    // Pass 2.4: resolve `attach` specs. Runs before skin binding so bind-pose
    // world matrices reflect final part positions.
    resolve_attaches(&expanded, &mut graph)?;

    // Pass 2.5: bind mesh nodes carrying `skin="<name>"` to their skeleton.
    // Runs after every mesh exists and before animations so weights are
    // computed against bind-pose world transforms.
    bind_meshes(&expanded, &mut graph)?;

    // Pass 3: joints first (clips may reference joint names), then clips,
    // then procedural templates (which can target either joints or nodes).
    lower_animations(&expanded, &mut graph)?;
    Ok(graph)
}

fn lower_animations(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    let iter = ast.iter().flat_map(|n| {
        if n.kind == "scene" {
            Box::new(n.children.iter()) as Box<dyn Iterator<Item = &Node>>
        } else {
            Box::new(std::iter::once(n))
        }
    });
    // Collect anim nodes by kind so ordering is deterministic regardless of
    // how the user wrote them in the file.
    let mut joints = Vec::new();
    let mut clips = Vec::new();
    let mut templates = Vec::new();
    for n in iter {
        match n.kind.as_str() {
            "joint" => joints.push(n),
            "clip" => clips.push(n),
            "spin" | "open_close" | "wave" | "flap" | "idle" => templates.push(n),
            _ => {}
        }
    }
    for n in joints {
        lower_joint(n, graph)?;
    }
    for n in clips {
        lower_clip(n, graph)?;
    }
    for n in templates {
        lower_template(n, graph)?;
    }
    Ok(())
}

fn collect_materials(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
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
    graph.add_material(mat);
    Ok(())
}

fn lower_into(node: &Node, parent: Option<NodeId>, graph: &mut SceneGraph) -> Result<NodeId> {
    if node.kind == "mirror" || node.kind == "array" {
        return expand_replicator(node, parent, graph);
    }
    if matches!(node.kind.as_str(), "union" | "difference" | "intersect") {
        return lower_csg(node, parent, graph);
    }

    let transform = transform_from_attrs(node);
    let name = node.name.clone().unwrap_or_else(|| node.kind.clone());

    let id = match parent {
        None => graph.add_root(&name, &node.kind, transform),
        Some(p) => graph.add_child(p, &name, &node.kind, transform),
    };

    // Metadata: role, tags (comma-separated string).
    if let Some(Value::String(role)) = node.attr("role") {
        graph.nodes[id.0 as usize].role = Some(role.clone());
    } else if let Some(Value::Ident(role)) = node.attr("role") {
        graph.nodes[id.0 as usize].role = Some(role.clone());
    }
    if let Some(Value::String(tags)) = node.attr("tags") {
        graph.nodes[id.0 as usize].tags =
            tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    }

    // Material lookup.
    if let Some(Value::String(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material(mat_name)
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    } else if let Some(Value::Ident(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material(mat_name)
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    }

    if let Some(mesh) = primitive_mesh(node) {
        graph.set_mesh(id, mesh);
    } else {
        match node.kind.as_str() {
            "group" | "scene" => {}
            "material" => bail!("`material` must be a top-level or scene-level declaration"),
            other => bail!("unknown node kind: {}", other),
        }
    }

    // Expose canonical connectors (top/bottom/etc.) for primitives, derived
    // from the declared size/radius/height. User-declared `connector` children
    // further down replace these by name.
    for (name, at, dir) in default_connectors(node) {
        graph.nodes[id.0 as usize].connectors.push(Connector::from_at_dir(
            name.to_string(),
            at,
            dir,
            String::new(),
            None,
        ));
    }

    for c in &node.children {
        match c.kind.as_str() {
            "material" | "attach" => continue,
            "connector" => {
                add_connector(c, id, graph)?;
            }
            _ => {
                lower_into(c, Some(id), graph)?;
            }
        }
    }

    // Groups pick up six face connectors (top/bottom/left/right/front/back)
    // synthesized from the subtree AABB. User-declared connectors with the
    // same name already took precedence via `add_connector`'s replace-by-name,
    // so we only push names that aren't present.
    if node.kind == "group" {
        add_aabb_connectors_if_missing(id, graph);
    }

    Ok(id)
}

/// Canonical attachment points for each primitive, in the primitive's local
/// space. Returned as `(name, at, dir)` triples where `dir` points outward from
/// the surface. Non-primitives (groups, CSG, scenes) return nothing — they need
/// user-declared connectors.
fn default_connectors(node: &Node) -> Vec<(&'static str, Vec3, Vec3)> {
    let mut out: Vec<(&'static str, Vec3, Vec3)> = Vec::new();
    match node.kind.as_str() {
        "box" | "rounded_box" | "prism" => {
            let s = node.attr_vec3("size").unwrap_or(Vec3::ONE);
            let (hx, hy, hz) = (s.x * 0.5, s.y * 0.5, s.z * 0.5);
            out.push(("top",    Vec3::new(0.0,  hy, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -hy, 0.0), -Vec3::Y));
            out.push(("right",  Vec3::new( hx, 0.0, 0.0), Vec3::X));
            out.push(("left",   Vec3::new(-hx, 0.0, 0.0), -Vec3::X));
            out.push(("back",   Vec3::new(0.0, 0.0,  hz), Vec3::Z));
            out.push(("front",  Vec3::new(0.0, 0.0, -hz), -Vec3::Z));
        }
        "cylinder" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let hy = height * 0.5;
            out.push(("top",    Vec3::new(0.0,  hy, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -hy, 0.0), -Vec3::Y));
            out.push(("side",   Vec3::new(radius, 0.0, 0.0), Vec3::X));
        }
        "cone" | "pyramid" => {
            let height = node.attr_number("height").unwrap_or(1.0);
            let hy = height * 0.5;
            out.push(("apex",   Vec3::new(0.0,  hy, 0.0), Vec3::Y));
            out.push(("top",    Vec3::new(0.0,  hy, 0.0), Vec3::Y));
            out.push(("base",   Vec3::new(0.0, -hy, 0.0), -Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -hy, 0.0), -Vec3::Y));
        }
        "sphere" | "icosphere" => {
            let r = node.attr_number("radius").unwrap_or(0.5);
            out.push(("top",    Vec3::new(0.0,  r, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -r, 0.0), -Vec3::Y));
            out.push(("right",  Vec3::new( r, 0.0, 0.0), Vec3::X));
            out.push(("left",   Vec3::new(-r, 0.0, 0.0), -Vec3::X));
            out.push(("back",   Vec3::new(0.0, 0.0,  r), Vec3::Z));
            out.push(("front",  Vec3::new(0.0, 0.0, -r), -Vec3::Z));
        }
        "capsule" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let top = height * 0.5 + radius;
            out.push(("top",    Vec3::new(0.0,  top, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -top, 0.0), -Vec3::Y));
        }
        "torus" => {
            let major = node.attr_number("major").unwrap_or(0.5);
            let minor = node.attr_number("minor").unwrap_or(0.15);
            out.push(("top",    Vec3::new(0.0,  minor, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -minor, 0.0), -Vec3::Y));
            out.push(("outer",  Vec3::new(major + minor, 0.0, 0.0), Vec3::X));
            out.push(("inner",  Vec3::new(major - minor, 0.0, 0.0), -Vec3::X));
        }
        "plane" | "disc" => {
            out.push(("top",    Vec3::ZERO, Vec3::Y));
            out.push(("bottom", Vec3::ZERO, -Vec3::Y));
        }
        "quad" => {
            out.push(("front",  Vec3::ZERO, Vec3::Z));
            out.push(("back",   Vec3::ZERO, -Vec3::Z));
        }
        "wedge" => {
            // Doorstop: tall back wall at -Z, slopes down to front edge at +Z.
            let s = node.attr_vec3("size").unwrap_or(Vec3::ONE);
            let (hx, hy, hz) = (s.x * 0.5, s.y * 0.5, s.z * 0.5);
            out.push(("bottom", Vec3::new(0.0, -hy, 0.0), -Vec3::Y));
            out.push(("back",   Vec3::new(0.0, 0.0, -hz), -Vec3::Z));
            out.push(("right",  Vec3::new( hx, 0.0, 0.0),  Vec3::X));
            out.push(("left",   Vec3::new(-hx, 0.0, 0.0), -Vec3::X));
            // Slope face normal has +Y and +Z; connector sits at slope midpoint.
            let sl = ((2.0 * hy).powi(2) + (2.0 * hz).powi(2)).sqrt().max(1e-6);
            let slope_n = Vec3::new(0.0, (2.0 * hz) / sl, (2.0 * hy) / sl);
            out.push(("top",   Vec3::new(0.0, 0.0, 0.0), slope_n));
            out.push(("slope", Vec3::new(0.0, 0.0, 0.0), slope_n));
        }
        "frustum" => {
            let bottom = node.attr_pair("bottom").unwrap_or([1.0, 1.0]);
            let top = node.attr_pair("top").unwrap_or([0.5, 0.5]);
            let height = node.attr_number("height").unwrap_or(1.0);
            let hy = height * 0.5;
            let max_x = bottom[0].max(top[0]) * 0.5;
            let max_z = bottom[1].max(top[1]) * 0.5;
            out.push(("top",    Vec3::new(0.0,  hy, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -hy, 0.0), -Vec3::Y));
            out.push(("right",  Vec3::new( max_x, 0.0, 0.0), Vec3::X));
            out.push(("left",   Vec3::new(-max_x, 0.0, 0.0), -Vec3::X));
            out.push(("back",   Vec3::new(0.0, 0.0, -max_z), -Vec3::Z));
            out.push(("front",  Vec3::new(0.0, 0.0,  max_z), Vec3::Z));
        }
        "tube" => {
            let outer = node.attr_number("outer").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let hy = height * 0.5;
            out.push(("top",    Vec3::new(0.0,  hy, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -hy, 0.0), -Vec3::Y));
            out.push(("side",   Vec3::new(outer, 0.0, 0.0), Vec3::X));
        }
        "hemisphere" => {
            // Base sits on y=0; apex at y=+radius. Origin is the base centre.
            let r = node.attr_number("radius").unwrap_or(0.5);
            out.push(("top",    Vec3::new(0.0, r, 0.0), Vec3::Y));
            out.push(("apex",   Vec3::new(0.0, r, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::ZERO, -Vec3::Y));
            out.push(("base",   Vec3::ZERO, -Vec3::Y));
        }
        "half_cylinder" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let hy = height * 0.5;
            out.push(("top",    Vec3::new(0.0,  hy, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -hy, 0.0), -Vec3::Y));
            out.push(("side",   Vec3::new(radius, 0.0, 0.0), Vec3::X));
            // Flat face sits on x=0 with outward normal -X.
            out.push(("flat",   Vec3::ZERO, -Vec3::X));
        }
        "torus_arc" => {
            let major = node.attr_number("major").unwrap_or(0.5);
            let minor = node.attr_number("minor").unwrap_or(0.15);
            let arc_deg = node.attr_number("arc").unwrap_or(90.0);
            let arc = arc_deg.to_radians();
            out.push(("top",    Vec3::new(0.0,  minor, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -minor, 0.0), -Vec3::Y));
            // start cap at phi=0 (on +X axis, facing -Z).
            out.push(("start",  Vec3::new(major, 0.0, 0.0), -Vec3::Z));
            // end cap at phi=arc: tangent direction = (-sin phi, 0, cos phi).
            let (sp, cp) = (arc.sin(), arc.cos());
            out.push(("end",    Vec3::new(cp * major, 0.0, sp * major), Vec3::new(-sp, 0.0, cp)));
        }
        "ellipsoid" | "superellipsoid" => {
            let s = node.attr_vec3("size").unwrap_or(Vec3::ONE);
            let (hx, hy, hz) = (s.x * 0.5, s.y * 0.5, s.z * 0.5);
            out.push(("top",    Vec3::new(0.0,  hy, 0.0), Vec3::Y));
            out.push(("bottom", Vec3::new(0.0, -hy, 0.0), -Vec3::Y));
            out.push(("right",  Vec3::new( hx, 0.0, 0.0), Vec3::X));
            out.push(("left",   Vec3::new(-hx, 0.0, 0.0), -Vec3::X));
            out.push(("back",   Vec3::new(0.0, 0.0,  hz), Vec3::Z));
            out.push(("front",  Vec3::new(0.0, 0.0, -hz), -Vec3::Z));
        }
        "curved_plane" => {
            // Unbent frame only — top faces +Y, bottom faces -Y, just like `plane`.
            // Bent geometry will offset these from the declared origin, but +Y is
            // still the "outward" direction of the unbent patch.
            out.push(("top",    Vec3::ZERO, Vec3::Y));
            out.push(("bottom", Vec3::ZERO, -Vec3::Y));
        }
        "lathe" => {
            // Profile is authored in [r, y] — bottom = first row, top = last row.
            // Without parsing the profile list we pick sensible axial connectors.
            let profile = node.attr_list_pair("profile").unwrap_or_default();
            if let (Some(first), Some(last)) = (profile.first(), profile.last()) {
                out.push(("bottom", Vec3::new(0.0, first[1], 0.0), -Vec3::Y));
                out.push(("top",    Vec3::new(0.0, last[1],  0.0),  Vec3::Y));
            } else {
                out.push(("bottom", Vec3::ZERO, -Vec3::Y));
                out.push(("top",    Vec3::ZERO,  Vec3::Y));
            }
        }
        "spline_tube" => {
            // `start` at the first control point (facing -tangent), `end` at the last.
            let points = node.attr_list_vec3("points").unwrap_or_default();
            if points.len() >= 2 {
                let p0 = Vec3::from_array(points[0]);
                let p1 = Vec3::from_array(points[1]);
                let pn = Vec3::from_array(points[points.len() - 1]);
                let pn1 = Vec3::from_array(points[points.len() - 2]);
                let t_start = (p1 - p0).normalize_or(Vec3::Y);
                let t_end = (pn - pn1).normalize_or(Vec3::Y);
                out.push(("start", p0, -t_start));
                out.push(("end",   pn,  t_end));
            }
        }
        _ => {}
    }
    out
}

fn add_connector(node: &Node, parent: NodeId, graph: &mut SceneGraph) -> Result<()> {
    let name = node.name.clone().ok_or_else(|| anyhow!("connector requires a name"))?;
    let at = node.attr_vec3("at").unwrap_or(Vec3::ZERO);
    let dir = node.attr_vec3("dir").unwrap_or(Vec3::Y);
    let tag = match node.attr("tag") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Ident(s)) => s.clone(),
        _ => String::new(),
    };
    let radius = node.attr_number("radius");
    let c = Connector::from_at_dir(name.clone(), at, dir, tag, radius);
    let connectors = &mut graph.nodes[parent.0 as usize].connectors;
    connectors.retain(|existing| existing.name != name);
    connectors.push(c);
    Ok(())
}

fn transform_from_attrs(node: &Node) -> Transform {
    let t = node.attr_vec3("pos").unwrap_or(Vec3::ZERO);
    let r = node.attr_rotation("rot").unwrap_or(glam::Quat::IDENTITY);
    let s = node.attr_scale("scale").unwrap_or(Vec3::ONE);
    Transform::from_trs(t, r, s)
}

/// Lower a CSG node. Each child is evaluated into a single mesh (in the CSG
/// node's local space, with the child's own transform baked in); the boolean
/// op is applied left-to-right; the result is cleaned (weld/cull/normals) and
/// attached to the CSG node. CSG children do not become separate scene nodes.
fn lower_csg(node: &Node, parent: Option<NodeId>, graph: &mut SceneGraph) -> Result<NodeId> {
    let transform = transform_from_attrs(node);
    let name = node.name.clone().unwrap_or_else(|| node.kind.clone());

    let id = match parent {
        None => graph.add_root(&name, &node.kind, transform),
        Some(p) => graph.add_child(p, &name, &node.kind, transform),
    };

    // Metadata + material exactly as for other kinds.
    apply_metadata(node, id, graph)?;

    // If the CSG node itself declares no material, inherit from the first
    // operand's `mat=`. Operands don't become separate scene nodes, so their
    // material would otherwise be silently dropped — and "hollow out this
    // brick dome" naturally should stay brick.
    if graph.nodes[id.0 as usize].material.is_none() {
        if let Some(first_operand) = node
            .children
            .iter()
            .find(|c| !matches!(c.kind.as_str(), "material" | "connector"))
        {
            let mat_name = match first_operand.attr("mat") {
                Some(Value::String(s)) | Some(Value::Ident(s)) => Some(s.clone()),
                _ => None,
            };
            if let Some(name) = mat_name {
                if let Some(mid) = graph.find_material(&name) {
                    graph.set_material(id, mid);
                }
            }
        }
    }

    // Evaluate operand meshes. Connectors are allowed on the CSG node itself
    // (captured below) but silently skipped inside operand bodies.
    let mut operand_meshes: Vec<Mesh> = Vec::new();
    for c in &node.children {
        match c.kind.as_str() {
            "material" | "connector" => continue,
            _ => operand_meshes.push(eval_mesh(c, /*bake_transform=*/ true)?),
        }
    }

    let combined = match node.kind.as_str() {
        "union" => {
            if operand_meshes.is_empty() {
                bail!("`union` requires at least one operand");
            }
            union_many(&operand_meshes)
        }
        "difference" => {
            if operand_meshes.is_empty() {
                bail!("`difference` requires at least one operand");
            }
            let (first, rest) = operand_meshes.split_first().unwrap();
            difference_many(first, rest)
        }
        "intersect" => {
            if operand_meshes.len() < 2 {
                bail!("`intersect` requires at least two operands");
            }
            intersect_many(&operand_meshes)
        }
        _ => unreachable!(),
    };

    graph.set_mesh(id, clean_csg_output(&combined));

    // Attach connectors declared directly on the CSG node.
    for c in &node.children {
        if c.kind == "connector" {
            add_connector(c, id, graph)?;
        }
    }
    // Default face connectors from the combined mesh AABB.
    add_aabb_connectors_if_missing(id, graph);
    Ok(id)
}

/// Synthesize `top`/`bottom`/`left`/`right`/`front`/`back` connectors from the
/// node's subtree AABB. Skips any name already present (so user-declared
/// connectors keep priority). No-op when the subtree has no geometry.
fn add_aabb_connectors_if_missing(id: mgen_core::NodeId, graph: &mut mgen_core::SceneGraph) {
    let Some(aabb) = mgen_core::subtree_local_aabb(graph, id) else { return };
    let c = aabb.center();
    let specs: [(&'static str, Vec3, Vec3); 6] = [
        ("top",    Vec3::new(c.x, aabb.max.y, c.z),  Vec3::Y),
        ("bottom", Vec3::new(c.x, aabb.min.y, c.z), -Vec3::Y),
        ("right",  Vec3::new(aabb.max.x, c.y, c.z),  Vec3::X),
        ("left",   Vec3::new(aabb.min.x, c.y, c.z), -Vec3::X),
        ("back",   Vec3::new(c.x, c.y, aabb.max.z),  Vec3::Z),
        ("front",  Vec3::new(c.x, c.y, aabb.min.z), -Vec3::Z),
    ];
    let conns = &mut graph.nodes[id.0 as usize].connectors;
    for (name, at, dir) in specs {
        if conns.iter().any(|c| c.name == name) {
            continue;
        }
        conns.push(Connector::from_at_dir(name.to_string(), at, dir, String::new(), None));
    }
}

fn apply_metadata(node: &Node, id: NodeId, graph: &mut SceneGraph) -> Result<()> {
    if let Some(Value::String(role)) = node.attr("role") {
        graph.nodes[id.0 as usize].role = Some(role.clone());
    } else if let Some(Value::Ident(role)) = node.attr("role") {
        graph.nodes[id.0 as usize].role = Some(role.clone());
    }
    if let Some(Value::String(tags)) = node.attr("tags") {
        graph.nodes[id.0 as usize].tags =
            tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    }
    if let Some(Value::String(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material(mat_name)
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    } else if let Some(Value::Ident(mat_name)) = node.attr("mat") {
        let mid = graph
            .find_material(mat_name)
            .ok_or_else(|| anyhow!("unknown material: {mat_name}"))?;
        graph.set_material(id, mid);
    }
    Ok(())
}

/// Evaluate a DSL node into a single triangle mesh in its parent's space.
/// If `bake_transform` is true, the node's own pos/rot/scale is baked into the
/// vertices — used when folding operands into a CSG result.
fn eval_mesh(node: &Node, bake_transform: bool) -> Result<Mesh> {
    let local = if let Some(mesh) = primitive_mesh(node) {
        mesh
    } else {
        match node.kind.as_str() {
        "union" | "difference" | "intersect" => {
            let mut operands: Vec<Mesh> = Vec::new();
            for c in &node.children {
                match c.kind.as_str() {
                    "material" | "connector" => continue,
                    _ => operands.push(eval_mesh(c, true)?),
                }
            }
            match node.kind.as_str() {
                "union" => {
                    if operands.is_empty() {
                        bail!("`union` requires at least one operand");
                    }
                    union_many(&operands)
                }
                "difference" => {
                    if operands.is_empty() {
                        bail!("`difference` requires at least one operand");
                    }
                    let (first, rest) = operands.split_first().unwrap();
                    difference_many(first, rest)
                }
                "intersect" => {
                    if operands.len() < 2 {
                        bail!("`intersect` requires at least two operands");
                    }
                    intersect_many(&operands)
                }
                _ => unreachable!(),
            }
        }
        other => bail!("`{other}` is not allowed as a CSG operand"),
        }
    };

    if !bake_transform {
        return Ok(local);
    }
    let t = transform_from_attrs(node);
    if t.is_identity() {
        Ok(local)
    } else {
        Ok(transform_mesh(&local, t.to_mat4()))
    }
}

fn axis_vec3(v: &Value) -> Option<Vec3> {
    match v {
        Value::Ident(s) | Value::String(s) => match s.as_str() {
            "x" | "X" => Some(Vec3::X),
            "y" | "Y" => Some(Vec3::Y),
            "z" | "Z" => Some(Vec3::Z),
            _ => None,
        },
        Value::Vec3(v) => Some(Vec3::from_array(*v)),
        _ => None,
    }
}

fn expand_replicator(node: &Node, parent: Option<NodeId>, graph: &mut SceneGraph) -> Result<NodeId> {
    let wrapper_name = node.name.clone().unwrap_or_else(|| node.kind.clone());
    let wrapper_transform = transform_from_attrs(node);
    let wrapper_id = match parent {
        None => graph.add_root(&wrapper_name, &node.kind, wrapper_transform),
        Some(p) => graph.add_child(p, &wrapper_name, &node.kind, wrapper_transform),
    };

    let instance_transforms: Vec<Transform> = match node.kind.as_str() {
        "mirror" => {
            let axis = node
                .attr("axis")
                .and_then(axis_vec3)
                .unwrap_or(Vec3::X);
            let s = Vec3::ONE - 2.0 * axis.normalize_or_zero().abs();
            // Clamp to [-1, 1]: +axis component becomes -1.
            let mirror_scale = Vec3::new(
                if axis.x.abs() > 0.5 { -1.0 } else { 1.0 },
                if axis.y.abs() > 0.5 { -1.0 } else { 1.0 },
                if axis.z.abs() > 0.5 { -1.0 } else { 1.0 },
            );
            let _ = s; // Reserved for arbitrary-axis mirror in future.
            vec![Transform::IDENTITY, Transform::from_trs(Vec3::ZERO, Quat::IDENTITY, mirror_scale)]
        }
        "array" => {
            let count = node.attr_number("count").unwrap_or(1.0).max(1.0) as u32;
            let axis = node.attr("around").and_then(axis_vec3).unwrap_or(Vec3::Y);
            let axis = axis.normalize_or(Vec3::Y);
            let start = node.attr_number("start_angle").unwrap_or(0.0).to_radians();
            (0..count)
                .map(|i| {
                    let a = start + TAU * (i as f32) / (count as f32);
                    Transform::from_trs(Vec3::ZERO, Quat::from_axis_angle(axis, a), Vec3::ONE)
                })
                .collect()
        }
        _ => unreachable!(),
    };

    for (i, t) in instance_transforms.iter().enumerate() {
        let instance_name = format!("{wrapper_name}_{i}");
        let iid = graph.add_child(wrapper_id, instance_name, "group", *t);
        for c in &node.children {
            match c.kind.as_str() {
                "material" | "attach" => continue,
                "connector" => add_connector(c, iid, graph)?,
                _ => { lower_into(c, Some(iid), graph)?; }
            }
        }
        // Resolve attach specs declared inside the replicator body, scoped to
        // this instance's subtree. Without this, every copy would resolve
        // parent/child against the first instance's nodes (name collision).
        resolve_attaches_in_scope(&node.children, graph, iid)?;
        add_aabb_connectors_if_missing(iid, graph);
    }
    add_aabb_connectors_if_missing(wrapper_id, graph);

    Ok(wrapper_id)
}

/// Dispatch a primitive `Node` to its mesh builder. Returns `None` for non-
/// primitive kinds (group, scene, material, CSG ops, animation decls, …) so
/// callers can handle those separately.
fn primitive_mesh(node: &Node) -> Option<Mesh> {
    let m = match node.kind.as_str() {
        "box" => {
            let size = node.attr_vec3("size").map(|v| [v.x, v.y, v.z]).unwrap_or([1.0, 1.0, 1.0]);
            box_mesh(size)
        }
        "plane" => {
            let v = node.attr_vec3("size").unwrap_or(Vec3::ONE);
            plane_mesh([v.x, v.z])
        }
        "quad" => {
            // Accept vec3 (ignore Y) or a 2-element list; default 1×1.
            let (w, h) = if let Some(v) = node.attr_vec3("size") {
                (v.x, v.y)
            } else if let Some([w, h]) = node.attr_pair("size") {
                (w, h)
            } else {
                (1.0, 1.0)
            };
            quad_mesh([w, h])
        }
        "cylinder" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            cylinder_mesh(radius, height, segments)
        }
        "cone" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            cone_mesh(radius, height, segments)
        }
        "sphere" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let rings = node.attr_number("rings").map(|n| n as u32).unwrap_or(16);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            sphere_mesh(radius, rings, segments)
        }
        "capsule" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let rings = node.attr_number("rings").map(|n| n as u32).unwrap_or(8);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            capsule_mesh(radius, height, rings, segments)
        }
        "torus" => {
            let major = node.attr_number("major").unwrap_or(0.5);
            let minor = node.attr_number("minor").unwrap_or(0.15);
            let major_segments =
                node.attr_number("major_segments").map(|n| n as u32).unwrap_or(24);
            let minor_segments =
                node.attr_number("minor_segments").map(|n| n as u32).unwrap_or(12);
            torus_mesh(major, minor, major_segments, minor_segments)
        }
        "prism" => {
            let size = node.attr_vec3("size").map(|v| [v.x, v.y, v.z]).unwrap_or([1.0, 1.0, 1.0]);
            prism_mesh(size)
        }
        "pyramid" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let sides = node.attr_number("sides").map(|n| n as u32).unwrap_or(4);
            pyramid_mesh(radius, height, sides)
        }
        "disc" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            disc_mesh(radius, segments)
        }
        "icosphere" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let subdivisions = node.attr_number("subdivisions").map(|n| n as u32).unwrap_or(2);
            icosphere_mesh(radius, subdivisions)
        }
        "rounded_box" => {
            let size = node.attr_vec3("size").map(|v| [v.x, v.y, v.z]).unwrap_or([1.0, 1.0, 1.0]);
            let radius = node.attr_number("radius").unwrap_or(0.1);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(4);
            rounded_box_mesh(size, radius, segments)
        }
        "wedge" => {
            let size = node.attr_vec3("size").map(|v| [v.x, v.y, v.z]).unwrap_or([1.0, 1.0, 1.0]);
            wedge_mesh(size)
        }
        "frustum" => {
            let bottom = node.attr_pair("bottom").unwrap_or([1.0, 1.0]);
            let top = node.attr_pair("top").unwrap_or([0.5, 0.5]);
            let height = node.attr_number("height").unwrap_or(1.0);
            frustum_mesh(bottom, top, height)
        }
        "tube" => {
            let outer = node.attr_number("outer").unwrap_or(0.5);
            let inner = node.attr_number("inner").unwrap_or(0.3);
            let height = node.attr_number("height").unwrap_or(1.0);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            tube_mesh(outer, inner, height, segments)
        }
        "hemisphere" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let rings = node.attr_number("rings").map(|n| n as u32).unwrap_or(8);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            hemisphere_mesh(radius, rings, segments)
        }
        "half_cylinder" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            half_cylinder_mesh(radius, height, segments)
        }
        "torus_arc" => {
            let major = node.attr_number("major").unwrap_or(0.5);
            let minor = node.attr_number("minor").unwrap_or(0.15);
            let arc_deg = node.attr_number("arc").unwrap_or(90.0);
            let major_segments =
                node.attr_number("major_segments").map(|n| n as u32).unwrap_or(24);
            let minor_segments =
                node.attr_number("minor_segments").map(|n| n as u32).unwrap_or(12);
            torus_arc_mesh(major, minor, arc_deg.to_radians(), major_segments, minor_segments)
        }
        "ellipsoid" => {
            let size = node.attr_vec3("size").map(|v| [v.x, v.y, v.z]).unwrap_or([1.0, 1.0, 1.0]);
            let rings = node.attr_number("rings").map(|n| n as u32).unwrap_or(16);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            ellipsoid_mesh(size, rings, segments)
        }
        "superellipsoid" => {
            let size = node.attr_vec3("size").map(|v| [v.x, v.y, v.z]).unwrap_or([1.0, 1.0, 1.0]);
            let ew = node.attr_number("ew").unwrap_or(1.0);
            let ns = node.attr_number("ns").unwrap_or(1.0);
            let rings = node.attr_number("rings").map(|n| n as u32).unwrap_or(16);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            superellipsoid_mesh(size, ew, ns, rings, segments)
        }
        "curved_plane" => {
            // `size` accepts vec3 (use x,z — ignore y) or a 2-element list [x, z].
            let (sx, sz) = if let Some(v) = node.attr_vec3("size") {
                (v.x, v.z)
            } else if let Some(p) = node.attr_pair("size") {
                (p[0], p[1])
            } else {
                (1.0, 1.0)
            };
            let bend_u = node.attr_number("bend_u").unwrap_or(0.0).to_radians();
            let bend_v = node.attr_number("bend_v").unwrap_or(0.0).to_radians();
            let segments_u = node.attr_number("segments_u").map(|n| n as u32).unwrap_or(12);
            let segments_v = node.attr_number("segments_v").map(|n| n as u32).unwrap_or(12);
            curved_plane_mesh([sx, sz], bend_u, bend_v, segments_u, segments_v)
        }
        "lathe" => {
            let profile = node
                .attr_list_pair("profile")
                .unwrap_or_else(|| vec![[0.0, -0.5], [0.5, 0.0], [0.0, 0.5]]);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(24);
            let cap_ends = node.attr_number("cap_ends").map(|n| n != 0.0).unwrap_or(true);
            lathe_mesh(&profile, segments, cap_ends)
        }
        "spline_tube" => {
            let points = node
                .attr_list_vec3("points")
                .unwrap_or_else(|| vec![[0.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
            // `radii` (list) takes precedence; else fall back to scalar `radius`.
            let radii = if let Some(r) = node.attr_list("radii") {
                r.to_vec()
            } else {
                vec![node.attr_number("radius").unwrap_or(0.1)]
            };
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(12);
            let samples = node.attr_number("samples").map(|n| n as u32).unwrap_or(8);
            let cap_ends = node.attr_number("cap_ends").map(|n| n != 0.0).unwrap_or(true);
            spline_tube_mesh(&points, &radii, segments, samples, cap_ends)
        }
        _ => return None,
    };
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn lower_src(src: &str) -> SceneGraph {
        let ast = parse(src).expect("parse");
        lower(&ast).expect("lower")
    }

    fn find_mesh_node<'a>(g: &'a SceneGraph, name: &str) -> &'a mgen_core::SceneNode {
        g.nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("no node named {name}"))
    }

    #[test]
    fn lowers_every_new_primitive() {
        // One scene that exercises every new primitive kind end-to-end:
        // parse → validate attrs → lower → mesh attached to node.
        let g = lower_src(
            r#"
            scene {
              wedge         "w" (size=[1, 0.5, 1])
              frustum       "f" (bottom=[1, 1], top=[0.5, 0.5], height=1)
              tube          "t" (outer=0.5, inner=0.3, height=1)
              hemisphere    "h" (radius=0.5)
              half_cylinder "hc" (radius=0.5, height=1)
              torus_arc     "ta" (major=0.5, minor=0.1, arc=90)
              ellipsoid     "e" (size=[1, 0.5, 0.8])
            }
        "#,
        );
        for name in ["w", "f", "t", "h", "hc", "ta", "e"] {
            let n = find_mesh_node(&g, name);
            assert!(n.mesh.is_some(), "{name} has no mesh");
            let mesh = n.mesh.as_ref().unwrap();
            assert!(!mesh.positions.is_empty(), "{name} mesh has no positions");
            assert!(!mesh.indices.is_empty(), "{name} mesh has no indices");
            // Default connectors were populated.
            assert!(!n.connectors.is_empty(), "{name} has no default connectors");
        }
    }

    #[test]
    fn tube_has_inner_and_outer_walls() {
        let g = lower_src(
            r#"scene { tube "t" (outer=1.0, inner=0.5, height=1.0) }"#,
        );
        let n = find_mesh_node(&g, "t");
        let mesh = n.mesh.as_ref().unwrap();
        // Some verts at outer radius, some at inner radius — cheap "is hollow" check.
        let has_outer = mesh.positions.iter().any(|p| (p[0] * p[0] + p[2] * p[2]).sqrt() > 0.9);
        let has_inner = mesh.positions.iter().any(|p| {
            let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
            r > 0.4 && r < 0.6
        });
        assert!(has_outer, "tube is missing outer wall");
        assert!(has_inner, "tube is missing inner wall");
    }

    #[test]
    fn hemisphere_has_base_at_origin() {
        let g = lower_src(r#"scene { hemisphere "h" (radius=1.0) }"#);
        let mesh = find_mesh_node(&g, "h").mesh.as_ref().unwrap();
        // Base cap sits on y=0; apex at y=+radius.
        let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
        assert!((min_y).abs() < 1e-5, "expected base at y=0, got {min_y}");
        assert!((max_y - 1.0).abs() < 1e-5, "expected apex at y=radius, got {max_y}");
    }

    #[test]
    fn wedge_slope_connector_faces_up_and_forward() {
        let g = lower_src(r#"scene { wedge "w" (size=[1.0, 1.0, 1.0]) }"#);
        let n = find_mesh_node(&g, "w");
        let slope = n
            .connectors
            .iter()
            .find(|c| c.name == "slope")
            .expect("wedge missing slope connector");
        // Connector rotation turns +Y into the connector's outward dir.
        let dir = slope.rotation * Vec3::Y;
        assert!(dir.y > 0.0 && dir.z > 0.0, "slope normal should point +Y and +Z, got {dir:?}");
    }

    #[test]
    fn lowers_every_organic_primitive() {
        // End-to-end check of the four organic-shape primitives. Uses nested
        // list literals (`[[x,y,z], ...]`, `[[r,y], ...]`) to confirm the
        // grammar extension landed.
        let g = lower_src(
            r#"
            scene {
              superellipsoid "se"   (size=[1, 0.8, 1], ew=0.5, ns=1)
              curved_plane   "leaf" (size=[0.4, 1.0], bend_u=20, bend_v=40)
              lathe          "vase" (profile=[[0.0, -0.5], [0.4, -0.3], [0.5, 0.0], [0.3, 0.4], [0.0, 0.5]])
              spline_tube    "ban"  (points=[[0, 0, 0], [0.3, 0.2, 0], [0.5, 0.1, 0], [0.6, -0.1, 0]],
                                     radii=[0.08, 0.12, 0.10, 0.05])
            }
        "#,
        );
        for name in ["se", "leaf", "vase", "ban"] {
            let n = find_mesh_node(&g, name);
            assert!(n.mesh.is_some(), "{name} has no mesh");
            let mesh = n.mesh.as_ref().unwrap();
            assert!(!mesh.positions.is_empty(), "{name} mesh has no positions");
            assert!(!mesh.indices.is_empty(), "{name} mesh has no indices");
            assert_eq!(mesh.positions.len(), mesh.normals.len(), "{name} normals arity mismatch");
        }
    }

    #[test]
    fn superellipsoid_boxy_exponent_fills_corners() {
        // ew, ns > 1 push the shape toward a box — corner vertices sit close to
        // the declared size bounds, unlike a sphere which tucks them inward.
        let g = lower_src(
            r#"scene { superellipsoid "s" (size=[1.0, 1.0, 1.0], ew=3.0, ns=3.0, rings=24, segments=32) }"#,
        );
        let mesh = find_mesh_node(&g, "s").mesh.as_ref().unwrap();
        // Find the vertex nearest the +X+Y+Z corner and check it's close to [0.5, 0.5, 0.5].
        let max_corner = mesh
            .positions
            .iter()
            .map(|p| (p[0] + p[1] + p[2], *p))
            .fold((f32::NEG_INFINITY, [0.0; 3]), |acc, x| if x.0 > acc.0 { x } else { acc })
            .1;
        // Sphere would give ~0.29 on each axis; boxy should be > 0.4.
        assert!(max_corner[0] > 0.4 && max_corner[1] > 0.4 && max_corner[2] > 0.4,
            "boxy superellipsoid should reach corners, got {max_corner:?}");
    }

    #[test]
    fn curved_plane_bends_toward_positive_y() {
        // Positive bend_u lifts the left/right edges. The centre stays near y=0;
        // the edges sit well above y=0.
        let g = lower_src(
            r#"scene { curved_plane "l" (size=[1.0, 0.2], bend_u=90, segments_u=16) }"#,
        );
        let mesh = find_mesh_node(&g, "l").mesh.as_ref().unwrap();
        let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
        let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        assert!(max_y > 0.05, "bent plane should lift edges above y=0, got max_y={max_y}");
        assert!(min_y.abs() < 1e-4, "unbent center should still sit at y=0, got min_y={min_y}");
    }

    #[test]
    fn lathe_revolves_around_y() {
        // A flat profile `[0.5, 0.0]` for two rows makes a closed cylinder;
        // every vertex on the side wall lands at radius ≈ 0.5.
        let g = lower_src(
            r#"scene { lathe "l" (profile=[[0.5, -0.5], [0.5, 0.5]], segments=16) }"#,
        );
        let mesh = find_mesh_node(&g, "l").mesh.as_ref().unwrap();
        let side_verts: Vec<_> = mesh
            .positions
            .iter()
            .filter(|p| (p[0] * p[0] + p[2] * p[2]).sqrt() > 0.4)
            .collect();
        assert!(!side_verts.is_empty(), "lathe should have side-wall verts");
        for p in side_verts {
            let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
            assert!((r - 0.5).abs() < 1e-4, "side-wall radius should be 0.5, got {r}");
        }
    }

    #[test]
    fn spline_tube_follows_control_points() {
        // Straight tube along Y should yield every vertex in a narrow X-band
        // around the axis.
        let g = lower_src(
            r#"scene { spline_tube "t" (points=[[0,0,0],[0,0.5,0],[0,1,0]], radius=0.1, segments=8, samples=4) }"#,
        );
        let mesh = find_mesh_node(&g, "t").mesh.as_ref().unwrap();
        for p in &mesh.positions {
            let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
            assert!(r < 0.12, "straight tube along Y should stay near the axis, got r={r}");
        }
        let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
        assert!(min_y < 0.05 && max_y > 0.95, "tube should span y∈[0, 1], got [{min_y}, {max_y}]");
    }

    #[test]
    fn spline_tube_exposes_start_and_end_connectors() {
        let g = lower_src(
            r#"scene { spline_tube "t" (points=[[0,0,0],[0.5,0.5,0],[1,0,0]], radius=0.05) }"#,
        );
        let n = find_mesh_node(&g, "t");
        let names: Vec<_> = n.connectors.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"start"), "connectors: {names:?}");
        assert!(names.contains(&"end"), "connectors: {names:?}");
    }

    #[test]
    fn csg_inherits_first_operand_material_when_unset() {
        let g = lower_src(
            r#"
            material "brick" (color=[0.7, 0.3, 0.2])
            material "soot"  (color=[0.1, 0.1, 0.1])
            scene {
              difference "dome" {
                hemisphere "outer" (radius=0.6, mat="brick")
                hemisphere "inner" (radius=0.5, mat="soot")
              }
            }
            "#,
        );
        let dome = find_mesh_node(&g, "dome");
        let brick = g.find_material("brick").expect("brick material");
        assert_eq!(dome.material, Some(brick),
            "CSG should inherit first operand's material when own mat is absent");
    }

    #[test]
    fn csg_own_material_wins_over_operand() {
        let g = lower_src(
            r#"
            material "brick" (color=[0.7, 0.3, 0.2])
            material "stone" (color=[0.5, 0.5, 0.5])
            scene {
              difference "dome" (mat="stone") {
                hemisphere "outer" (radius=0.6, mat="brick")
                hemisphere "inner" (radius=0.5)
              }
            }
            "#,
        );
        let dome = find_mesh_node(&g, "dome");
        let stone = g.find_material("stone").expect("stone material");
        assert_eq!(dome.material, Some(stone),
            "explicit mat on CSG node must win over first-operand inheritance");
    }
}
