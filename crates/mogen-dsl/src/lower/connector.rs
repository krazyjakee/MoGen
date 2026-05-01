use anyhow::{anyhow, Result};
use glam::Vec3;

use mogen_core::{Connector, NodeId, SceneGraph};

use crate::ast::{Node, Value};

use super::helpers::resolve_size3;

/// Canonical attachment points for each primitive, in the primitive's local
/// space. Returned as `(name, at, dir)` triples where `dir` points outward from
/// the surface. Non-primitives (groups, CSG, scenes) return nothing — they need
/// user-declared connectors.
pub(super) fn default_connectors(node: &Node) -> Vec<(&'static str, Vec3, Vec3)> {
    let mut out: Vec<(&'static str, Vec3, Vec3)> = Vec::new();
    match node.kind.as_str() {
        "box" | "rounded_box" | "prism" | "slab" | "post" | "panel" | "wall" => {
            let s = resolve_size3(node, Vec3::ONE);
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
            let s = resolve_size3(node, Vec3::ONE);
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
            let s = resolve_size3(node, Vec3::ONE);
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
        "leaf_card" => {
            // Stem at origin (the leaf grows upward from y=0); tip at the top.
            let s = node
                .attr("size")
                .and_then(|v| match v {
                    Value::Number(n) => Some([*n, *n]),
                    Value::Vec3(v) => Some([v[0], v[1]]),
                    Value::List(v) if v.len() == 2 => Some([v[0], v[1]]),
                    _ => None,
                })
                .unwrap_or([0.4, 0.4]);
            let h = node.attr_number("h").unwrap_or(s[1]);
            out.push(("stem", Vec3::ZERO, -Vec3::Y));
            out.push(("base", Vec3::ZERO, -Vec3::Y));
            out.push(("tip",  Vec3::new(0.0, h, 0.0), Vec3::Y));
            out.push(("top",  Vec3::new(0.0, h, 0.0), Vec3::Y));
        }
        "spline_tube" | "spline_ribbon" => {
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

pub(super) fn add_connector(node: &Node, parent: NodeId, graph: &mut SceneGraph) -> Result<()> {
    let name = node.name.clone().ok_or_else(|| anyhow!("connector requires a name"))?;
    let at = node.attr_vec3("at").unwrap_or(Vec3::ZERO);
    let dir = node.attr_vec3("dir").unwrap_or(Vec3::Y);
    let tag = match node.attr("tag") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Ident(s)) => s.clone(),
        _ => String::new(),
    };
    let radius = node.attr_number("radius");
    let mut c = Connector::from_at_dir(name.clone(), at, dir, tag, radius);
    c.source_span = Some(node.span);
    let connectors = &mut graph.nodes[parent.0 as usize].connectors;
    connectors.retain(|existing| existing.name != name);
    connectors.push(c);
    Ok(())
}

/// Synthesize `top`/`bottom`/`left`/`right`/`front`/`back` connectors from the
/// node's subtree AABB. Skips any name already present (so user-declared
/// connectors keep priority). No-op when the subtree has no geometry.
pub(super) fn add_aabb_connectors_if_missing(id: NodeId, graph: &mut SceneGraph) {
    let Some(aabb) = mogen_core::subtree_local_aabb(graph, id) else { return };
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
