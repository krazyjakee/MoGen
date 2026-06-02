//! Multi-selection bookkeeping for the viewport: plain / shift-click
//! handling, Figma-style click drill-down, and the import-aware
//! `redirect_pick` walk that keeps picks landing on user-authored spans.

use std::sync::Arc;

use eframe::egui;
use mogen_core::{NodeId, SceneGraph};

use super::path::node_path;
use super::ViewerState;

/// Per-click cycle state for Figma-style drill-down. `cursor` and `leaf`
/// must both match the next click for the cycle to advance — different
/// cursor (>[`PICK_CYCLE_RADIUS_PX`] away) or a different deepest hit
/// resets the depth to 0.
#[derive(Clone, Copy, Debug)]
pub struct PickCycle {
    pub cursor: egui::Pos2,
    pub leaf: NodeId,
    /// 0 = the default `redirect_pick` target (top of the editable chain).
    /// Each repeat click adds 1, walking one node closer to `leaf`.
    pub depth: usize,
}

/// Pixel radius within which a follow-up click counts as "the same click"
/// for cycle purposes. Slightly forgiving so a hand-held mouse twitch
/// between clicks doesn't reset the cycle.
pub(crate) const PICK_CYCLE_RADIUS_PX: f32 = 4.0;

/// Replace the selection with a single node (or clear it). Used for plain
/// (non-modifier) clicks and `Esc`. Picks that land on an imported subtree
/// (`use_id != None`) get redirected to the nearest user-authored wrapper —
/// the group whose span lives in the active source. Without this the gizmo
/// / inspector would write back at byte offsets from a different file and
/// either no-op or silently corrupt the active scene. See `redirect_pick`.
///
/// On a successful selection the editor caret jumps to the node's declaration.
pub(crate) fn replace_selection(st: &mut ViewerState, id: Option<NodeId>) {
    let id = id.and_then(|n| {
        st.scene.as_ref().and_then(|s| redirect_pick(s, n))
    });
    set_primary_selection_raw(st, id);
}

/// Set the selection to exactly `id` (or clear when `None`) without any
/// `redirect_pick` rewriting. Used by the cycling drill-down where the
/// caller has already computed the precise target — re-running the
/// redirect would walk the cycle's deeper picks back up to the wrapper /
/// outer group, defeating the drill.
fn set_primary_selection_raw(st: &mut ViewerState, id: Option<NodeId>) {
    st.selected.clear();
    st.selected_paths.clear();
    if let Some(n) = id {
        st.selected.push(n);
        if let Some(path) = st.scene.as_ref().and_then(|s| node_path(s, n)) {
            st.selected_paths.push(path);
        }
    }
    st.pending_caret = id
        .and_then(|n| {
            st.scene
                .as_ref()
                .and_then(|s| s.nodes.get(n.0 as usize))
                .and_then(|node| node.source_span)
        })
        .map(|span| span.start);
}

