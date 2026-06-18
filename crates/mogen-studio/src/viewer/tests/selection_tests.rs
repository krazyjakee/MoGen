//! Tests for selection / pick redirect / source-offset lookup / cycling
//! drill-down. Shared scene-builder helpers live at the top.

use super::super::state::{
    find_deepest_node_at_offset, find_use_at_offset, gizmo_handles_supported, is_import_wrapper,
    node_path, redirect_pick, replace_selection, replace_selection_cycling, resolve_node_path,
    toggle_selection, ViewerState, PICK_CYCLE_RADIUS_PX,
};
use eframe::egui;
use mogen_core::{NodeId, SceneGraph, Span, Transform};
use std::path::PathBuf;
use std::sync::Arc;

/// Build a scene mirroring the office assetpack pattern:
///   group "lptp" { use "laptop" }
/// — a user-authored wrapper group with one imported child carrying a
/// non-`None` `use_id` AND `origin = Some(path)` (the latter is what
/// distinguishes a real cross-file import from a same-file `use`
/// expansion: only imported nodes have a foreign source path stamped
/// on them by `set_origin_recursive`). The wrapper has
/// `use_id = None`; the imported child has `use_id = Some(7)` and
/// `origin = Some("laptop.mog")`. Returns `(wrapper_id, imported_id)`.
fn scene_with_imported_child() -> (SceneGraph, NodeId, NodeId) {
    let mut scene = SceneGraph::new();
    let wrapper = scene.add_root("lptp", "group", Transform::IDENTITY);
    let imported = scene.add_child(wrapper, "laptop_body", "box", Transform::IDENTITY);
    scene.nodes[imported.0 as usize].use_id = Some(7);
    scene.nodes[imported.0 as usize].origin = Some(PathBuf::from("laptop.mog"));
    (scene, wrapper, imported)
}

#[test]
fn redirect_pick_returns_user_authored_node_unchanged() {
    let (scene, wrapper, _) = scene_with_imported_child();
    assert_eq!(redirect_pick(&scene, wrapper), Some(wrapper));
}

#[test]
fn redirect_pick_walks_up_to_wrapper_for_imported_child() {
    let (scene, wrapper, imported) = scene_with_imported_child();
    assert_eq!(redirect_pick(&scene, imported), Some(wrapper));
}

#[test]
fn redirect_pick_walks_through_nested_imported_chain() {
    // Outer wrapper → imported group → imported leaf. Both imported
    // nodes share the same `use_id` (matches how nested module bodies
    // are flattened by `expand_node_into`). The redirect must skip past
    // the inner imported group, not stop at it.
    let mut scene = SceneGraph::new();
    let wrapper = scene.add_root("dsk", "group", Transform::IDENTITY);
    let inner = scene.add_child(wrapper, "desk_top", "group", Transform::IDENTITY);
    scene.nodes[inner.0 as usize].use_id = Some(3);
    let leaf = scene.add_child(inner, "desk_top_box", "box", Transform::IDENTITY);
    scene.nodes[leaf.0 as usize].use_id = Some(3);
    assert_eq!(redirect_pick(&scene, leaf), Some(wrapper));
}

#[test]
fn redirect_pick_returns_none_when_no_user_authored_ancestor() {
    // `scene { use "desk" }` with no wrapping group: the imported node
    // is a root, every parent walk halts immediately with no
    // user-authored wrapper. The redirect bails to `None` so picking
    // doesn't latch onto a node we can't safely write back. `origin`
    // must be `Some(...)` here — that's what marks the node as
    // imported (its `source_span` lives in another file). A
    // locally-expanded `use` of a module declared in the same `.mog`
    // would have `origin = None` and the redirect would return self.
    let mut scene = SceneGraph::new();
    let imported = scene.add_root("desk_top", "box", Transform::IDENTITY);
    scene.nodes[imported.0 as usize].use_id = Some(1);
    scene.nodes[imported.0 as usize].origin = Some(PathBuf::from("desk.mog"));
    assert_eq!(redirect_pick(&scene, imported), None);
}

