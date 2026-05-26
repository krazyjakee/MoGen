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

/// Render three `DragValue`s for the X/Y/Z components of `vals`, all sharing
/// `speed` and `suffix` (the suffix renders e.g. the `°` on rotation rows).
/// Returns `true` if any of the three changed this frame — callers use this
/// to decide whether to emit a `SetAttrCanonical` write.
fn drag_triple(
    ui: &mut egui::Ui,
    vals: &mut [f32; 3],
    speed: f32,
    suffix: &str,
) -> Option<u8> {
    let mut changed_axis: Option<u8> = None;
    for (i, v) in vals.iter_mut().enumerate() {
        if ui
            .add(egui::DragValue::new(v).speed(speed).suffix(suffix))
            .changed()
        {
            changed_axis = Some(i as u8);
        }
    }
    changed_axis
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
    let mut t = [
        effective_translation.x,
        effective_translation.y,
        effective_translation.z,
    ];
    let mut r = [rx_rad.to_degrees(), ry_rad.to_degrees(), rz_rad.to_degrees()];
    let mut s = [t_scale.x, t_scale.y, t_scale.z];

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
                if drag_triple(ui, &mut t, 0.02, "").is_some() {
                    // Emit the full `pos=[x,y,z]` vector and strip shadow attrs.
                    // Per-axis `x=`/`y=`/`z=` writes left two attrs fighting in
                    // the header; whichever won depended on resolution order.
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "pos".into(),
                        value: format!(
                            "[{}, {}, {}]",
                            format_inspector_scalar(t[0]),
                            format_inspector_scalar(t[1]),
                            format_inspector_scalar(t[2]),
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
                if drag_triple(ui, &mut r, 0.5, "\u{00B0}").is_some() {
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "rot".into(),
                        value: format!(
                            "[{}, {}, {}]",
                            format_inspector_scalar(r[0]),
                            format_inspector_scalar(r[1]),
                            format_inspector_scalar(r[2]),
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
            let pre = s;
            let linked = *scale_linked;
            let changed_axis = drag_triple(ui, &mut s, 0.02, "");
            if let Some(axis) = changed_axis {
                if linked {
                    // Multiply the other two axes by the same ratio the
                    // dragged axis just took, falling back to uniform when
                    // the old value is ~0 — otherwise the others would stay
                    // at 0 and silently swallow the drag.
                    let i = axis as usize;
                    let (new_v, old_v) = (s[i], pre[i]);
                    if old_v.abs() > 1.0e-6 {
                        let ratio = new_v / old_v;
                        s = [pre[0] * ratio, pre[1] * ratio, pre[2] * ratio];
                    } else {
                        s = [new_v, new_v, new_v];
                    }
                }
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "scale".into(),
                    delete: Vec::new(),
                    value: format!(
                        "[{}, {}, {}]",
                        format_inspector_scalar(s[0]),
                        format_inspector_scalar(s[1]),
                        format_inspector_scalar(s[2]),
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