/// Figma-style click drill-down. Replaces the selection with the node
/// `redirect_pick` would normally pick (the editable wrapper or top-level
/// group), unless this click targets the same screen point and the same
/// raw leaf as the previous click — in which case it advances one
/// ancestor closer to `leaf`. Repeat-clicking eventually lands on `leaf`
/// itself (or stops one short, when crossing into a node would land on a
/// span from an imported `.mog` file that the gizmo can't legally edit).
///
/// Resets the cycle to depth 0 whenever the cursor or the deepest hit
/// changes between consecutive clicks. The caller is responsible for
/// clearing `pick_cycle` on selection changes from other sources (Esc,
/// shift-click, scene recompile, gizmo commit).
pub(crate) fn replace_selection_cycling(
    st: &mut ViewerState,
    leaf: NodeId,
    cursor: egui::Pos2,
) {
    let Some(scene) = st.scene.as_ref().map(Arc::clone) else {
        replace_selection(st, Some(leaf));
        st.pick_cycle = None;
        return;
    };

    // chain = [leaf, parent, grandparent, …, root]
    let mut chain: Vec<NodeId> = Vec::new();
    let mut cur = Some(leaf);
    while let Some(id) = cur {
        chain.push(id);
        cur = scene.nodes.get(id.0 as usize).and_then(|n| n.parent);
    }
    if chain.is_empty() {
        replace_selection(st, Some(leaf));
        st.pick_cycle = None;
        return;
    }

    // Depth-0 selection: whatever `redirect_pick` would return today.
    // For imports that's the wrapper; for plain user-authored geometry
    // it's the leaf itself, in which case cycling is a no-op (nothing
    // deeper to walk to). When `redirect_pick` returns `None` (imported
    // root with no editable ancestor anywhere) we mirror today's
    // `replace_selection` and clear the selection — landing on the
    // un-editable leaf instead would let the user grab a gizmo handle on
    // a node whose source span lives in another file.
    let Some(default_id) = redirect_pick(&scene, leaf) else {
        set_primary_selection_raw(st, None);
        st.pick_cycle = None;
        return;
    };
    let default_idx = chain.iter().position(|&n| n == default_id).unwrap_or(0);

    let same_target = match st.pick_cycle {
        Some(pc) => {
            pc.leaf == leaf
                && (pc.cursor - cursor).length() <= PICK_CYCLE_RADIUS_PX
        }
        None => false,
    };
    let prev_depth = st.pick_cycle.map(|pc| pc.depth).unwrap_or(0);
    let candidate_depth = if same_target { prev_depth + 1 } else { 0 };

    // Editability boundary: stop one short of any node whose source span
    // lives in an imported file. The depth-0 target is always safe
    // (`redirect_pick` already enforces that), so the walk-down only
    // needs to check the nodes strictly between it and the leaf.
    let max_depth = max_editable_depth(&scene, &chain, default_idx);
    let depth = candidate_depth.min(max_depth);

    let target_idx = default_idx.saturating_sub(depth);
    let target = chain[target_idx];
    set_primary_selection_raw(st, Some(target));
    st.pick_cycle = Some(PickCycle { cursor, leaf, depth });
}

/// How many steps from `default_idx` toward the leaf (index 0) we can
/// take before crossing into a node authored in another file. Caller
/// uses this to clamp the requested cycle depth.
fn max_editable_depth(scene: &SceneGraph, chain: &[NodeId], default_idx: usize) -> usize {
    let mut depth = 0usize;
    let mut idx = default_idx;
    while idx > 0 {
        let next = chain[idx - 1];
        let Some(node) = scene.nodes.get(next.0 as usize) else {
            break;
        };
        if node.origin.is_some() {
            break;
        }
        depth += 1;
        idx -= 1;
    }
    depth
}

/// Toggle a node's membership in the selection. Used for shift/cmd-click.
/// Picks are redirected through `redirect_pick` first (same reason as
/// `replace_selection`). If the node is already selected, it's removed and
/// the primary becomes whichever entry is now last (no caret jump unless the
/// primary changed). Otherwise the node is appended (becoming the new primary)
/// and the caret jumps to its declaration.
pub(crate) fn toggle_selection(st: &mut ViewerState, id: NodeId) {
    let Some(target) = st.scene.as_ref().and_then(|s| redirect_pick(s, id)) else {
        return;
    };
    if let Some(pos) = st.selected.iter().position(|n| *n == target) {
        let was_primary = pos + 1 == st.selected.len();
        st.selected.remove(pos);
        if pos < st.selected_paths.len() {
            st.selected_paths.remove(pos);
        }
        if was_primary {
            // Caret follows the new primary, if there is one. No jump when
            // the selection emptied — the editor stays put.
            st.pending_caret = st
                .selected
                .last()
                .copied()
                .and_then(|n| {
                    st.scene
                        .as_ref()
                        .and_then(|s| s.nodes.get(n.0 as usize))
                        .and_then(|node| node.source_span)
                })
                .map(|span| span.start);
        }
    } else {
        st.selected.push(target);
        if let Some(path) = st.scene.as_ref().and_then(|s| node_path(s, target)) {
            st.selected_paths.push(path);
        }
        st.pending_caret = st
            .scene
            .as_ref()
            .and_then(|s| s.nodes.get(target.0 as usize))
            .and_then(|node| node.source_span)
            .map(|span| span.start);
    }
}

