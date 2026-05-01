use anyhow::{anyhow, bail, Result};
use glam::{Quat, Vec3};

use mogen_core::{Light, LightKind, NodeId, SceneGraph};

use crate::ast::{Node, Value};

use super::helpers::transform_from_attrs;

/// Lower a `light "..." (kind=…, color=[…], intensity=…, …)` node into a
/// transform-only `SceneNode` carrying a [`Light`]. Direction is implicit:
/// glTF lights point along the node's local `-Z`. If the user supplies
/// `dir=[x, y, z]`, we synthesize a rotation that takes `-Z` to that vector
/// (overriding any `rot=` on the same node — a light can only point one way).
pub(super) fn lower_light(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let mut transform = transform_from_attrs(node);
    if let Some(dir) = node.attr_vec3("dir") {
        let len = dir.length();
        if len > 1e-6 {
            transform.rotation = Quat::from_rotation_arc(Vec3::NEG_Z, dir / len);
        }
    }

    let name = node.name.clone().unwrap_or_else(|| node.kind.clone());
    let id = match parent {
        None => graph.add_root(&name, &node.kind, transform),
        Some(p) => graph.add_child(p, &name, &node.kind, transform),
    };
    graph.set_source_span(id, node.span);
    graph.nodes[id.0 as usize].use_id = node.use_id;
    graph.nodes[id.0 as usize].origin = node.origin.clone();

    if let Some(Value::String(role) | Value::Ident(role)) = node.attr("role") {
        graph.nodes[id.0 as usize].role = Some(role.clone());
    }
    if let Some(Value::String(tags)) = node.attr("tags") {
        graph.nodes[id.0 as usize].tags = tags
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
    }

    let light = build_light(node)?;
    graph.set_light(id, light);

    if !node.children.is_empty() {
        bail!("`light` does not accept child nodes");
    }
    Ok(id)
}

fn build_light(node: &Node) -> Result<Light> {
    let mut light = Light::default();

    let kind_str = node
        .attr("kind")
        .and_then(|v| match v {
            Value::String(s) | Value::Ident(s) => Some(s.as_str()),
            _ => None,
        })
        .ok_or_else(|| anyhow!("`light` requires kind=directional|point|spot"))?;
    light.kind = match kind_str {
        "directional" => LightKind::Directional,
        "point" => LightKind::Point,
        "spot" => LightKind::Spot,
        other => bail!("unknown light kind \"{other}\" (expected directional|point|spot)"),
    };

    if let Some(c) = node.attr_vec3("color") {
        light.color = [c.x, c.y, c.z];
    }
    if let Some(n) = node.attr_number("intensity") {
        light.intensity = n;
    }
    if let Some(n) = node.attr_number("range") {
        if matches!(light.kind, LightKind::Directional) {
            // glTF spec: `range` is point/spot only. Silently dropping would
            // hide a real authoring mistake; bail with a clear message.
            bail!("`range` has no effect on directional lights");
        }
        if n <= 0.0 {
            bail!("`range` must be > 0 (got {n})");
        }
        light.range = Some(n);
    }
    if let Some(n) = node.attr_number("inner_cone") {
        if !matches!(light.kind, LightKind::Spot) {
            bail!("`inner_cone` is only valid on spot lights");
        }
        light.inner_cone_rad = n.to_radians();
    }
    if let Some(n) = node.attr_number("outer_cone") {
        if !matches!(light.kind, LightKind::Spot) {
            bail!("`outer_cone` is only valid on spot lights");
        }
        light.outer_cone_rad = n.to_radians();
    }
    if matches!(light.kind, LightKind::Spot) && light.inner_cone_rad > light.outer_cone_rad {
        bail!(
            "spot `inner_cone` ({:.2}°) must be ≤ `outer_cone` ({:.2}°)",
            light.inner_cone_rad.to_degrees(),
            light.outer_cone_rad.to_degrees()
        );
    }
    Ok(light)
}