#[test]
fn redirect_pick_returns_self_for_local_module_top_level_node() {
    // `module "outfit" () { box "panel" (...) }` followed by
    // `use "outfit" ()` — the expanded `panel` lands at scene root with
    // `use_id = Some(...)` and `origin = None` (its `source_span`
    // points at editable bytes in the active file). The redirect must
    // return the panel itself; otherwise the viewport click wipes the
    // selection and the user can't grab the gizmo handle on local
    // outfit / clothing modules.
    let mut scene = SceneGraph::new();
    let panel = scene.add_root("panel", "box", Transform::IDENTITY);
    scene.nodes[panel.0 as usize].use_id = Some(7);
    // origin stays None — local module body sits in the active source.
    assert_eq!(redirect_pick(&scene, panel), Some(panel));
}

#[test]
fn redirect_pick_walks_non_editable_generated_child_up_to_editable_wrapper() {
    // Mirrors a `cave` subtree: an editable wrapper (with a source span)
    // owns a non-editable generated child (the rock shell) which in turn
    // owns a non-editable nested marker (a POI sphere). Clicking either
    // generated node must redirect to the wrapper — the only node carrying
    // the cave's editable header (and its debug toggles).
    let mut scene = SceneGraph::new();
    let wrapper = scene.add_root("den", "cave", Transform::IDENTITY);
    scene.set_source_span(wrapper, Span { start: 0, end: 40 });
    let rock = scene.add_child(wrapper, "den_rock", "mesh", Transform::IDENTITY);
    scene.nodes[rock.0 as usize].editable = false;
    let marker = scene.add_child(rock, "mushroom_spot", "empty", Transform::IDENTITY);
    scene.nodes[marker.0 as usize].editable = false;
    assert_eq!(redirect_pick(&scene, rock), Some(wrapper));
    assert_eq!(redirect_pick(&scene, marker), Some(wrapper));
    // The editable wrapper itself is returned unchanged.
    assert_eq!(redirect_pick(&scene, wrapper), Some(wrapper));
}

#[test]
fn replace_selection_redirects_pick_to_wrapper() {
    // Picking the imported child must land selection on the wrapper —
    // the gizmo + inspector both read `st.selected`, so this is what
    // makes the visual editor "edit the group, not the import".
    let (scene, wrapper, imported) = scene_with_imported_child();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    replace_selection(&mut st, Some(imported));
    assert_eq!(st.selected, vec![wrapper]);
    assert_eq!(
        st.selected_paths,
        vec![vec![("lptp".to_string(), 0u32)]],
    );
}

#[test]
fn replace_selection_clears_selection_when_redirect_finds_no_wrapper() {
    // Bare imported root (no enclosing user-authored group): the
    // redirect returns `None`, so the viewer should clear its
    // selection rather than latch onto an un-editable node. Imported
    // nodes carry `origin = Some(path)`; without that flag the
    // redirect treats the node as a local module body and returns
    // self.
    let mut scene = SceneGraph::new();
    let imported = scene.add_root("desk_top", "box", Transform::IDENTITY);
    scene.nodes[imported.0 as usize].use_id = Some(1);
    scene.nodes[imported.0 as usize].origin = Some(PathBuf::from("desk.mog"));
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    replace_selection(&mut st, Some(imported));
    assert!(st.selected.is_empty());
    assert!(st.selected_paths.is_empty());
}

/// Build a tiny scene whose nodes carry hand-rolled `source_span`s so
/// the offset-lookup tests can pick precise byte positions without
/// running the real DSL parser. Layout (byte offsets in brackets):
///   `[0..40] group "outer" { [10..30] box "inner" { } }`
/// — a parent span that fully contains a child span. The third node
/// (`imported`) is a sibling of `inner` whose `origin = Some(...)` to
/// exercise the imported-skip rule.
fn scene_with_overlapping_spans() -> (SceneGraph, NodeId, NodeId, NodeId) {
    let mut scene = SceneGraph::new();
    let outer = scene.add_root("outer", "group", Transform::IDENTITY);
    let inner = scene.add_child(outer, "inner", "box", Transform::IDENTITY);
    let imported = scene.add_child(outer, "imported", "box", Transform::IDENTITY);
    scene.nodes[outer.0 as usize].source_span = Some(Span::new(0, 40));
    scene.nodes[inner.0 as usize].source_span = Some(Span::new(10, 30));
    // Picked to overlap `inner`'s range so the deepest-by-length tiebreak
    // genuinely depends on whether we let an imported span participate.
    scene.nodes[imported.0 as usize].source_span = Some(Span::new(15, 25));
    scene.nodes[imported.0 as usize].origin = Some(PathBuf::from("other.mog"));
    (scene, outer, inner, imported)
}

