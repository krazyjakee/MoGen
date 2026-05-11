//! Stable node-path bookkeeping and source-offset lookups. Selections
//! survive recompiles by re-resolving these `(name, sibling_disambiguator)`
//! paths, and editor-driven selections walk back the other way through
//! `find_deepest_node_at_offset` / `find_use_at_offset`.

use mogen_core::{NodeId, SceneGraph};

use super::SelectionPath;

/// Walk from `id` up to a root collecting `(name, sibling_disambiguator)`
/// in root → ... → node order. The disambiguator is the index of the node
/// among siblings under the same parent that share its `name` (in scene
/// order). Replicators (`array`, `mirror`, …) produce siblings with
/// identical names, so name-only paths would collide.
pub(crate) fn node_path(scene: &SceneGraph, id: NodeId) -> Option<SelectionPath> {
    if id.0 as usize >= scene.nodes.len() {
        return None;
    }
    let mut out: SelectionPath = Vec::new();
    let mut cur = Some(id);
    while let Some(n) = cur {
        let node = scene.nodes.get(n.0 as usize)?;
        let siblings: &[NodeId] = match node.parent {
            Some(pid) => &scene.nodes.get(pid.0 as usize)?.children,
            None => &scene.roots,
        };
        let mut disamb: u32 = 0;
        for sib in siblings {
            if *sib == n {
                break;
            }
            if let Some(s) = scene.nodes.get(sib.0 as usize) {
                if s.name == node.name {
                    disamb += 1;
                }
            }
        }
        out.push((node.name.clone(), disamb));
        cur = node.parent;
    }
    out.reverse();
    Some(out)
}

/// Re-resolve a saved selection path against a freshly-lowered scene by
/// walking root → leaf and picking the `disamb`-th same-named child at each
/// step. Returns `None` when any step finds no matching sibling — usually
/// because the node was deleted by the most recent edit.
pub(crate) fn resolve_node_path(scene: &SceneGraph, path: &[(String, u32)]) -> Option<NodeId> {
    let mut iter = path.iter();
    let (root_name, root_disamb) = iter.next()?;
    let mut current = pick_nth_named(scene, &scene.roots, root_name, *root_disamb)?;
    for (name, disamb) in iter {
        let children = &scene.nodes.get(current.0 as usize)?.children;
        current = pick_nth_named(scene, children, name, *disamb)?;
    }
    Some(current)
}

fn pick_nth_named(scene: &SceneGraph, ids: &[NodeId], name: &str, n: u32) -> Option<NodeId> {
    let mut count: u32 = 0;
    for &id in ids {
        let node = scene.nodes.get(id.0 as usize)?;
        if node.name == name {
            if count == n {
                return Some(id);
            }
            count += 1;
        }
    }
    None
}

/// Resolve a `use "<stem>" (...)` source line at `byte_offset` (in the active
/// `source`) to a SceneNode in `scene`. Returns the first imported root whose
/// origin's file stem matches the use's name — an "imported root" being a
/// SceneNode whose origin differs from its parent's, so we land on the topmost
/// node of the imported subtree rather than some interior leaf.
///
/// `find_deepest_node_at_offset` skips imported nodes outright (their spans
/// live in another file), so a click on a `use "X"` line in the editor
/// otherwise falls through to whatever surrounding container's span contains
/// it (typically `scene`). This fallback turns those clicks into the
/// import's root selection, which is what the user actually wants when they
/// click the `use` line.
///
/// Returns `None` when the source no longer parses, the offset isn't inside
/// any `use` AST node, or no SceneNode origin matches the use's stem.
pub(crate) fn find_use_at_offset(
    scene: &SceneGraph,
    source: &str,
    byte_offset: usize,
) -> Option<NodeId> {
    let ast = mogen_dsl::parse(source).ok()?;
    let stem = find_use_stem_at_offset(&ast, byte_offset)?;
    for (idx, node) in scene.nodes.iter().enumerate() {
        let Some(origin) = node.origin.as_deref() else {
            continue;
        };
        let Some(node_stem) = origin.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if node_stem != stem {
            continue;
        }
        // Skip interior imported nodes: the parent must be from a different
        // origin (or have no origin at all) for `idx` to be the topmost node
        // of the imported subtree.
        let parent_same_origin = node
            .parent
            .and_then(|pid| scene.nodes.get(pid.0 as usize))
            .and_then(|p| p.origin.as_deref())
            == Some(origin);
        if parent_same_origin {
            continue;
        }
        return Some(NodeId(idx as u32));
    }
    None
}

fn find_use_stem_at_offset(
    ast: &[mogen_dsl::ast::Node],
    byte_offset: usize,
) -> Option<String> {
    fn walk(node: &mogen_dsl::ast::Node, offset: usize) -> Option<String> {
        // Recurse first so a `use` nested inside another node wins over its
        // enclosing container, mirroring the deepest-span tiebreak elsewhere.
        for c in &node.children {
            if let Some(s) = walk(c, offset) {
                return Some(s);
            }
        }
        if node.kind == "use"
            && offset >= node.span.start
            && offset < node.span.end
        {
            return node.name.clone();
        }
        None
    }
    for n in ast {
        if let Some(s) = walk(n, byte_offset) {
            return Some(s);
        }
    }
    None
}

/// Find the deepest user-authored scene node whose `source_span` contains
/// `byte_offset`. "Deepest" = smallest containing span, so a click inside a
/// child wins over its enclosing group. Nodes lowered from imported `.mog`
/// files (`origin = Some(...)`) are skipped: their spans index into another
/// file, not the active source. Returns `None` when the offset falls in a
/// comment, whitespace, or otherwise outside every node's authored range —
/// callers preserve the existing selection in that case rather than treating
/// it as a deselect.
pub(crate) fn find_deepest_node_at_offset(
    scene: &SceneGraph,
    byte_offset: usize,
) -> Option<NodeId> {
    let mut best: Option<(NodeId, usize)> = None;
    for (idx, node) in scene.nodes.iter().enumerate() {
        if node.origin.is_some() {
            continue;
        }
        let Some(span) = node.source_span else {
            continue;
        };
        // Half-open: a caret resting at `span.end` belongs to whatever
        // structure starts there (or to none, if nothing follows). Without
        // this, two adjacent siblings with `prev.end == next.start` would
        // both claim the boundary offset and the deepest-by-length tiebreak
        // would pick whichever happened to be enumerated last.
        if byte_offset < span.start || byte_offset >= span.end {
            continue;
        }
        let len = span.end - span.start;
        match best {
            None => best = Some((NodeId(idx as u32), len)),
            Some((_, prev_len)) if len < prev_len => best = Some((NodeId(idx as u32), len)),
            _ => {}
        }
    }
    if let Some((id, _)) = best {
        return Some(id);
    }
    // Fallback: caret may be inside a `track "..." (...)` header. Tracks
    // aren't scene nodes — the bone they drive is — but selecting the bone
    // is what makes the gizmo land on the moving target. Walk every active
    // (or any-status) clip authored in the active source and match track
    // spans by enclosing offset.
    for clip in &scene.clips {
        if clip.origin.is_some() {
            continue;
        }
        for track in &clip.tracks {
            let Some(span) = track.source_span else {
                continue;
            };
            if byte_offset >= span.start && byte_offset < span.end {
                return Some(track.node);
            }
        }
    }
    None
}
