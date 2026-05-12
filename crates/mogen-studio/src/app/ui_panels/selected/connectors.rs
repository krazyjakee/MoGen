use eframe::egui;

/// Render the read-only Connectors table. Synthesised AABB connectors (the
/// six face anchors every mesh gets for free) are tagged with `ⓢ` so the
/// user can see at a glance which entries the lowering pass added vs. what
/// they authored.
pub(super) fn render(
    ui: &mut egui::Ui,
    connectors: &[mogen_core::Connector],
    node_id: mogen_core::NodeId,
) {
    if connectors.is_empty() {
        return;
    }
    ui.add_space(8.0);
    ui.separator();
    egui::CollapsingHeader::new(format!("Connectors ({})", connectors.len()))
        .id_salt(("inspector_connectors", node_id.0))
        .default_open(false)
        .show(ui, |ui| {
            egui::Grid::new(("inspector_conn_grid", node_id.0))
                .num_columns(3)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("name").strong().weak());
                    ui.label(egui::RichText::new("tag").strong().weak());
                    ui.label(egui::RichText::new("pos").strong().weak());
                    ui.end_row();
                    for c in connectors {
                        let synthesised = c.source_span.is_none();
                        let name_label = if synthesised {
                            egui::RichText::new(format!("{} ⓢ", c.name)).weak()
                        } else {
                            egui::RichText::new(&c.name).monospace()
                        };
                        ui.label(name_label).on_hover_text(if synthesised {
                            "Synthesised from the AABB face anchors — no DSL declaration to edit."
                        } else {
                            "Authored connector — edit the `connector` line in the source."
                        });
                        ui.label(egui::RichText::new(&c.tag).monospace());
                        ui.label(format!(
                            "[{:.2}, {:.2}, {:.2}]",
                            c.pos.x, c.pos.y, c.pos.z
                        ));
                        ui.end_row();
                    }
                });
        });
}
