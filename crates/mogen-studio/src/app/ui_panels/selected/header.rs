use eframe::egui;

use crate::app::MogenStudioApp;

/// Renders the multi-select hint (when more than one node is selected) and the
/// Name / Kind / Source rows at the top of the inspector.
pub(super) fn render(ui: &mut egui::Ui, app: &MogenStudioApp, node: &mogen_core::SceneNode) {
    // Multi-select hint: tell the user the inspector is editing the
    // primary (most-recently-selected) node; the others come along for
    // delete/highlight only. Without this, a shift-click that adds to
    // the selection looks identical in the inspector and the user can't
    // tell the inspector is intentionally pinned to the primary.
    let selected_count = app.viewer.all_selected().len();
    if selected_count > 1 {
        ui.colored_label(
            egui::Color32::from_rgb(170, 200, 240),
            format!("{selected_count} nodes selected — editing primary"),
        )
        .on_hover_text(
            "Shift/Cmd-click adds nodes to the selection. The inspector \
             shows the most recently clicked node; Delete removes every \
             selected node.",
        );
        // Spell out which node is primary vs secondary so the user can
        // see at a glance whose attributes the inspector is editing.
        // Pulled from the viewer-side scene snapshot rather than the
        // file's last_result so the names line up with what's painted
        // in the viewport.
        if let Some(scene_arc) = app
            .files
            .get(app.active)
            .and_then(|f| f.last_result.as_ref())
            .and_then(|r| r.scene.as_ref())
        {
            let all = app.viewer.all_selected();
            ui.horizontal_wrapped(|ui| {
                for (idx, id) in all.iter().enumerate() {
                    let is_primary = idx + 1 == all.len();
                    let name = scene_arc
                        .nodes
                        .get(id.0 as usize)
                        .map(|n| n.name.as_str())
                        .unwrap_or("(stale)");
                    let prefix = if is_primary { "★ " } else { "" };
                    let label = format!("{prefix}{name}");
                    let rich = if is_primary {
                        egui::RichText::new(label).strong()
                    } else {
                        egui::RichText::new(label).weak()
                    };
                    ui.label(rich);
                }
            });
        }
        ui.add_space(4.0);
    }

    ui.horizontal(|ui| {
        ui.label("Name:");
        ui.monospace(&node.name);
    });
    ui.horizontal(|ui| {
        ui.label("Kind:");
        ui.monospace(&node.kind);
    });
    if let Some(p) = &node.origin {
        // Make the cross-file provenance discoverable: scoping the
        // sidebar to a specific import is otherwise invisible — without
        // this badge a user wouldn't know why Materials/Animation just
        // grew when they clicked an imported node.
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("import");
        ui.horizontal(|ui| {
            ui.label("Source:");
            ui.colored_label(
                egui::Color32::from_rgb(170, 200, 240),
                format!("⤴ {stem}"),
            )
            .on_hover_text(format!("Imported from {}", p.display()));
        });
    }
}