#[test]
fn find_deepest_node_at_offset_picks_smallest_containing_span() {
    let (scene, _outer, inner, _imported) = scene_with_overlapping_spans();
    assert_eq!(find_deepest_node_at_offset(&scene, 20), Some(inner));
}

#[test]
fn find_deepest_node_at_offset_skips_imported_nodes() {
    let (scene, _outer, inner, _imported) = scene_with_overlapping_spans();
    assert_eq!(find_deepest_node_at_offset(&scene, 20), Some(inner));
}

#[test]
fn find_deepest_node_at_offset_falls_back_to_outer_when_only_outer_contains() {
    let (scene, outer, _inner, _imported) = scene_with_overlapping_spans();
    assert_eq!(find_deepest_node_at_offset(&scene, 5), Some(outer));
}

#[test]
fn find_deepest_node_at_offset_returns_none_outside_every_span() {
    let (scene, _outer, _inner, _imported) = scene_with_overlapping_spans();
    assert_eq!(find_deepest_node_at_offset(&scene, 1000), None);
}

#[test]
fn find_use_at_offset_resolves_use_line_to_imported_root() {
    let source = "scene {\n  use \"leg\" (h=0.5)\n}\n";
    let mut scene = SceneGraph::new();
    let root = scene.add_root("scene", "scene", Transform::IDENTITY);
    let imported_root = scene.add_child(root, "leg_root", "group", Transform::IDENTITY);
    scene.nodes[imported_root.0 as usize].origin = Some(PathBuf::from("stdlib/leg.mog"));
    let imported_leaf = scene.add_child(imported_root, "leg_mesh", "box", Transform::IDENTITY);
    scene.nodes[imported_leaf.0 as usize].origin = Some(PathBuf::from("stdlib/leg.mog"));

    let use_offset = source.find("use \"leg\"").unwrap() + 5;
    assert_eq!(
        find_use_at_offset(&scene, source, use_offset),
        Some(imported_root)
    );
}

#[test]
fn find_use_at_offset_returns_none_when_offset_outside_any_use() {
    let source = "scene {\n  use \"leg\" (h=0.5)\n}\n";
    let mut scene = SceneGraph::new();
    let root = scene.add_root("scene", "scene", Transform::IDENTITY);
    let imported_root = scene.add_child(root, "leg_root", "group", Transform::IDENTITY);
    scene.nodes[imported_root.0 as usize].origin = Some(PathBuf::from("stdlib/leg.mog"));
    assert_eq!(find_use_at_offset(&scene, source, 0), None);
}

#[test]
fn find_deepest_node_at_offset_treats_span_end_as_exclusive() {
    let mut scene = SceneGraph::new();
    let a = scene.add_root("a", "box", Transform::IDENTITY);
    let b = scene.add_root("b", "box", Transform::IDENTITY);
    scene.nodes[a.0 as usize].source_span = Some(Span::new(0, 10));
    scene.nodes[b.0 as usize].source_span = Some(Span::new(10, 20));
    assert_eq!(find_deepest_node_at_offset(&scene, 9), Some(a));
    assert_eq!(find_deepest_node_at_offset(&scene, 10), Some(b));
}

/// Three-sibling scene with three user-authored boxes under one root.
fn scene_with_three_siblings() -> (SceneGraph, [NodeId; 3]) {
    let mut scene = SceneGraph::new();
    let root = scene.add_root("scene", "group", Transform::IDENTITY);
    let a = scene.add_child(root, "a", "box", Transform::IDENTITY);
    let b = scene.add_child(root, "b", "box", Transform::IDENTITY);
    let c = scene.add_child(root, "c", "box", Transform::IDENTITY);
    (scene, [a, b, c])
}

