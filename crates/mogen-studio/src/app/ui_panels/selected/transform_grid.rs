use eframe::egui;

use crate::app::util::format_inspector_scalar;
use crate::edit::get_attr;
use crate::gizmo::GizmoMode;
use crate::viewer::{PendingEdit, Viewer};

/// First attribute in `attrs` whose source text is a parametric expression
/// (a constant `expr` or a `$param`), with its raw text. The grid reads the
/// *evaluated* `node.transform`, so it cannot see an expression at all —
/// without this guard a single drag would re-emit the channel as a numeric
/// literal and silently destroy `pos=[0, $h, 0]` / `rot=[0, 90/2, 0]`.
fn expr_attr(
    src: &str,
    span: Option<mogen_core::Span>,
    attrs: &[&str],
) -> Option<String> {
    let span = span?;
    for a in attrs {
        if let Some(raw) = get_attr(src, span, a) {
            let inner = raw.trim().trim_start_matches('[').trim_end_matches(']');
            let all_num = inner
                .split(',')
                .all(|p| p.trim().parse::<f32>().is_ok());
            if !all_num {
                return Some(format!("{a}={}", raw.trim()));
            }
        }
    }
    None
}

fn locked_transform_row(ui: &mut egui::Ui, label: &str, raw: &str) {
    // The transform grid has num_columns(4): label + X + Y + Z (or link).
    // Span the value across all three value columns so the lock row aligns
    // with the draggable rows rather than being cramped into column 2.
    ui.label(label);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(raw).monospace().weak())
            .on_hover_text(
                "Parametric transform (expression or $param). Edit it in the \
                 code view — dragging here would overwrite it with a plain \
                 number.",
            );
        ui.label("\u{1F512}");
    });
    // Pad the remaining columns so this row occupies the same logical width
    // as the three-DragValue rows (label + X + Y + Z).
    ui.label("");
    ui.label("");
    ui.end_row();
}

/// Render the gizmo-mode toggle row and the translate/rotate/scale grid for
/// the inspector. Changes are queued onto `edits` as
/// `PendingEdit::SetAttrCanonical`; the link-axes toggle state is read and
/// updated through `scale_linked`. For attached nodes the grid shows the
/// user-authored portion of the transform (live = attach + user) so writeback
/// doesn't double-count the attach contribution.
#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    ui: &mut egui::Ui,
    viewer: &Viewer,
    node: &mogen_core::SceneNode,
    scale_linked: &mut bool,
    node_id: mogen_core::NodeId,
    src: &str,
    node_span: Option<mogen_core::Span>,
    edits: &mut Vec<PendingEdit>,
) {
    // A parametric value on any attr that feeds a channel (including the
    // `x=`/`from=`/`rx=` shorthands the commit path would strip) locks that
    // channel to read-only so the drag can't clobber the expression.
    let pos_expr = expr_attr(src, node_span, &["pos", "x", "y", "z", "from", "to"]);
    let rot_expr = expr_attr(src, node_span, &["rot", "rx", "ry", "rz"]);
    let scale_expr = expr_attr(src, node_span, &["scale"]);
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
            if let Some(raw) = &pos_expr {
                locked_transform_row(ui, "Translate", raw);
            } else {
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
            }

            if let Some(raw) = &rot_expr {
                locked_transform_row(ui, "Rotate\u{00B0}", raw);
            } else {
                ui.label("Rotate\u{00B0}");
                let mut emit_rot = false;
                if ui.add(egui::DragValue::new(&mut rx).speed(0.5).suffix("\u{00B0}")).changed() {
                    emit_rot = true;
                }
                if ui.add(egui::DragValue::new(&mut ry).speed(0.5).suffix("\u{00B0}")).changed() {
                    emit_rot = true;
                }
                if ui.add(egui::DragValue::new(&mut rz).speed(0.5).suffix("\u{00B0}")).changed() {
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
            }

            if let Some(raw) = &scale_expr {
                locked_transform_row(ui, "Scale", raw);
                return;
            }
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
            let link_label = if linked { "\u{1F517}" } else { "\u{1F513}" };
            let link_tip = if linked {
                "Scale axes linked \u{2014} drag any axis to scale all three (click to unlink)"
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

#[cfg(test)]
mod tests {
    use super::*;
    use mogen_core::Span;

    fn sp(src: &str) -> Option<Span> {
        Some(Span { start: 0, end: src.len() })
    }

    #[test]
    fn expr_attr_none_when_all_literals() {
        let src = r#"box "b" (pos=[1.0, 2.0, 3.0])"#;
        assert_eq!(expr_attr(src, sp(src), &["pos"]), None);
    }

    #[test]
    fn expr_attr_detects_param_ref_in_pos() {
        let src = r#"box "b" (pos=[0, $h, 0])"#;
        assert!(expr_attr(src, sp(src), &["pos", "x"]).is_some());
    }

    #[test]
    fn expr_attr_detects_arithmetic_expr_in_rot() {
        let src = r#"box "b" (rot=[0, 90/2, 0])"#;
        assert!(expr_attr(src, sp(src), &["rot", "rx"]).is_some());
    }

    #[test]
    fn expr_attr_none_when_span_absent() {
        // Without a span, no source text can be read.
        let src = r#"box "b" (pos=[$x, 0, 0])"#;
        assert_eq!(expr_attr(src, None, &["pos"]), None);
    }

    #[test]
    fn expr_attr_skips_literal_attr_and_finds_later_param() {
        // pos=[1,2,3] is literal — skip. x=$v is a param ref — lock.
        let src = r#"box "b" (pos=[1, 2, 3], x=$v)"#;
        let result = expr_attr(src, sp(src), &["pos", "x"]);
        assert!(result.is_some());
        let raw = result.unwrap();
        assert!(raw.starts_with("x="), "expected x= prefix, got {raw}");
    }

    #[test]
    fn expr_attr_scalar_pos_with_param_is_locked() {
        let src = r#"post "p" (y=$shelf_h)"#;
        assert!(expr_attr(src, sp(src), &["pos", "x", "y", "z"]).is_some());
    }
}
