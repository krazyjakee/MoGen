use eframe::egui;

/// Render the mesh `src=` browse button for `kind="mesh"` nodes. Returns
/// `true` when the user clicked Browse, so the caller can open the file
/// dialog (which needs `&mut self` for `self.files[i]` lookups).
pub(super) fn render(
    ui: &mut egui::Ui,
    node_kind: &str,
    source: &str,
    span: Option<mogen_core::Span>,
) -> bool {
    if node_kind != "mesh" {
        return false;
    }
    let Some(span) = span else {
        return false;
    };
    let mut wants_pick = false;
    ui.add_space(8.0);
    ui.separator();
    ui.label(egui::RichText::new("Mesh source").strong());
    let cur_src = crate::edit::get_attr(source, span, "src")
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_default();
    ui.horizontal(|ui| {
        if cur_src.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                "(no path set)",
            );
        } else {
            ui.label(
                egui::RichText::new(&cur_src)
                    .monospace()
                    .weak(),
            )
            .on_hover_text(&cur_src);
        }
        if ui
            .small_button("Browse…")
            .on_hover_text(
                "Pick a .glb file. Path is stored relative to the .mog when possible.",
            )
            .clicked()
        {
            wants_pick = true;
        }
    });
    wants_pick
}
