//! Gradient editor for the material inspector. Renders a draggable-stop ramp
//! strip, per-stop colour pickers, and a kind/axis selector for the four
//! `gradient = …` surface forms (`linear` / `vertical` / `radial` / `stops`).
//! Every edit funnels through `pending` (or `pending_delete`) so the materials
//! panel can splice the resulting DSL via the same span-aware `edit::set_attr`
//! / `edit::delete_attr` pipeline as the PBR widgets — formatting and
//! diagnostics on the surrounding `material(…)` block stay intact.
//!
//! Serialization rule: when the ramp is two stops at `t = 0` and `t = 1`,
//! emit the matching 2-stop sugar (`vertical` / `linear` / `radial`) instead
//! of `stops(…)` so authored files stay readable; only ≥3 stops or off-edge
//! 2-stop layouts fall through to `stops(…)`.

use eframe::egui;
use mogen_core::{sample_stops, Gradient, GradientAxis, GradientKind, GradientStop};

use crate::app::util::format_inspector_scalar;

/// Render the gradient editor for one material. `pending` collects
/// `set_attr` updates for the `gradient=` attr; `pending_delete` is set to
/// the material name when the user clicks "Remove gradient" so the caller
/// can `delete_attr` the field. Salts every widget ID with the material's
/// scene-graph index so imports that re-use a material name don't collide.
pub(super) fn render(
    ui: &mut egui::Ui,
    idx: usize,
    mat: &mogen_core::Material,
    pending: &mut Vec<(String, &'static str, String)>,
    pending_delete: &mut Option<String>,
) {
    let section_id = egui::Id::new(("gradient_section", idx, mat.name.as_str()));
    egui::CollapsingHeader::new("Gradient")
        .id_salt(section_id)
        .default_open(mat.gradient.is_some())
        .show(ui, |ui| match &mat.gradient {
            None => {
                if ui
                    .button("+ Add gradient")
                    .on_hover_text(
                        "Add a vertical two-stop ramp baked into per-vertex \
                         COLOR_0 at export. Authored as `gradient=vertical(…)` \
                         on this material.",
                    )
                    .clicked()
                {
                    let default = "vertical(from=[0, 0, 0], to=[1, 1, 1])".to_string();
                    pending.push((mat.name.clone(), "gradient", default));
                }
            }
            Some(g) => render_editor(ui, idx, mat, g, pending, pending_delete),
        });
}

fn render_editor(
    ui: &mut egui::Ui,
    idx: usize,
    mat: &mogen_core::Material,
    g: &Gradient,
    pending: &mut Vec<(String, &'static str, String)>,
    pending_delete: &mut Option<String>,
) {
    let mut stops = g.stops.clone();
    let mut kind = g.kind;
    let mut changed = false;

    ui.horizontal(|ui| {
        if ui
            .button("Remove gradient")
            .on_hover_text("Strip the gradient= attribute from this material")
            .clicked()
        {
            *pending_delete = Some(mat.name.clone());
        }
    });

    // Bail out before further edits if the user already requested deletion —
    // the next frame will re-enter with `mat.gradient = None`.
    if pending_delete.as_deref() == Some(mat.name.as_str()) {
        return;
    }

    changed |= ramp_strip(ui, idx, mat, &mut stops);

    ui.add_space(4.0);
    let mut remove_stop: Option<usize> = None;
    let stop_count = stops.len();
    for (si, s) in stops.iter_mut().enumerate() {
        let row_id = egui::Id::new(("grad_stop", idx, mat.name.as_str(), si));
        ui.push_id(row_id, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("#{si}"));
                let mut t = s.t;
                if ui
                    .add(
                        egui::DragValue::new(&mut t)
                            .speed(0.005)
                            .range(0.0..=1.0)
                            .fixed_decimals(3)
                            .prefix("t "),
                    )
                    .changed()
                {
                    s.t = t.clamp(0.0, 1.0);
                    changed = true;
                }
                let mut rgb = [s.color[0], s.color[1], s.color[2]];
                if ui.color_edit_button_rgb(&mut rgb).changed() {
                    s.color = [rgb[0], rgb[1], rgb[2], s.color[3]];
                    changed = true;
                }
                let removable = stop_count > 2;
                let resp = ui.add_enabled(removable, egui::Button::new("×"));
                let hover = if removable {
                    "Remove this stop"
                } else {
                    "Minimum 2 stops required"
                };
                if resp.on_hover_text(hover).clicked() {
                    remove_stop = Some(si);
                }
            });
        });
    }
    if let Some(i) = remove_stop {
        stops.remove(i);
        changed = true;
    }

    ui.add_space(2.0);
    if ui
        .button("+ Add stop")
        .on_hover_text("Insert a stop at the midpoint of the largest gap")
        .clicked()
    {
        let new_stop = stop_at_largest_gap(&stops);
        stops.push(new_stop);
        sort_stops(&mut stops);
        changed = true;
    }

    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label("Type");
        let kind_id = egui::Id::new(("grad_kind", idx, mat.name.as_str()));
        let mut is_radial = matches!(kind, GradientKind::Radial);
        let prev_radial = is_radial;
        egui::ComboBox::from_id_salt(kind_id)
            .selected_text(if is_radial { "Radial" } else { "Linear" })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut is_radial, false, "Linear");
                ui.selectable_value(&mut is_radial, true, "Radial");
            });
        if is_radial != prev_radial {
            kind = if is_radial {
                GradientKind::Radial
            } else {
                GradientKind::Linear { axis: GradientAxis::Y }
            };
            changed = true;
        }

        if let GradientKind::Linear { ref mut axis } = kind {
            ui.label("Axis");
            let ax_id = egui::Id::new(("grad_axis", idx, mat.name.as_str()));
            let prev_axis = *axis;
            egui::ComboBox::from_id_salt(ax_id)
                .selected_text(match axis {
                    GradientAxis::X => "X",
                    GradientAxis::Y => "Y",
                    GradientAxis::Z => "Z",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(axis, GradientAxis::X, "X");
                    ui.selectable_value(axis, GradientAxis::Y, "Y");
                    ui.selectable_value(axis, GradientAxis::Z, "Z");
                });
            if *axis != prev_axis {
                changed = true;
            }
        }
    });

    ui.label(
        egui::RichText::new(
            "Tip: vertex colours interpolate between vertices — bump primitive \
             segments / rings for smoother ramps.",
        )
        .weak(),
    );

    if changed {
        sort_stops(&mut stops);
        let g = Gradient { kind, stops };
        let dsl = serialize_gradient(&g);
        pending.push((mat.name.clone(), "gradient", dsl));
    }
}

