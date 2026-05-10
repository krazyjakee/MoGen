//! `conform` primitive resolution.
//!
//! A `conform (target="...", child="...", from="...", to="...", ...)` node
//! says: deform `child`'s mesh so its vertices follow a path on `target`'s
//! surface. The path runs from `target.from` to `target.to` (two connectors
//! on the target node), the strip's "along" axis becomes arc-length on the
//! path, and the strip's perpendicular axes lie tangent / normal to the
//! surface at each sample.
//!
//! Companion to `attach.rs`: where attach sets a rigid transform, conform
//! mutates vertex positions. Runs immediately after `resolve_attaches` so an
//! attached child can also be conformed (the conform pass reads the post-
//! attach transforms when computing the target↔child coordinate map). Runs
//! before `bind_meshes` so skin bind-pose world matrices reflect the
//! deformed geometry.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

use mogen_core::{NodeId, SceneGraph};

use crate::ast::Node;

mod kinds;
mod patch;
mod path;
mod place;
mod spec;

#[cfg(test)]
mod tests;

use spec::{collect_conforms, walk, ConformMode, ConformSpec};

/// Resolve every `conform` declared at AST scope.
pub fn resolve_conforms(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    let specs = collect_conforms(ast)?;
    let mut by_use: HashMap<Option<u32>, Vec<ConformSpec>> = HashMap::new();
    for s in specs {
        by_use.entry(s.use_id).or_default().push(s);
    }
    for (_, group) in by_use {
        for spec in group {
            apply_conform(&spec, graph, None)?;
        }
    }
    Ok(())
}

/// Resolve `conform` declarations inside a replicated subtree.
pub fn resolve_conforms_in_scope(
    children: &[Node],
    graph: &mut SceneGraph,
    scope_root: NodeId,
) -> Result<()> {
    let mut specs = Vec::new();
    for c in children {
        walk(c, &mut specs)?;
    }
    for spec in &specs {
        apply_conform(spec, graph, Some(scope_root))?;
    }
    Ok(())
}

fn apply_conform(
    spec: &ConformSpec,
    graph: &mut SceneGraph,
    scope: Option<NodeId>,
) -> Result<()> {
    // Lookup precedence mirrors `attach`: explicit scope (replicator
    // per-instance pass) is strict; otherwise frame-visible match against
    // the spec's `use_id`.
    let find = |name: &str| -> Option<NodeId> {
        if let Some(root) = scope {
            return graph.find_node_in_subtree(root, name);
        }
        graph
            .nodes
            .iter()
            .position(|n| n.name == name && graph.use_id_visible(spec.use_id, n.use_id))
            .map(|i| NodeId(i as u32))
    };

    let target_id = find(&spec.target)
        .ok_or_else(|| anyhow!("conform: unknown target node \"{}\"", spec.target))?;
    let child_id = find(&spec.child)
        .ok_or_else(|| anyhow!("conform: unknown child node \"{}\"", spec.child))?;
    if target_id == child_id {
        bail!(
            "conform: \"{}\" cannot be conformed onto itself",
            spec.child
        );
    }

    match &spec.mode {
        ConformMode::Path { .. } => path::apply_path(spec, graph, target_id, child_id),
        ConformMode::Patch { .. } => patch::apply_patch(spec, graph, target_id, child_id),
    }
}
