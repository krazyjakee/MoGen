use eframe::egui;

use crate::app::util::format_inspector_scalar;
use crate::gizmo::GizmoMode;
use crate::viewer::{PendingEdit, Viewer};

/// Render the gizmo-mode toggle row and the translate/rotate/scale grid for
/// the inspector. Changes are queued onto `edits` as
/// `PendingEdit::SetAttrCanonical`; the link-axes toggle state is read and
/// updated through `scale_linked`. For attached nodes the grid shows the
/// user-authored portion of the transform (live = attach + user) so writeback
/// doesn't double-count the attach contribution.
pub(super) fn render(
    ui: &mut egui::Ui,
    viewer: &Viewer,
    node: &mogen_core::SceneNode,
    scale_linked: &mut bool,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let cur = viewer.gizmo_mode();
        for (label, mode) in [
            ("Move", GizmoMode::Translate),
            ("Rotate", GizmoMode::Rotate),
            ("Scale", GizmoMode::Scale),
        ] {
            if ui.selectable_label(cur == mode, label).clicked() {
                viewer.set_gizmo_mode(mode);
            }
        }
    });

    let (effective_translation, effective_rotation) = match node.attach_binding.as_ref() {
        Some(b) => (
            node.transform.translation - b.anchor_vec3(),
            node.transform.rotation * b.rotation_quat().inverse(),
        ),
        None => (node.transform.translation, node.transform.rotation),
    };
    let t_scale = node.transform.scale;
    let (rx_rad, ry_rad, rz_rad) = effective_rotation.to_euler(glam::EulerRot::XYZ);
    let mut tx = effective_translation.x;
    let mut ty = effective_translation.y;
    let mut tz = effective_translation.z;
    let mut rx = rx_rad.to_degrees();
    let mut ry = ry_rad.to_degrees();
    let mut rz = rz_rad.to_degrees();
    let mut sx = t_scale.x;
    let mut sy = t_scale.y;
    let mut sz = t_scale.z;

    // DSL shortcut/corner-form attrs that override the canonical transform
    // field. Stripped on commit for the same reason the gizmo does — otherwise
    // a node authored with `x=` / `from=` / `rx=` shorthand would silently
    // win on recompile and make the just-typed value snap back. Kept in sync
    // with the viewport gizmo's shadow lists in `viewer/state.rs`.
    let pos_shadows: Vec<String> = ["x", "y", "z", "from", "to"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let rot_shadows: Vec<String> = ["rx", "ry", "rz"].iter().map(|s| s.to_string()).collect();

    ui.add_space(6.0);
    egui::Grid::new("inspector_transform")
        .num_columns(4)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            ui.label("Translate");
            let mut emit_pos = false;
            if ui.add(egui::DragValue::new(&mut tx).speed(0.02)).changed() {
                emit_pos = true;
            }
            if ui.add(egui::DragValue::new(&mut ty).speed(0.02)).changed() {
                emit_pos = true;
            }
            if ui.add(egui::DragValue::new(&mut tz).speed(0.02)).changed() {
                emit_pos = true;
            }
            if emit_pos {
                // Emit the full `pos=[x,y,z]` vector and strip shadow attrs.
                // Per-axis `x=`/`y=`/`z=` writes left two attrs fighting in
                // the header; whichever won depended on resolution order.
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "pos".into(),
                    value: format!(
                        "[{}, {}, {}]",
                        format_inspector_scalar(tx),
                        format_inspector_scalar(ty),
                        format_inspector_scalar(tz),
                    ),
                    delete: pos_shadows.clone(),
                });
            }
            ui.end_row();

            ui.label("Rotate°");
            let mut emit_rot = false;
            if ui.add(egui::DragValue::new(&mut rx).speed(0.5).suffix("°")).changed() {
                emit_rot = true;
            }
            if ui.add(egui::DragValue::new(&mut ry).speed(0.5).suffix("°")).changed() {
                emit_rot = true;
            }
            if ui.add(egui::DragValue::new(&mut rz).speed(0.5).suffix("°")).changed() {
                emit_rot = true;
            }
            if emit_rot {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "rot".into(),
                    value: format!(
                        "[{}, {}, {}]",
                        format_inspector_scalar(rx),
                        format_inspector_scalar(ry),
                        format_inspector_scalar(rz),
                    ),
                    delete: rot_shadows.clone(),
                });
            }
            ui.end_row();

            ui.label("Scale");
            let pre_sx = sx;
            let pre_sy = sy;
            let pre_sz = sz;
            let linked = *scale_linked;
            let mut changed_axis: Option<u8> = None;
            if ui.add(egui::DragValue::new(&mut sx).speed(0.02)).changed() {
                changed_axis = Some(0);
            }
            if ui.add(egui::DragValue::new(&mut sy).speed(0.02)).changed() {
                changed_axis = Some(1);
            }
            if ui.add(egui::DragValue::new(&mut sz).speed(0.02)).changed() {
                changed_axis = Some(2);
            }
            if let Some(axis) = changed_axis {
                if linked {
                    // Multiply the other two axes by the same ratio the
                    // dragged axis just took, falling back to uniform when
                    // the old value is ~0 — otherwise the others would stay
                    // at 0 and silently swallow the drag.
                    let (new_v, old_v) = match axis {
                        0 => (sx, pre_sx),
                        1 => (sy, pre_sy),
                        _ => (sz, pre_sz),
                    };
                    if old_v.abs() > 1.0e-6 {
                        let ratio = new_v / old_v;
                        sx = pre_sx * ratio;
                        sy = pre_sy * ratio;
                        sz = pre_sz * ratio;
                    } else {
                        sx = new_v;
                        sy = new_v;
                        sz = new_v;
                    }
                }
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "scale".into(),
                    delete: Vec::new(),
                    value: format!(
                        "[{}, {}, {}]",
                        format_inspector_scalar(sx),
                        format_inspector_scalar(sy),
                        format_inspector_scalar(sz),
                    ),
                });
            }
            let link_label = if linked { "🔗" } else { "🔓" };
            let link_tip = if linked {
                "Scale axes linked — drag any axis to scale all three (click to unlink)"
            } else {
                "Scale axes independent (click to link)"
            };
            if ui
                .selectable_label(linked, link_label)
                .on_hover_text(link_tip)
                .clicked()
            {
                *scale_linked = !linked;
            }
            ui.end_row();
        });
}