#[test]
fn node_path_round_trips_same_named_replicas() {
    let mut scene = SceneGraph::new();
    let root = scene.add_root("scene", "group", Transform::IDENTITY);
    let a = scene.add_child(root, "leg", "box", Transform::IDENTITY);
    let b = scene.add_child(root, "leg", "box", Transform::IDENTITY);
    let c = scene.add_child(root, "leg", "box", Transform::IDENTITY);

    let pa = node_path(&scene, a).unwrap();
    let pb = node_path(&scene, b).unwrap();
    let pc = node_path(&scene, c).unwrap();
    assert_eq!(pa.last().unwrap().1, 0);
    assert_eq!(pb.last().unwrap().1, 1);
    assert_eq!(pc.last().unwrap().1, 2);

    assert_eq!(resolve_node_path(&scene, &pa), Some(a));
    assert_eq!(resolve_node_path(&scene, &pb), Some(b));
    assert_eq!(resolve_node_path(&scene, &pc), Some(c));
}

#[test]
fn toggle_selection_appends_new_node_as_primary() {
    let (scene, [a, b, _]) = scene_with_three_siblings();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    replace_selection(&mut st, Some(a));
    toggle_selection(&mut st, b);
    assert_eq!(st.selected, vec![a, b]);
    assert_eq!(st.primary_selected(), Some(b));
}

#[test]
fn toggle_selection_removes_already_selected_node() {
    let (scene, [a, b, _]) = scene_with_three_siblings();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    replace_selection(&mut st, Some(a));
    toggle_selection(&mut st, b);
    toggle_selection(&mut st, b);
    assert_eq!(st.selected, vec![a]);
    assert_eq!(st.primary_selected(), Some(a));
}

#[test]
fn toggle_selection_removing_primary_promotes_previous_to_primary() {
    let (scene, [a, b, _]) = scene_with_three_siblings();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    replace_selection(&mut st, Some(a));
    toggle_selection(&mut st, b);
    assert_eq!(st.primary_selected(), Some(b));
    toggle_selection(&mut st, b);
    assert_eq!(st.primary_selected(), Some(a));
    assert_eq!(st.selected.len(), 1);
}

#[test]
fn toggle_selection_redirects_through_imported_subtree() {
    let (scene, wrapper, imported) = scene_with_imported_child();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    toggle_selection(&mut st, imported);
    assert_eq!(st.selected, vec![wrapper]);
}

#[test]
fn gizmo_handles_refused_for_imported_subtree() {
    let (scene, _, imported) = scene_with_imported_child();
    assert!(!gizmo_handles_supported(
        &scene,
        &[],
        imported,
        crate::gizmo::GizmoMode::Translate,
    ));
}

#[test]
fn gizmo_handles_allowed_on_user_wrapper_around_use() {
    let (scene, wrapper, _) = scene_with_imported_child();
    assert!(gizmo_handles_supported(
        &scene,
        &[],
        wrapper,
        crate::gizmo::GizmoMode::Translate,
    ));
}

#[test]
fn gizmo_handles_allowed_on_local_module_expansion() {
    let mut scene = SceneGraph::new();
    let panel = scene.add_root("panel", "box", Transform::IDENTITY);
    scene.nodes[panel.0 as usize].use_id = Some(7);
    assert!(gizmo_handles_supported(
        &scene,
        &[],
        panel,
        crate::gizmo::GizmoMode::Translate,
    ));
}

/// Mirror the office assetpack pattern: a top-level
/// `use "watercooler" (pos=...)` of an imported file. After expansion
/// the synthesised wrapper group has `use_id = Some(...)` (it opens a
/// new frame) and `origin = None` (the `use` line lives in the active
/// source); its imported body has `use_id = Some(...)` (same frame)
/// and `origin = Some("watercooler.mog")`.
fn scene_with_imported_use_wrapper() -> (SceneGraph, NodeId, NodeId) {
    let mut scene = SceneGraph::new();
    let wrapper = scene.add_root("watercooler", "group", Transform::IDENTITY);
    scene.nodes[wrapper.0 as usize].use_id = Some(2);
    scene.nodes[wrapper.0 as usize].origin = None;
    let body = scene.add_child(wrapper, "lower_cabinet", "post", Transform::IDENTITY);
    scene.nodes[body.0 as usize].use_id = Some(2);
    scene.nodes[body.0 as usize].origin = Some(PathBuf::from("watercooler.mog"));
    (scene, wrapper, body)
}

