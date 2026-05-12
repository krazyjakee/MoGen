use std::time::Instant;

use eframe::egui;

use crate::app::types::UndoKey;
use crate::app::MogenStudioApp;
use crate::edit;

/// Render the Duplicate / Delete footer. Both buttons rewrite the node's
/// full source span (rather than a single attr), so they go through the
/// raw source-edit pipeline instead of `PendingEdit`.
pub(super) fn render(
    ui: &mut egui::Ui,
    app: &mut MogenStudioApp,
    i: usize,
    node_span: Option<mogen_core::Span>,
) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        let span_ok = node_span.is_some();
        if ui
            .add_enabled(span_ok, egui::Button::new("Duplicate"))
            .on_hover_text("Duplicate this node in the DSL source")
            .clicked()
        {
            if let Some(span) = node_span {
                let before = app.files[i].source.clone();
                let new_src = edit::duplicate_node(&before, span);
                {
                    let f = &mut app.files[i];
                    f.source = new_src;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
                // Discrete click — never coalesce with a prior entry.
                app.break_undo_chain(i);
                app.push_undo(
                    i,
                    before,
                    UndoKey {
                        surface: "inspector-action",
                        attr: None,
                        node_path: Vec::new(),
                    },
                );
            }
        }
        // Multi-select aware delete: removes every node in the current
        // selection, not just the inspector's primary. Spans come from
        // the last compile result and are applied right-to-left so
        // earlier byte offsets stay valid as later regions are removed —
        // same reason `drain_viewport_edits` does the sort. Disabled
        // when no spans resolve (rare; only in stale-selection-after-
        // failed-compile cases).
        let all_selected = app.viewer.all_selected();
        let delete_label = if all_selected.len() > 1 {
            format!("Delete {} nodes", all_selected.len())
        } else {
            "Delete".to_string()
        };
        let mut delete_spans: Vec<mogen_core::Span> = Vec::new();
        if let Some(result) = &app.files[i].last_result {
            for n in &all_selected {
                if let Some(s) = result
                    .node_spans
                    .get(n.0 as usize)
                    .and_then(|s| *s)
                {
                    delete_spans.push(s);
                }
            }
        }
        // If the user shift-selected a parent and a descendant, the
        // parent's delete already removes the descendant — keep only
        // the outermost spans so the right-to-left pass below can't
        // fire a stale child-span delete after its parent is gone.
        let mut delete_spans = edit::dedup_contained_spans(&delete_spans);
        delete_spans.sort_by(|a, b| b.start.cmp(&a.start));
        let delete_ok = !delete_spans.is_empty();
        let hover_text = if all_selected.len() > 1 {
            "Remove every selected node from the DSL source"
        } else {
            "Remove this node from the DSL source"
        };
        if ui
            .add_enabled(delete_ok, egui::Button::new(delete_label))
            .on_hover_text(hover_text)
            .clicked()
            && delete_ok
        {
            let before = app.files[i].source.clone();
            let mut src = before.clone();
            for span in &delete_spans {
                src = edit::delete_node(&src, *span);
            }
            {
                let f = &mut app.files[i];
                f.source = src;
                f.dirty = f.source != f.last_saved_source;
                f.needs_compile = true;
                f.last_edit_at = Some(Instant::now());
            }
            app.break_undo_chain(i);
            app.push_undo(
                i,
                before,
                UndoKey {
                    surface: "inspector-action",
                    attr: None,
                    node_path: Vec::new(),
                },
            );
            app.viewer.set_primary_selection(None);
        }
    });
}