/// Walk from `id` up through parents to the nearest ancestor authored
/// directly in the active source (`use_id == None`). Returns the original
/// `id` when it's already user-authored. Returns `None` when the walk
/// runs out without finding one — e.g. `scene { use "desk" }` with no
/// wrapping group, where no parent has a span in the active file.
///
/// Editing a node carrying `use_id != None` would splice into the active
/// `.mog` source at byte offsets that come from the imported file, so the
/// viewport's gizmo + inspector route every interaction through this
/// redirect first. The output is what the user actually manipulates.
///
/// Import wrappers are a special case: `use "X" (pos=...)` of an imported
/// file synthesises a wrapper group whose `use_id` is set (it opens a new
/// frame) but whose `origin` is `None` (the `use` line lives in the active
/// source). For a top-level `use` the wrapper is a root with no further
/// ancestors, so the plain walk-up-to-`use_id == None` rule bottoms out at
/// `None` and the user can never select the import. The fallback below
/// detects the wrapper by the origin transition (parent `origin = None`,
/// child `origin = Some(...)`) and returns it when no fully use-free
/// ancestor exists.
pub(crate) fn redirect_pick(scene: &SceneGraph, id: NodeId) -> Option<NodeId> {
    let node = scene.nodes.get(id.0 as usize)?;
    // A non-editable node belongs to a generated subtree (cave / array /
    // mirror / CSG output) that a rebuild regenerates from its source. The
    // user can't act on it directly, so redirect the pick up to the nearest
    // editable ancestor that owns a source span — the generator wrapper /
    // operator node they CAN edit. Without this, clicking a cave's rock or
    // decoration geometry dead-ends on a non-editable child and the cave's
    // wrapper-level controls (the debug toggles) stay unreachable.
    if !node.editable {
        let mut cur = node.parent;
        while let Some(pid) = cur {
            let parent = scene.nodes.get(pid.0 as usize)?;
            if parent.editable && parent.source_span.is_some() {
                return Some(pid);
            }
            cur = parent.parent;
        }
        // No editable ancestor (a bare non-editable root). Fall through to
        // the use_id logic below, which bails to None / self as appropriate.
    }
    if node.use_id.is_none() {
        return Some(id);
    }
    let mut import_wrapper: Option<NodeId> = None;
    let mut prev_origin_some = node.origin.is_some();
    let mut cur = node.parent;
    while let Some(pid) = cur {
        let parent = scene.nodes.get(pid.0 as usize)?;
        if parent.use_id.is_none() {
            return Some(pid);
        }
        if parent.origin.is_none() && prev_origin_some && import_wrapper.is_none() {
            import_wrapper = Some(pid);
        }
        prev_origin_some = parent.origin.is_some();
        cur = parent.parent;
    }
    // Fallback: a node authored in the active source (origin=None) but
    // expanded out of a `use "local_module" ()` call has `use_id=Some` and
    // sits at scene root with no wrapper to walk up to. Its `source_span`
    // still points at editable bytes in the active file (the module body
    // is right there in the same `.mog`), so let the click select it
    // directly. Imported nodes (origin=Some(path)) keep bailing to None —
    // their span is in another file and a span-based set_attr would
    // splice into the wrong source.
    import_wrapper.or_else(|| {
        if node.origin.is_none() {
            Some(id)
        } else {
            None
        }
    })
}

/// True when `id` is the synthesised wrapper group of a `use "..."` of an
/// imported file. Such wrappers carry `use_id = Some(...)` (they open a
/// new frame) but `origin = None` (the `use` was authored in the active
/// source) and contain at least one descendant whose `origin` is `Some`
/// (the imported body). The viewport gizmo and inspector treat them as
/// editable even though `use_id` is set, because the wrapper's source
/// span points at the active `.mog` and a `pos=` writeback round-trips
/// cleanly through `set_attr` on the `use` line.
pub fn is_import_wrapper(scene: &SceneGraph, id: NodeId) -> bool {
    let Some(node) = scene.nodes.get(id.0 as usize) else {
        return false;
    };
    if node.use_id.is_none() || node.origin.is_some() {
        return false;
    }
    has_imported_descendant(scene, id)
}

fn has_imported_descendant(scene: &SceneGraph, id: NodeId) -> bool {
    let Some(node) = scene.nodes.get(id.0 as usize) else {
        return false;
    };
    for &cid in &node.children {
        let Some(child) = scene.nodes.get(cid.0 as usize) else {
            continue;
        };
        if child.origin.is_some() {
            return true;
        }
        if has_imported_descendant(scene, cid) {
            return true;
        }
    }
    false
}
