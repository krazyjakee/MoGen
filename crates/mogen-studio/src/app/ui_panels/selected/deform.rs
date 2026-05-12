use eframe::egui;

use super::deform_rows::{
    deform_angle_row, deform_faceted_row, deform_seed_row, deform_taper_row, deform_unit_row,
};
use crate::viewer::PendingEdit;

/// Render the collapsing "Deform" section. Returns the list of attributes
/// the user clicked "✕" on so the caller can `delete_attr` + push undo.
///
/// Gated to nodes whose mesh actually flows through `apply_deform`: skip
/// loaded `.glb` meshes (`kind="mesh"`), top-level CSG results, and
/// `solid` / group-style nodes that don't carry a primitive mesh of their
/// own — those are filtered by the caller before calling render.
pub(super) fn render(
    ui: &mut egui::Ui,
    source: &str,
    span: mogen_core::Span,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
) -> Vec<&'static str> {
    let mut wants_remove_deform: Vec<&'static str> = Vec::new();
    let read_f32 = |attr: &str| -> Option<f32> {
        crate::edit::get_attr(source, span, attr)
            .and_then(|s| s.parse::<f32>().ok())
    };
    let cur_noise = read_f32("noise");
    let cur_jitter = read_f32("jitter");
    let cur_bend_x = read_f32("bend_x");
    let cur_bend_y = read_f32("bend_y");
    let cur_bend_z = read_f32("bend_z");
    let cur_twist_y = read_f32("twist_y");
    let cur_taper = read_f32("taper");
    let cur_droop = read_f32("droop");
    let cur_faceted = read_f32("faceted");
    let cur_seed = crate::edit::get_attr(source, span, "seed")
        .and_then(|s| s.parse::<u32>().ok());
    let any_set = cur_noise.is_some()
        || cur_jitter.is_some()
        || cur_bend_x.is_some()
        || cur_bend_y.is_some()
        || cur_bend_z.is_some()
        || cur_twist_y.is_some()
        || cur_taper.is_some()
        || cur_droop.is_some()
        || cur_faceted.is_some()
        || cur_seed.is_some();

    ui.add_space(8.0);
    ui.separator();
    egui::CollapsingHeader::new("Deform")
        .id_salt("inspector_deform_header")
        .default_open(any_set)
        .show(ui, |ui| {
            egui::Grid::new("inspector_deform")
                .num_columns(2)
                .spacing([6.0, 4.0])
                .show(ui, |ui| {
                    deform_unit_row(
                        ui, "Noise", "noise", cur_noise, 0.3,
                        node_id, edits, &mut wants_remove_deform,
                    );
                    deform_unit_row(
                        ui, "Jitter", "jitter", cur_jitter, 0.3,
                        node_id, edits, &mut wants_remove_deform,
                    );
                    deform_angle_row(
                        ui, "Bend X", "bend_x", cur_bend_x, 15.0,
                        node_id, edits, &mut wants_remove_deform,
                    );
                    deform_angle_row(
                        ui, "Bend Y", "bend_y", cur_bend_y, 15.0,
                        node_id, edits, &mut wants_remove_deform,
                    );
                    deform_angle_row(
                        ui, "Bend Z", "bend_z", cur_bend_z, 15.0,
                        node_id, edits, &mut wants_remove_deform,
                    );
                    deform_angle_row(
                        ui, "Twist Y", "twist_y", cur_twist_y, 30.0,
                        node_id, edits, &mut wants_remove_deform,
                    );
                    deform_taper_row(
                        ui, cur_taper, node_id,
                        edits, &mut wants_remove_deform,
                    );
                    deform_unit_row(
                        ui, "Droop", "droop", cur_droop, 0.3,
                        node_id, edits, &mut wants_remove_deform,
                    );
                    deform_faceted_row(
                        ui, cur_faceted, node_id,
                        edits, &mut wants_remove_deform,
                    );
                    deform_seed_row(
                        ui, cur_seed, node_id,
                        edits, &mut wants_remove_deform,
                    );
                });
        });
    wants_remove_deform
}