/// Allocate an interactive horizontal ramp showing the current gradient with
/// per-stop handles. Left-click on empty space adds a stop, drag on a handle
/// reposition it in `[0, 1]`, right-click on a handle removes it (minimum 2).
fn ramp_strip(
    ui: &mut egui::Ui,
    idx: usize,
    mat: &mogen_core::Material,
    stops: &mut Vec<GradientStop>,
) -> bool {
    let width = ui.available_width().max(160.0);
    let height = 36.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());

    let painter = ui.painter();

    // Checker so semi-transparent stops read correctly. Cheap small grid.
    let checker_a = egui::Color32::from_gray(48);
    let checker_b = egui::Color32::from_gray(72);
    let cells = 16;
    let cell_w = rect.width() / cells as f32;
    let cell_h = (rect.height() - 12.0) / 2.0;
    for cy in 0..2 {
        for cx in 0..cells {
            let c = if (cx + cy) % 2 == 0 { checker_a } else { checker_b };
            let r = egui::Rect::from_min_size(
                egui::pos2(rect.left() + cx as f32 * cell_w, rect.top() + cy as f32 * cell_h),
                egui::vec2(cell_w + 0.5, cell_h + 0.5),
            );
            painter.rect_filled(r, 0.0, c);
        }
    }

    // Sorted snapshot for sampling the overlay and for click-to-add colour
    // interpolation. Drag handling operates on `stops` directly.
    let mut sorted = stops.clone();
    sort_stops(&mut sorted);
    const STRIPS: usize = 128;
    let strip_w = rect.width() / STRIPS as f32;
    let bar_top = rect.top();
    let bar_h = rect.height() - 12.0;
    for s in 0..STRIPS {
        let t = (s as f32 + 0.5) / STRIPS as f32;
        let c = sample_stops(&sorted, t);
        let r = egui::Rect::from_min_size(
            egui::pos2(rect.left() + strip_w * s as f32, bar_top),
            egui::vec2(strip_w + 1.0, bar_h),
        );
        painter.rect_filled(r, 0.0, color32_from_f4(c));
    }

    // Outline
    painter.rect_stroke(
        egui::Rect::from_min_max(
            egui::pos2(rect.left(), bar_top),
            egui::pos2(rect.right(), bar_top + bar_h),
        ),
        2.0,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.fg_stroke.color),
    );

    let drag_id = egui::Id::new(("grad_drag", idx, mat.name.as_str()));
    let mut changed = false;

    let pixel_x_to_t = |x: f32| -> f32 {
        ((x - rect.left()) / rect.width()).clamp(0.0, 1.0)
    };

    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = pixel_x_to_t(pos.x);
            if let Some(i) = nearest_stop_within(stops, t, rect.width(), 10.0) {
                ui.ctx().memory_mut(|m| {
                    m.data.insert_temp::<usize>(drag_id, i);
                });
            }
        }
    }

    if response.dragged() {
        let dragged_idx: Option<usize> =
            ui.ctx().memory(|m| m.data.get_temp::<usize>(drag_id));
        if let (Some(i), Some(pos)) = (dragged_idx, response.interact_pointer_pos()) {
            if i < stops.len() {
                let t_raw = pixel_x_to_t(pos.x);
                // Clamp within neighboring stops so the dragged stop never
                // crosses a sibling. The stops slice is always sorted at frame
                // start (fresh clone from the compiled scene), so index `i`
                // remains valid as long as ordering is preserved. Without this
                // clamp a cross would resort the slice next frame, making `i`
                // point to the wrong stop for the rest of the drag.
                let lo = if i > 0 { stops[i - 1].t } else { 0.0 };
                let hi = if i + 1 < stops.len() { stops[i + 1].t } else { 1.0 };
                let t = t_raw.clamp(lo, hi);
                if (stops[i].t - t).abs() > 1e-5 {
                    stops[i].t = t;
                    changed = true;
                }
            }
        }
    }

    if response.drag_stopped() {
        ui.ctx().memory_mut(|m| {
            m.data.remove::<usize>(drag_id);
        });
    }

    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = pixel_x_to_t(pos.x);
            if nearest_stop_within(stops, t, rect.width(), 10.0).is_none() {
                let c = sample_stops(&sorted, t);
                stops.push(GradientStop { t, color: c });
                changed = true;
            }
        }
    }

    if response.secondary_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = pixel_x_to_t(pos.x);
            if let Some(i) = nearest_stop_within(stops, t, rect.width(), 16.0) {
                if stops.len() > 2 {
                    stops.remove(i);
                    changed = true;
                }
            }
        }
    }

    // Paint stop markers last so they sit on top of the ramp strip.
    let dragged_idx_now: Option<usize> =
        ui.ctx().memory(|m| m.data.get_temp::<usize>(drag_id));
    for (i, s) in stops.iter().enumerate() {
        let x = rect.left() + s.t.clamp(0.0, 1.0) * rect.width();
        let stop_y = bar_top + bar_h + 2.0;
        let handle_rect = egui::Rect::from_center_size(
            egui::pos2(x, stop_y + 5.0),
            egui::vec2(10.0, 10.0),
        );
        painter.rect_filled(handle_rect, 2.0, color32_from_f4(s.color));
        let stroke_col = if Some(i) == dragged_idx_now {
            egui::Color32::WHITE
        } else {
            ui.visuals().widgets.active.fg_stroke.color
        };
        painter.rect_stroke(handle_rect, 2.0, egui::Stroke::new(1.5, stroke_col));
        // Drop-tick from the ramp bottom to the handle top so it's clear
        // which stop owns which colour, regardless of theme contrast.
        painter.line_segment(
            [
                egui::pos2(x, bar_top + bar_h - 2.0),
                egui::pos2(x, stop_y),
            ],
            egui::Stroke::new(1.0, stroke_col),
        );
    }

    response.on_hover_text(
        "Left-click to add a stop, drag to reposition, right-click on a \
         stop to remove (minimum 2).",
    );

    changed
}