#[test]
fn redirect_pick_lands_on_import_wrapper_when_wrapper_is_a_root() {
    let (scene, wrapper, body) = scene_with_imported_use_wrapper();
    assert_eq!(redirect_pick(&scene, body), Some(wrapper));
}

#[test]
fn is_import_wrapper_detects_use_wrapper_for_imported_file() {
    let (scene, wrapper, body) = scene_with_imported_use_wrapper();
    assert!(is_import_wrapper(&scene, wrapper));
    assert!(!is_import_wrapper(&scene, body));
}

#[test]
fn is_import_wrapper_rejects_local_use_wrapper() {
    let mut scene = SceneGraph::new();
    let wrapper = scene.add_root("legs", "group", Transform::IDENTITY);
    scene.nodes[wrapper.0 as usize].use_id = Some(5);
    let body = scene.add_child(wrapper, "leg", "cylinder", Transform::IDENTITY);
    scene.nodes[body.0 as usize].use_id = Some(5);
    let _ = body;
    assert!(!is_import_wrapper(&scene, wrapper));
}

#[test]
fn gizmo_handles_allowed_on_import_wrapper() {
    let (scene, wrapper, _) = scene_with_imported_use_wrapper();
    assert!(gizmo_handles_supported(
        &scene,
        &[],
        wrapper,
        crate::gizmo::GizmoMode::Translate,
    ));
}

#[test]
fn gizmo_handles_still_refused_on_imported_body_inside_wrapper() {
    let (scene, _, body) = scene_with_imported_use_wrapper();
    assert!(!gizmo_handles_supported(
        &scene,
        &[],
        body,
        crate::gizmo::GizmoMode::Translate,
    ));
}

#[test]
fn replace_selection_lands_on_import_wrapper() {
    let (scene, wrapper, body) = scene_with_imported_use_wrapper();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    replace_selection(&mut st, Some(body));
    assert_eq!(st.selected, vec![wrapper]);
}

#[test]
fn gizmo_handles_supported_for_relative_placed_in_every_mode() {
    let mut scene = SceneGraph::new();
    let parent = scene.add_root("group", "group", Transform::IDENTITY);
    let child = scene.add_child(parent, "tier2", "box", Transform::IDENTITY);
    scene.nodes[child.0 as usize].relative_placed = true;
    for mode in [
        crate::gizmo::GizmoMode::Translate,
        crate::gizmo::GizmoMode::Rotate,
        crate::gizmo::GizmoMode::Scale,
    ] {
        assert!(
            gizmo_handles_supported(&scene, &[], child, mode),
            "expected handles for mode {mode:?}"
        );
    }
}

/// Three-level scene: outer user-authored group containing a `use`
/// wrapper of an imported file. Mirrors a real `.mog` whose top
/// declares `group "scene" { use "trash_bin" (pos=...) }`.
fn scene_with_use_inside_outer_group() -> (SceneGraph, NodeId, NodeId, NodeId) {
    let mut scene = SceneGraph::new();
    let outer = scene.add_root("scene", "group", Transform::IDENTITY);
    let wrapper = scene.add_child(outer, "trash_bin", "group", Transform::IDENTITY);
    scene.nodes[wrapper.0 as usize].use_id = Some(11);
    scene.nodes[wrapper.0 as usize].origin = None;
    let body = scene.add_child(wrapper, "bin_body", "cylinder", Transform::IDENTITY);
    scene.nodes[body.0 as usize].use_id = Some(11);
    scene.nodes[body.0 as usize].origin = Some(PathBuf::from("trash_bin.mog"));
    (scene, outer, wrapper, body)
}

#[test]
fn cycling_first_click_matches_today_redirect_pick() {
    let (scene, outer, _wrapper, body) = scene_with_use_inside_outer_group();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    replace_selection_cycling(&mut st, body, egui::pos2(100.0, 100.0), 1.0);
    assert_eq!(st.selected, vec![outer]);
    assert_eq!(st.pick_cycle.map(|c| c.depth), Some(0));
}

