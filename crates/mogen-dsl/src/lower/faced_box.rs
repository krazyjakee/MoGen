//! Per-face materials on `box`.
//!
//! A `box "crate" (size=…, faces=[…])` carries six material names in the fixed
//! `+X, -X, +Y, -Y, +Z, -Z` order (the same order the BD1 → mog map converter
//! emits, one per cube face). Rather than teach the core mesh to hold submeshes,
//! the box lowers to an editable wrapper (the box node itself, keeping its
//! transform / metadata / connectors) plus one *frozen* child per distinct
//! material — each a quad group of just that material's faces. Faces sharing a
//! material collapse into one child, so a crate with one material on the lid and
//! another on the sides becomes two children, not six.
//!
//! The children are stamped non-editable: they have no AST node of their own, so
//! the Studio inspector redirects clicks to the wrapper (the same contract the
//! procedural generators use). An empty `""` entry means "use the box's own
//! `mat=` / inherited material" for that face.

use anyhow::{anyhow, bail, Result};
use glam::Vec3;

use mogen_core::{MaterialId, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{box_faces_mesh, box_mesh};

use crate::ast::Node;

use super::helpers::{anchor_for, apply_anchor_to_mesh, resolve_size3};

/// Short, filename-safe tokens for the six faces, used to name the per-material
/// child nodes. Index order matches [`mogen_geom::box_faces_mesh`].
const FACE_TOKENS: [&str; 6] = ["px", "nx", "py", "ny", "pz", "nz"];

/// Lower a `box` carrying `faces=[…]` into per-material quad children under the
/// (meshless) wrapper `box_id`. Returns the anchor shift applied to the child
/// geometry so the caller can offset the wrapper's default connectors by the
/// same amount.
pub(super) fn lower_faced_box(
    node: &Node,
    box_id: NodeId,
    graph: &mut SceneGraph,
) -> Result<Vec3> {
    let faces = node
        .attr_list_string("faces")
        .ok_or_else(|| anyhow!("`box` faces= must be a list of 6 material names"))?;
    if faces.len() != 6 {
        bail!(
            "`box` faces= needs exactly 6 entries (+X, -X, +Y, -Y, +Z, -Z order), got {}",
            faces.len()
        );
    }

    let size = resolve_size3(node, Vec3::ONE);
    let fallback = graph.nodes[box_id.0 as usize].material;

    // Resolve each face's material name to an id (empty string → the box's own
    // material), preserving first-seen order so the emitted children are stable.
    let mut groups: Vec<(Option<MaterialId>, Vec<usize>)> = Vec::new();
    for (fi, name) in faces.iter().enumerate() {
        let mat = if name.is_empty() {
            fallback
        } else {
            Some(
                graph
                    .find_material_scoped(name, node.origin.as_deref())
                    .ok_or_else(|| anyhow!("unknown material: {name}"))?,
            )
        };
        match groups.iter_mut().find(|(m, _)| *m == mat) {
            Some((_, idxs)) => idxs.push(fi),
            None => groups.push((mat, vec![fi])),
        }
    }

    // Anchor shift comes from the full box AABB so every face group lands at the
    // same anchored origin (a single-face AABB would be degenerate on one axis).
    let anchor = anchor_for(node);
    let mut probe = box_mesh([size.x, size.y, size.z], UvMode::Fit);
    let anchor_shift = apply_anchor_to_mesh(&mut probe, anchor.as_deref());

    let box_name = graph.nodes[box_id.0 as usize].name.clone();
    for (mat, idxs) in groups {
        let uv_mode = mat
            .and_then(|m| graph.materials.get(m.0 as usize))
            .map(|m| m.uv_mode)
            .unwrap_or_default();
        let mut mesh = box_faces_mesh([size.x, size.y, size.z], uv_mode, &idxs);
        if anchor_shift != Vec3::ZERO {
            for p in &mut mesh.positions {
                p[0] += anchor_shift.x;
                p[1] += anchor_shift.y;
                p[2] += anchor_shift.z;
            }
        }
        let tokens: String = idxs.iter().map(|&i| FACE_TOKENS[i]).collect::<Vec<_>>().join("");
        let child_name = format!("{box_name}_{tokens}");
        let child = graph.add_child(box_id, child_name, "box_face", Transform::IDENTITY);
        graph.set_mesh(child, mesh);
        if let Some(m) = mat {
            graph.set_material(child, m);
        }
        graph.set_not_editable(child);
    }

    Ok(anchor_shift)
}
