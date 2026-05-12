use eframe::egui;

use crate::viewer::PendingEdit;

/// Render the material combo box. Returns `true` when the user picked the
/// `(inherit)` row and the caller should `delete_attr("material")`.
pub(super) fn render(
    ui: &mut egui::Ui,
    node: &mogen_core::SceneNode,
    scene: &mogen_core::SceneGraph,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
) -> bool {
    if node.light.is_some() {
        return false;
    }
    let mat_names: Vec<String> = scene.materials.iter().map(|m| m.name.clone()).collect();
    if mat_names.is_empty() {
        return false;
    }
    let current_mat: Option<String> = node
        .material
        .and_then(|id| scene.materials.get(id.0 as usize))
        .map(|m| m.name.clone());
    let mut wants_clear_material = false;
    ui.add_space(8.0);
    ui.separator();
    ui.label(egui::RichText::new("Material").strong());
    ui.horizontal(|ui| {
        let label = current_mat
            .clone()
            .unwrap_or_else(|| "(inherit)".to_string());
        egui::ComboBox::from_id_salt(("inspector_material", node_id.0))
            .selected_text(label)
            .show_ui(ui, |ui| {
                // "(inherit)" clears `material=` and lets the parent's
                // material flow down — the lowering pass already
                // propagates parent material when child omits it.
                let none_selected = current_mat.is_none();
                if ui
                    .selectable_label(none_selected, "(inherit)")
                    .clicked()
                    && !none_selected
                {
                    wants_clear_material = true;
                }
                for name in &mat_names {
                    let selected = current_mat.as_deref() == Some(name.as_str());
                    if ui
                        .selectable_label(selected, name)
                        .clicked()
                        && !selected
                    {
                        edits.push(PendingEdit::SetAttrCanonical {
                            node: node_id,
                            attr: "material".into(),
                            value: format!("\"{name}\""),
                            delete: Vec::new(),
                        });
                    }
                }
            });
    });
    wants_clear_material
}