#[test]
fn cycling_second_click_drills_to_use_wrapper() {
    let (scene, _outer, wrapper, body) = scene_with_use_inside_outer_group();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    let cursor = egui::pos2(100.0, 100.0);
    replace_selection_cycling(&mut st, body, cursor, 1.0);
    replace_selection_cycling(&mut st, body, cursor, 1.0);
    assert_eq!(st.selected, vec![wrapper]);
    assert_eq!(st.pick_cycle.map(|c| c.depth), Some(1));
}

#[test]
fn cycling_clamps_at_editability_boundary() {
    let (scene, _outer, wrapper, body) = scene_with_use_inside_outer_group();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    let cursor = egui::pos2(100.0, 100.0);
    replace_selection_cycling(&mut st, body, cursor, 1.0);
    replace_selection_cycling(&mut st, body, cursor, 1.0);
    replace_selection_cycling(&mut st, body, cursor, 1.0);
    assert_eq!(st.selected, vec![wrapper]);
    assert_eq!(st.pick_cycle.map(|c| c.depth), Some(1));
}

#[test]
fn cycling_resets_when_cursor_moves_past_radius() {
    let (scene, outer, _wrapper, body) = scene_with_use_inside_outer_group();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    replace_selection_cycling(&mut st, body, egui::pos2(100.0, 100.0), 1.0);
    let far = egui::pos2(100.0 + PICK_CYCLE_RADIUS_PX + 1.0, 100.0);
    replace_selection_cycling(&mut st, body, far, 1.0);
    assert_eq!(st.selected, vec![outer]);
    assert_eq!(st.pick_cycle.map(|c| c.depth), Some(0));
}

#[test]
fn cycling_preserves_state_when_cursor_drifts_within_radius() {
    let (scene, _outer, wrapper, body) = scene_with_use_inside_outer_group();
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    replace_selection_cycling(&mut st, body, egui::pos2(100.0, 100.0), 1.0);
    let drifted = egui::pos2(100.0 + PICK_CYCLE_RADIUS_PX - 0.5, 100.0);
    replace_selection_cycling(&mut st, body, drifted, 1.0);
    assert_eq!(st.selected, vec![wrapper]);
    assert_eq!(st.pick_cycle.map(|c| c.depth), Some(1));
}

#[test]
fn cycling_resets_when_leaf_changes() {
    let (mut scene, outer, _wrapper, body) = scene_with_use_inside_outer_group();
    let other = scene.add_child(outer, "other", "box", Transform::IDENTITY);
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    let cursor = egui::pos2(100.0, 100.0);
    replace_selection_cycling(&mut st, body, cursor, 1.0);
    replace_selection_cycling(&mut st, body, cursor, 1.0);
    replace_selection_cycling(&mut st, other, cursor, 1.0);
    assert_eq!(st.selected, vec![other]);
    assert_eq!(st.pick_cycle.map(|c| c.depth), Some(0));
}

#[test]
fn cycling_clears_selection_when_redirect_finds_no_wrapper() {
    let mut scene = SceneGraph::new();
    let imported = scene.add_root("desk_top", "box", Transform::IDENTITY);
    scene.nodes[imported.0 as usize].use_id = Some(1);
    scene.nodes[imported.0 as usize].origin = Some(PathBuf::from("desk.mog"));
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    replace_selection_cycling(&mut st, imported, egui::pos2(100.0, 100.0), 1.0);
    assert!(st.selected.is_empty());
    assert!(st.pick_cycle.is_none());
}

#[test]
fn cycling_on_plain_user_authored_leaf_is_a_no_op() {
    let mut scene = SceneGraph::new();
    let group = scene.add_root("scene", "group", Transform::IDENTITY);
    let leaf = scene.add_child(group, "box", "box", Transform::IDENTITY);
    let mut st = ViewerState::default();
    st.scene = Some(Arc::new(scene));
    let cursor = egui::pos2(100.0, 100.0);
    replace_selection_cycling(&mut st, leaf, cursor, 1.0);
    replace_selection_cycling(&mut st, leaf, cursor, 1.0);
    assert_eq!(st.selected, vec![leaf]);
    assert_eq!(st.pick_cycle.map(|c| c.depth), Some(0));
}
