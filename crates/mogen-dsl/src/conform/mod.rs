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

use anyhow::{bail, Result};

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
    resolve_specs(&collect_conforms(ast)?, graph, None)
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
    resolve_specs(&specs, graph, Some(scope_root))
}

fn resolve_specs(
    specs: &[ConformSpec],
    graph: &mut SceneGraph,
    scope: Option<NodeId>,
) -> Result<()> {
    if specs.is_empty() {
        return Ok(());
    }
    // Resolve identities before reparenting changes paths; operate on a private
    // graph so a failed/cyclic binding cannot leave a partially deformed scene.
    let bindings: Vec<_> = specs
        .iter()
        .map(|s| {
            Ok((
                find(s, graph, scope, &s.target, true)?,
                find(s, graph, scope, &s.child, false)?,
            ))
        })
        .collect::<Result<_>>()?;
    let mut staged = graph.clone();
    let mut done = vec![false; specs.len()];
    for _ in 0..specs.len() {
        let Some(i) = (0..specs.len()).find(|&i| {
            !done[i]
                && !bindings
                    .iter()
                    .enumerate()
                    .any(|(j, (_, child))| j != i && !done[j] && *child == bindings[i].0)
        }) else {
            bail!(
                "conform: cyclic target/child dependency at bytes {}..{}",
                specs[done.iter().position(|d| !*d).unwrap()].span.start,
                specs[done.iter().position(|d| !*d).unwrap()].span.end
            );
        };
        apply_conform(&specs[i], &mut staged, bindings[i].0, bindings[i].1).map_err(|e| {
            anyhow::anyhow!(
                "conform at bytes {}..{} in use {:?}: target {:?}, child {:?}: {e:#}",
                specs[i].span.start,
                specs[i].span.end,
                specs[i].use_id,
                specs[i].target,
                specs[i].child
            )
        })?;
        done[i] = true;
    }
    *graph = staged;
    Ok(())
}
fn find(
    spec: &ConformSpec,
    graph: &SceneGraph,
    scope: Option<NodeId>,
    name: &str,
    target: bool,
) -> Result<NodeId> {
    let matches: Vec<NodeId> = if target && name.starts_with('/') {
        let mut parents = graph.roots.clone();
        let segments: Vec<_> = name[1..].split('/').collect();
        if segments
            .iter()
            .any(|s| s.is_empty() || *s == "." || *s == "..")
        {
            bail!("conform: absolute target path requires named instance segments: {name:?}");
        }
        let mut found = vec![];
        for (index, segment) in segments.iter().enumerate() {
            found = parents
                .into_iter()
                .filter(|&id| graph.get(id).name == *segment)
                .collect();
            if found.len() != 1 {
                break;
            }
            if index + 1 < segments.len() {
                parents = graph.get(found[0]).children.clone();
            } else {
                parents = vec![];
            }
        }
        found
    } else {
        graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, n)| {
                if n.name != name {
                    return false;
                }
                if let Some(root) = scope {
                    let mut current = Some(NodeId(*i as u32));
                    while let Some(id) = current {
                        if id == root {
                            return true;
                        }
                        current = graph.get(id).parent;
                    }
                    false
                } else {
                    graph.use_id_visible(spec.use_id, n.use_id)
                }
            })
            .map(|(i, _)| NodeId(i as u32))
            .collect()
    };
    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    let reason = if matches.len() > 1 {
        "ambiguous"
    } else if graph.nodes.iter().any(|n| n.name == name) {
        "present but inaccessible"
    } else {
        "unknown"
    };
    bail!("conform: {reason} {} node {name:?} at bytes {}..{} (use {:?}, replicated scope {:?}). Plain names stay instance-local. Bind an external target with its absolute /assembly/body instance path, or declare conform at scene scope after instantiation. External targets in replicators must already exist.",if target{"target"}else{"child"},spec.span.start,spec.span.end,spec.use_id,scope);
}

fn apply_conform(
    spec: &ConformSpec,
    graph: &mut SceneGraph,
    target_id: NodeId,
    child_id: NodeId,
) -> Result<()> {
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
