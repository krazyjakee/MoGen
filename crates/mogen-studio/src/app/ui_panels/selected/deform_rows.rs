//! Inspector Deform-section row renderers. Each row is one editable
//! attribute on the selected node — collapsed into helpers so the
//! `ui_selected` impl method stays a flat orchestration script.

use eframe::egui;

use crate::viewer::PendingEdit;

/// Render a Deform-row for a 0..=1 unit modifier (`noise`, `jitter`, `droop`).
/// `default` is the value used when the user clicks "+ add" on an absent attr.
#[allow(clippy::too_many_arguments)]
pub(super) fn deform_unit_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    current: Option<f32>,
    default: f32,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    deform_scalar_row(
        ui, label, attr, current, default,
        0.0..=1.0, 0.01, "", node_id, edits, wants_remove,
    );
}

/// Render a Deform-row for an angle modifier (`bend_x`/`y`/`z`, `twist_y`),
/// authored in degrees. The lowering pass runs `.to_radians()` before applying.
#[allow(clippy::too_many_arguments)]
pub(super) fn deform_angle_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    current: Option<f32>,
    default: f32,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    deform_scalar_row(
        ui, label, attr, current, default,
        -180.0..=180.0, 0.5, "°", node_id, edits, wants_remove,
    );
}

/// Render the `taper` row. Different range/default from the unit row because
/// taper's neutral is 1.0 (no scale change) and authors flare past 1.0 for
/// trumpet shapes — clamping to [0, 1] would silently drop that case.
pub(super) fn deform_taper_row(
    ui: &mut egui::Ui,
    current: Option<f32>,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    deform_scalar_row(
        ui, "Taper", "taper", current, 0.5,
        0.0..=4.0, 0.02, "", node_id, edits, wants_remove,
    );
}

#[allow(clippy::too_many_arguments)]
fn deform_scalar_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    current: Option<f32>,
    default: f32,
    range: std::ops::RangeInclusive<f32>,
    speed: f32,
    suffix: &str,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    use crate::app::util::format_inspector_scalar;
    ui.label(label);
    ui.horizontal(|ui| match current {
        Some(initial) => {
            let mut v = initial;
            let resp = ui.add(
                egui::DragValue::new(&mut v)
                    .speed(speed)
                    .suffix(suffix)
                    .range(range),
            );
            if resp.changed() {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: attr.into(),
                    value: format_inspector_scalar(v),
                    delete: Vec::new(),
                });
            }
            if ui
                .small_button("✕")
                .on_hover_text("Remove modifier")
                .clicked()
            {
                wants_remove.push(attr);
            }
        }
        None => {
            ui.label(egui::RichText::new("(none)").italics().weak());
            if ui.small_button("+ add").clicked() {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: attr.into(),
                    value: format_inspector_scalar(default),
                    delete: Vec::new(),
                });
            }
        }
    });
    ui.end_row();
}

/// Render the `faceted` row as a checkbox: present-and-non-zero is on, absent
/// or `0` is off. Toggling on writes `faceted=1`; toggling off removes the
/// attr entirely so it doesn't sit at `0` polluting the source.
pub(super) fn deform_faceted_row(
    ui: &mut egui::Ui,
    current: Option<f32>,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    ui.label("Faceted");
    ui.horizontal(|ui| {
        let on = current.map(|v| v != 0.0).unwrap_or(false);
        let mut new_on = on;
        ui.checkbox(&mut new_on, "")
            .on_hover_text("Hard-edge per-triangle normals (low-poly look).");
        if new_on != on {
            if new_on {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "faceted".into(),
                    value: "1".into(),
                    delete: Vec::new(),
                });
            } else if current.is_some() {
                wants_remove.push("faceted");
            }
        }
    });
    ui.end_row();
}

/// Render the `seed` row. Drives the random stream for `noise` / `jitter`;
/// authors typically tweak it to roll a different stochastic result while
/// keeping the modifier amounts the same.
pub(super) fn deform_seed_row(
    ui: &mut egui::Ui,
    current: Option<u32>,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    ui.label("Seed");
    ui.horizontal(|ui| match current {
        Some(initial) => {
            let mut v = initial;
            let resp = ui.add(
                egui::DragValue::new(&mut v)
                    .speed(1.0)
                    .range(0..=u32::MAX),
            );
            if resp.changed() {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "seed".into(),
                    value: v.to_string(),
                    delete: Vec::new(),
                });
            }
            if ui
                .small_button("✕")
                .on_hover_text("Remove seed (defaults to 1)")
                .clicked()
            {
                wants_remove.push("seed");
            }
        }
        None => {
            ui.label(egui::RichText::new("(none)").italics().weak());
            if ui.small_button("+ add").clicked() {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "seed".into(),
                    value: "1".into(),
                    delete: Vec::new(),
                });
            }
        }
    });
    ui.end_row();
}
