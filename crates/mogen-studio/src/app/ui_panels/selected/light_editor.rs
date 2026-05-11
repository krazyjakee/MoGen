use eframe::egui;

use crate::app::util::format_inspector_scalar;
use crate::viewer::PendingEdit;

/// Render the light editor section (kind/colour/intensity + kind-conditional
/// range / cone angles). Returns `true` if the user clicked the "✕" next to
/// the range field — the caller performs the `delete_attr` + undo push, since
/// `PendingEdit` can only set+delete-shadows, not delete a primary attr.
///
/// A kind switch carries a `delete` list so attrs that no longer apply
/// (`range` for directional, `inner_cone` / `outer_cone` for non-spot) don't
/// sit around poisoning the next compile with a validation error.
pub(super) fn render(
    ui: &mut egui::Ui,
    light: &mogen_core::Light,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
) -> bool {
    use mogen_core::LightKind;

    let kind = light.kind;
    let light_color = light.color;
    let light_intensity = light.intensity;
    let light_range = light.range;
    let light_inner_deg = light.inner_cone_rad.to_degrees();
    let light_outer_deg = light.outer_cone_rad.to_degrees();
    let mut wants_remove_range = false;

    ui.add_space(8.0);
    ui.separator();
    ui.label(egui::RichText::new("Light").strong());

    egui::Grid::new("inspector_light")
        .num_columns(2)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            ui.label("Kind");
            let mut new_kind = kind;
            egui::ComboBox::from_id_salt("light_kind")
                .selected_text(match new_kind {
                    LightKind::Directional => "directional",
                    LightKind::Point => "point",
                    LightKind::Spot => "spot",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut new_kind, LightKind::Directional, "directional");
                    ui.selectable_value(&mut new_kind, LightKind::Point, "point");
                    ui.selectable_value(&mut new_kind, LightKind::Spot, "spot");
                });
            if new_kind != kind {
                let mut to_delete: Vec<String> = Vec::new();
                if matches!(new_kind, LightKind::Directional) {
                    to_delete.push("range".into());
                }
                if !matches!(new_kind, LightKind::Spot) {
                    to_delete.push("inner_cone".into());
                    to_delete.push("outer_cone".into());
                }
                let dsl = match new_kind {
                    LightKind::Directional => "directional",
                    LightKind::Point => "point",
                    LightKind::Spot => "spot",
                };
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "kind".into(),
                    value: dsl.into(),
                    delete: to_delete,
                });
            }
            ui.end_row();

            ui.label("Color");
            let mut color = light_color;
            if ui.color_edit_button_rgb(&mut color).changed() {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "color".into(),
                    value: format!(
                        "[{}, {}, {}]",
                        format_inspector_scalar(color[0]),
                        format_inspector_scalar(color[1]),
                        format_inspector_scalar(color[2]),
                    ),
                    delete: Vec::new(),
                });
            }
            ui.end_row();

            ui.label("Intensity");
            let mut intensity = light_intensity;
            let suffix = if matches!(kind, LightKind::Directional) {
                " lx"
            } else {
                " cd"
            };
            if ui
                .add(
                    egui::DragValue::new(&mut intensity)
                        .speed(0.1)
                        .suffix(suffix)
                        .range(0.0..=f32::INFINITY),
                )
                .changed()
            {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "intensity".into(),
                    value: format_inspector_scalar(intensity),
                    delete: Vec::new(),
                });
            }
            ui.end_row();

            if matches!(kind, LightKind::Point | LightKind::Spot) {
                ui.label("Range");
                match light_range {
                    Some(r) => {
                        let mut range = r;
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::DragValue::new(&mut range)
                                        .speed(0.1)
                                        .range(0.001..=f32::INFINITY),
                                )
                                .changed()
                            {
                                edits.push(PendingEdit::SetAttrCanonical {
                                    node: node_id,
                                    attr: "range".into(),
                                    value: format_inspector_scalar(range),
                                    delete: Vec::new(),
                                });
                            }
                            if ui
                                .small_button("✕")
                                .on_hover_text("Remove range (unlimited)")
                                .clicked()
                            {
                                // PendingEdit can only set+delete-shadows;
                                // the primary-attr delete happens in the
                                // caller to avoid a double borrow.
                                wants_remove_range = true;
                            }
                        });
                    }
                    None => {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("(unlimited)").italics().weak());
                            if ui.small_button("+ set").clicked() {
                                edits.push(PendingEdit::SetAttrCanonical {
                                    node: node_id,
                                    attr: "range".into(),
                                    value: "8".into(),
                                    delete: Vec::new(),
                                });
                            }
                        });
                    }
                }
                ui.end_row();
            }

            if matches!(kind, LightKind::Spot) {
                ui.label("Inner cone");
                let mut inner = light_inner_deg;
                // Spec: 0 ≤ inner ≤ outer ≤ 90°. Outer cone clamps
                // separately below; inner shares the same cap.
                if ui
                    .add(
                        egui::DragValue::new(&mut inner)
                            .speed(0.5)
                            .suffix("°")
                            .range(0.0..=90.0),
                    )
                    .changed()
                {
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "inner_cone".into(),
                        value: format_inspector_scalar(inner),
                        delete: Vec::new(),
                    });
                }
                ui.end_row();

                ui.label("Outer cone");
                let mut outer = light_outer_deg;
                if ui
                    .add(
                        egui::DragValue::new(&mut outer)
                            .speed(0.5)
                            .suffix("°")
                            .range(0.0..=90.0),
                    )
                    .changed()
                {
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "outer_cone".into(),
                        value: format_inspector_scalar(outer),
                        delete: Vec::new(),
                    });
                }
                ui.end_row();
            }
        });

    wants_remove_range
}
