use eframe::egui;

use crate::app::util::format_inspector_scalar;
use crate::viewer::PendingEdit;

/// Render the CSG-op switch and the array / mirror modifier rows for the
/// inspector. Pushes `PendingEdit::SetAttrCanonical` entries into `edits`.
/// Writes the requested CSG kind change into `change_kind_to` — the caller
/// performs the source rewrite because `PendingEdit` only mutates attrs,
/// not the node kind keyword.
pub(super) fn render(
    ui: &mut egui::Ui,
    node: &mogen_core::SceneNode,
    node_span: Option<mogen_core::Span>,
    source: &str,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
    change_kind_to: &mut Option<&'static str>,
) {
    // CSG op switch — flip union/difference/intersect on the same node
    // without retyping. Lowering keys off `node.kind`, so we use a dedicated
    // span rewrite below rather than a `PendingEdit`.
    if matches!(node.kind.as_str(), "union" | "difference" | "intersect") {
        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new("CSG op").strong());
        let cur = node.kind.as_str();
        ui.horizontal(|ui| {
            for k in ["union", "difference", "intersect"] {
                let selected = cur == k;
                if ui.selectable_label(selected, k).clicked() && !selected {
                    *change_kind_to = Some(match k {
                        "union" => "union",
                        "difference" => "difference",
                        "intersect" => "intersect",
                        _ => unreachable!(),
                    });
                }
            }
        });
    }

    // Array / mirror modifier rows. Scalar count/axis/start_angle for arrays;
    // axis-only for mirror. Replicators are not editable in the inspector when
    // their wrapper isn't `editable`, but the wrapper *itself* stays editable
    // per `lower::layout` so this still applies.
    match node.kind.as_str() {
        "array" => {
            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("Array").strong());
            if let Some(span) = node_span {
                let cur_count = crate::edit::get_attr(source, span, "count")
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(1.0);
                let cur_around = crate::edit::get_attr(source, span, "around")
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Y".into());
                let cur_start = crate::edit::get_attr(source, span, "start_angle")
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.0);
                egui::Grid::new(("inspector_array", node_id.0))
                    .num_columns(2)
                    .spacing([6.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Count");
                        let mut count = cur_count.max(1.0);
                        if ui
                            .add(egui::DragValue::new(&mut count).speed(0.1).range(1.0..=512.0))
                            .changed()
                        {
                            edits.push(PendingEdit::SetAttrCanonical {
                                node: node_id,
                                attr: "count".into(),
                                value: (count.round() as i32).to_string(),
                                delete: Vec::new(),
                            });
                        }
                        ui.end_row();

                        ui.label("Around");
                        let mut around = cur_around.clone();
                        egui::ComboBox::from_id_salt(("inspector_array_axis", node_id.0))
                            .selected_text(around.as_str())
                            .show_ui(ui, |ui| {
                                for axis in ["X", "Y", "Z"] {
                                    if ui
                                        .selectable_value(&mut around, axis.into(), axis)
                                        .clicked()
                                        && around.as_str() != cur_around.as_str()
                                    {
                                        edits.push(PendingEdit::SetAttrCanonical {
                                            node: node_id,
                                            attr: "around".into(),
                                            value: axis.into(),
                                            delete: Vec::new(),
                                        });
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("Start °");
                        let mut start = cur_start;
                        if ui
                            .add(egui::DragValue::new(&mut start).speed(0.5).suffix("°"))
                            .changed()
                        {
                            edits.push(PendingEdit::SetAttrCanonical {
                                node: node_id,
                                attr: "start_angle".into(),
                                value: format_inspector_scalar(start),
                                delete: Vec::new(),
                            });
                        }
                        ui.end_row();
                    });
            }
        }
        "mirror" => {
            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("Mirror").strong());
            if let Some(span) = node_span {
                let cur_axis = crate::edit::get_attr(source, span, "axis")
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "X".into());
                ui.horizontal(|ui| {
                    ui.label("Axis");
                    let mut axis = cur_axis.clone();
                    egui::ComboBox::from_id_salt(("inspector_mirror_axis", node_id.0))
                        .selected_text(axis.as_str())
                        .show_ui(ui, |ui| {
                            for a in ["X", "Y", "Z"] {
                                if ui
                                    .selectable_value(&mut axis, a.into(), a)
                                    .clicked()
                                    && axis.as_str() != cur_axis.as_str()
                                {
                                    edits.push(PendingEdit::SetAttrCanonical {
                                        node: node_id,
                                        attr: "axis".into(),
                                        value: a.into(),
                                        delete: Vec::new(),
                                    });
                                }
                            }
                        });
                });
            }
        }
        _ => {}
    }
}