fn nearest_stop_within(
    stops: &[GradientStop],
    t: f32,
    strip_px: f32,
    threshold_px: f32,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, s) in stops.iter().enumerate() {
        let dist_px = (s.t - t).abs() * strip_px;
        if dist_px <= threshold_px {
            match best {
                Some((_, d)) if d <= dist_px => {}
                _ => best = Some((i, dist_px)),
            }
        }
    }
    best.map(|(i, _)| i)
}

fn color32_from_f4(c: [f32; 4]) -> egui::Color32 {
    let to_byte = |v: f32| -> u8 { (v.clamp(0.0, 1.0) * 255.0).round() as u8 };
    egui::Color32::from_rgba_unmultiplied(to_byte(c[0]), to_byte(c[1]), to_byte(c[2]), to_byte(c[3]))
}

fn sort_stops(stops: &mut [GradientStop]) {
    stops.sort_by(|a, b| {
        a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Pick a position to drop a freshly-added stop. Targets the midpoint of the
/// largest existing gap so users adding stops one at a time progressively
/// subdivide the ramp instead of stacking new markers on top of one another.
fn stop_at_largest_gap(stops: &[GradientStop]) -> GradientStop {
    let mut sorted = stops.to_vec();
    sort_stops(&mut sorted);
    let mut best_t = 0.5;
    let mut best_gap = -1.0;
    for w in sorted.windows(2) {
        let gap = w[1].t - w[0].t;
        if gap > best_gap {
            best_gap = gap;
            best_t = (w[0].t + w[1].t) * 0.5;
        }
    }
    let c = sample_stops(&sorted, best_t);
    GradientStop { t: best_t, color: c }
}

/// Pick the right surface form for the editor's current state. Two stops at
/// `t = 0` and `t = 1` collapse to the matching sugar (`vertical` for Linear
/// + Y, `linear(..., axis=…)` for Linear + X|Z, `radial(...)` for Radial);
/// anything else (more stops, or off-edge layouts) emits `stops(...)`.
/// Default-valued attrs are omitted from `stops(...)` to keep the surface
/// minimal: `positions=` only appears when not evenly spaced, `axis=` only
/// when not Y, `kind=` only when `radial`.
pub(super) fn serialize_gradient(g: &Gradient) -> String {
    let collapsible = g.stops.len() == 2
        && (g.stops[0].t).abs() < 1e-4
        && (g.stops[1].t - 1.0).abs() < 1e-4;

    if collapsible {
        let a = g.stops[0].color;
        let b = g.stops[1].color;
        return match g.kind {
            GradientKind::Linear { axis: GradientAxis::Y } => format!(
                "vertical(from={}, to={})",
                color_vec3(a),
                color_vec3(b),
            ),
            GradientKind::Linear { axis } => format!(
                "linear(from={}, to={}, axis={})",
                color_vec3(a),
                color_vec3(b),
                axis_letter(axis),
            ),
            GradientKind::Radial => format!(
                "radial(center={}, edge={})",
                color_vec3(a),
                color_vec3(b),
            ),
        };
    }

    let colors = g
        .stops
        .iter()
        .map(|s| color_vec3(s.color))
        .collect::<Vec<_>>()
        .join(", ");
    let n = g.stops.len();
    let evenly_spaced = n >= 2
        && g.stops.iter().enumerate().all(|(i, s)| {
            let expected = i as f32 / (n - 1) as f32;
            (s.t - expected).abs() < 1e-4
        });

    let mut parts: Vec<String> = Vec::with_capacity(4);
    parts.push(format!("colors=[{colors}]"));
    if !evenly_spaced {
        let positions = g
            .stops
            .iter()
            .map(|s| format_inspector_scalar(s.t))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("positions=[{positions}]"));
    }
    match g.kind {
        GradientKind::Linear { axis: GradientAxis::Y } => {
            // Default kind + default axis — omit both.
        }
        GradientKind::Linear { axis } => {
            parts.push(format!("axis={}", axis_letter(axis)));
        }
        GradientKind::Radial => {
            parts.push("kind=radial".into());
        }
    }
    format!("stops({})", parts.join(", "))
}

fn color_vec3(c: [f32; 4]) -> String {
    format!(
        "[{}, {}, {}]",
        format_inspector_scalar(c[0]),
        format_inspector_scalar(c[1]),
        format_inspector_scalar(c[2]),
    )
}

fn axis_letter(a: GradientAxis) -> &'static str {
    match a {
        GradientAxis::X => "x",
        GradientAxis::Y => "y",
        GradientAxis::Z => "z",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(t: f32, rgb: [f32; 3]) -> GradientStop {
        GradientStop { t, color: [rgb[0], rgb[1], rgb[2], 1.0] }
    }

    #[test]
    fn two_stops_axis_y_serialize_as_vertical() {
        let g = Gradient {
            kind: GradientKind::Linear { axis: GradientAxis::Y },
            stops: vec![stop(0.0, [1.0, 0.0, 0.0]), stop(1.0, [0.0, 0.0, 1.0])],
        };
        let dsl = serialize_gradient(&g);
        assert_eq!(dsl, "vertical(from=[1, 0, 0], to=[0, 0, 1])");
    }

    #[test]
    fn two_stops_axis_x_serialize_as_linear_with_axis() {
        let g = Gradient {
            kind: GradientKind::Linear { axis: GradientAxis::X },
            stops: vec![stop(0.0, [1.0, 0.5, 0.0]), stop(1.0, [0.0, 0.5, 1.0])],
        };
        let dsl = serialize_gradient(&g);
        assert_eq!(dsl, "linear(from=[1, 0.5, 0], to=[0, 0.5, 1], axis=x)");
    }

    #[test]
    fn two_stops_radial_serialize_as_radial() {
        let g = Gradient {
            kind: GradientKind::Radial,
            stops: vec![stop(0.0, [1.0, 1.0, 0.0]), stop(1.0, [0.5, 0.0, 0.0])],
        };
        let dsl = serialize_gradient(&g);
        assert_eq!(dsl, "radial(center=[1, 1, 0], edge=[0.5, 0, 0])");
    }

    #[test]
    fn three_stops_default_axis_serialize_as_stops_minimal() {
        // 3 evenly-spaced stops along the default Y axis with linear kind —
        // both axis= and kind= should be omitted, positions= should be
        // omitted because spacing is even.
        let g = Gradient {
            kind: GradientKind::Linear { axis: GradientAxis::Y },
            stops: vec![
                stop(0.0, [1.0, 0.0, 0.0]),
                stop(0.5, [0.0, 1.0, 0.0]),
                stop(1.0, [0.0, 0.0, 1.0]),
            ],
        };
        let dsl = serialize_gradient(&g);
        assert_eq!(dsl, "stops(colors=[[1, 0, 0], [0, 1, 0], [0, 0, 1]])");
    }

    #[test]
    fn three_stops_non_y_axis_includes_axis_attr() {
        let g = Gradient {
            kind: GradientKind::Linear { axis: GradientAxis::Z },
            stops: vec![
                stop(0.0, [1.0, 0.0, 0.0]),
                stop(0.5, [0.0, 1.0, 0.0]),
                stop(1.0, [0.0, 0.0, 1.0]),
            ],
        };
        let dsl = serialize_gradient(&g);
        assert!(dsl.contains("axis=z"), "axis= must be present: {dsl}");
        assert!(!dsl.contains("positions"), "evenly spaced — no positions=: {dsl}");
        assert!(!dsl.contains("kind="), "linear is default — no kind=: {dsl}");
    }

    #[test]
    fn three_stops_radial_includes_kind_not_axis() {
        let g = Gradient {
            kind: GradientKind::Radial,
            stops: vec![
                stop(0.0, [1.0, 0.0, 0.0]),
                stop(0.5, [0.0, 1.0, 0.0]),
                stop(1.0, [0.0, 0.0, 1.0]),
            ],
        };
        let dsl = serialize_gradient(&g);
        assert!(dsl.contains("kind=radial"), "kind=radial required: {dsl}");
        assert!(!dsl.contains("axis="), "radial mustn't carry axis=: {dsl}");
    }

    #[test]
    fn unevenly_spaced_stops_emit_positions_list() {
        let g = Gradient {
            kind: GradientKind::Linear { axis: GradientAxis::Y },
            stops: vec![
                stop(0.0, [1.0, 0.0, 0.0]),
                stop(0.2, [0.5, 0.5, 0.0]),
                stop(1.0, [0.0, 0.0, 1.0]),
            ],
        };
        let dsl = serialize_gradient(&g);
        assert!(dsl.contains("positions=[0, 0.2, 1]"), "positions= present: {dsl}");
    }

    #[test]
    fn two_stops_with_inner_positions_still_emit_stops_form() {
        // Two stops but neither at the [0, 1] edges — `linear(...)` only
        // accepts a 2-stop ramp pinned to those edges, so we must fall
        // through to `stops(...)`.
        let g = Gradient {
            kind: GradientKind::Linear { axis: GradientAxis::Y },
            stops: vec![stop(0.25, [1.0, 0.0, 0.0]), stop(0.75, [0.0, 0.0, 1.0])],
        };
        let dsl = serialize_gradient(&g);
        assert!(dsl.starts_with("stops("), "off-edge 2-stop must use stops(): {dsl}");
        assert!(dsl.contains("positions=[0.25, 0.75]"));
    }

    #[test]
    fn roundtrip_through_parser_preserves_gradient() {
        // Every serialised form must parse + lower back to the same
        // gradient. Exercises the four collapse paths plus the stops()
        // fallback so the editor can't emit DSL the language refuses.
        let cases: Vec<Gradient> = vec![
            Gradient {
                kind: GradientKind::Linear { axis: GradientAxis::Y },
                stops: vec![stop(0.0, [1.0, 0.0, 0.0]), stop(1.0, [0.0, 0.0, 1.0])],
            },
            Gradient {
                kind: GradientKind::Linear { axis: GradientAxis::X },
                stops: vec![stop(0.0, [1.0, 1.0, 0.0]), stop(1.0, [0.0, 1.0, 1.0])],
            },
            Gradient {
                kind: GradientKind::Radial,
                stops: vec![stop(0.0, [1.0, 0.9, 0.4]), stop(1.0, [0.45, 0.05, 0.02])],
            },
            Gradient {
                kind: GradientKind::Linear { axis: GradientAxis::Y },
                stops: vec![
                    stop(0.0, [1.0, 0.0, 0.0]),
                    stop(0.5, [0.0, 1.0, 0.0]),
                    stop(1.0, [0.0, 0.0, 1.0]),
                ],
            },
            Gradient {
                kind: GradientKind::Radial,
                stops: vec![
                    stop(0.0, [1.0, 0.0, 0.0]),
                    stop(0.5, [0.0, 1.0, 0.0]),
                    stop(1.0, [0.0, 0.0, 1.0]),
                ],
            },
        ];
        for g in cases {
            let dsl = serialize_gradient(&g);
            let src = format!(
                "material \"x\" (color=[1, 1, 1], gradient={dsl})\nscene {{ box \"b\" (size=[1,1,1], mat=\"x\") }}\n"
            );
            let result = crate::pipeline::compile(&src, None);
            assert!(
                matches!(result.stage, crate::pipeline::Stage::Ok),
                "round-trip compile failed for `{dsl}`: stage={:?} diags={:?}",
                result.stage,
                result.diagnostics
            );
            let scene = result.scene.expect("scene present");
            let got = scene.materials[0]
                .gradient
                .as_ref()
                .expect("gradient present after compile");
            assert_eq!(got.kind, g.kind, "kind mismatch for `{dsl}`");
            assert_eq!(got.stops.len(), g.stops.len(), "stop count for `{dsl}`");
            for (a, b) in got.stops.iter().zip(g.stops.iter()) {
                assert!((a.t - b.t).abs() < 1e-4, "stop t mismatch for `{dsl}`");
                for k in 0..3 {
                    assert!(
                        (a.color[k] - b.color[k]).abs() < 1e-4,
                        "stop colour mismatch for `{dsl}`: got {:?} want {:?}",
                        a.color,
                        b.color
                    );
                }
            }
        }
    }

    #[test]
    fn add_gradient_default_parses_and_lowers() {
        // The default the "+ Add gradient" button stamps in must compile —
        // a typo here would land an unparseable .mog on every fresh add.
        let src = "material \"x\" (color=[1, 1, 1], gradient=vertical(from=[0, 0, 0], to=[1, 1, 1]))\n\
                   scene { box \"b\" (size=[1,1,1], mat=\"x\") }\n";
        let result = crate::pipeline::compile(src, None);
        assert!(
            matches!(result.stage, crate::pipeline::Stage::Ok),
            "default add must parse: stage={:?} diags={:?}",
            result.stage,
            result.diagnostics
        );
    }

    #[test]
    fn stop_at_largest_gap_targets_midpoint_of_biggest_gap() {
        // Two stops at [0, 1]: midpoint of the only gap is 0.5.
        let stops = vec![stop(0.0, [1.0, 0.0, 0.0]), stop(1.0, [0.0, 0.0, 1.0])];
        let s = stop_at_largest_gap(&stops);
        assert!((s.t - 0.5).abs() < 1e-5, "expected t≈0.5, got {}", s.t);
        // Colour at t=0.5 between red and blue is mid-purple.
        assert!((s.color[0] - 0.5).abs() < 1e-5);
        assert!((s.color[2] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn stop_at_largest_gap_picks_wider_gap_with_three_stops() {
        // Stops at 0.0, 0.1, 1.0: gap [0.1, 1.0] is larger, midpoint ≈ 0.55.
        let stops = vec![
            stop(0.0, [1.0, 0.0, 0.0]),
            stop(0.1, [0.9, 0.1, 0.0]),
            stop(1.0, [0.0, 0.0, 1.0]),
        ];
        let s = stop_at_largest_gap(&stops);
        assert!((s.t - 0.55).abs() < 1e-5, "expected t≈0.55, got {}", s.t);
    }

    #[test]
    fn nearest_stop_within_returns_none_outside_threshold() {
        let stops = vec![stop(0.0, [1.0, 0.0, 0.0]), stop(1.0, [0.0, 0.0, 1.0])];
        // At t=0.5 with strip 200px and threshold 10px: nearest stop is at 0
        // or 1 → distance 100px, which is > 10px threshold.
        assert!(nearest_stop_within(&stops, 0.5, 200.0, 10.0).is_none());
    }

    #[test]
    fn nearest_stop_within_returns_closest_within_threshold() {
        let stops = vec![
            stop(0.0, [1.0, 0.0, 0.0]),
            stop(0.5, [0.0, 1.0, 0.0]),
            stop(1.0, [0.0, 0.0, 1.0]),
        ];
        // t=0.51 on a 200px strip: stop at 0.5 is 2px away, stop at 1.0 is
        // 98px away — should find index 1.
        let idx = nearest_stop_within(&stops, 0.51, 200.0, 10.0);
        assert_eq!(idx, Some(1));
    }

    #[test]
    fn nearest_stop_within_breaks_tie_by_nearest() {
        // Two stops equidistant: returns the one at lower index.
        let stops = vec![stop(0.4, [1.0, 0.0, 0.0]), stop(0.6, [0.0, 0.0, 1.0])];
        // t=0.5 on 100px strip: both stops are 10px away. `best` is updated
        // only when dist < current best, so first match wins → index 0.
        let idx = nearest_stop_within(&stops, 0.5, 100.0, 10.0);
        assert_eq!(idx, Some(0));
    }
}
