use std::path::PathBuf;

/// Decide which `origin` paths the right sidebar should show right now.
///
/// `None` (locally-authored items) is always visible. When the viewport
/// selection is on a node that came from an imported `.mog` file, that
/// file's path is added to the visible set so its materials, clips, and
/// skins are revealed alongside the local ones. Without an imported
/// selection, only locally-authored items appear — keeping the sidebar
/// scoped to the file being edited rather than every dependency it pulls
/// in via `import`. Used by `ui_summary` / `ui_materials` / `ui_animation`.
pub(in crate::app) fn visible_origins(
    scene: &mogen_core::SceneGraph,
    selection: Option<mogen_core::NodeId>,
) -> std::collections::HashSet<PathBuf> {
    let mut out = std::collections::HashSet::new();
    if let Some(id) = selection {
        if let Some(node) = scene.nodes.get(id.0 as usize) {
            if let Some(p) = &node.origin {
                out.insert(p.clone());
            }
        }
    }
    out
}

/// True when the item's `origin` is currently visible. `None` always passes
/// (locally-authored items are part of the active scene).
pub(in crate::app) fn origin_in_visible_set(
    origin: &Option<PathBuf>,
    visible: &std::collections::HashSet<PathBuf>,
) -> bool {
    match origin {
        None => true,
        Some(p) => visible.contains(p),
    }
}

/// MaterialIds referenced by any scene node whose `origin` passes
/// `origin_in_visible_set`. Lets the materials panel always show a material
/// that locally-authored geometry binds to, even when the material itself
/// was hoisted from an import — per the q1 rule "if local code touches it,
/// always show it".
pub(in crate::app) fn materials_referenced_by_visible_nodes(
    scene: &mogen_core::SceneGraph,
    visible: &std::collections::HashSet<PathBuf>,
) -> std::collections::HashSet<u32> {
    let mut ids = std::collections::HashSet::new();
    for n in &scene.nodes {
        if !origin_in_visible_set(&n.origin, visible) {
            continue;
        }
        if let Some(mid) = n.material {
            ids.insert(mid.0);
        }
    }
    ids
}
