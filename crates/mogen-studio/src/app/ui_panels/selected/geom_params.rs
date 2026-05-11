//! Kind-switched primitive-parameter row renderer for the inspector's
//! Geometry section. Reads existing `size=` / `radius=` / `segments=` /
//! etc. attributes out of the source span and emits `(attr, value)`
//! pairs the caller turns into `PendingEdit::SetAttrCanonical` writes.

use eframe::egui;

use crate::app::util::format_inspector_scalar;
use crate::edit::get_attr;

fn read(src: &str, span: mogen_core::Span, attr: &str) -> Option<f32> {
    get_attr(src, span, attr).and_then(|s| s.parse::<f32>().ok())
}

fn read_vec3(src: &str, span: mogen_core::Span, attr: &str) -> Option<[f32; 3]> {
    let raw = get_attr(src, span, attr)?;
    let trimmed = raw.trim().trim_start_matches('[').trim_end_matches(']');
    let parts: Vec<f32> = trimmed
        .split(',')
        .filter_map(|s| s.trim().parse::<f32>().ok())
        .collect();
    match parts.len() {
        1 => Some([parts[0], parts[0], parts[0]]),
        3 => Some([parts[0], parts[1], parts[2]]),
        _ => None,
    }
}

fn read_vec2(src: &str, span: mogen_core::Span, attr: &str) -> Option<[f32; 2]> {
    let raw = get_attr(src, span, attr)?;
    let trimmed = raw.trim().trim_start_matches('[').trim_end_matches(']');
    let parts: Vec<f32> = trimmed
        .split(',')
        .filter_map(|s| s.trim().parse::<f32>().ok())
        .collect();
    match parts.len() {
        1 => Some([parts[0], parts[0]]),
        2 => Some([parts[0], parts[1]]),
        _ => None,
    }
}

fn scalar_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    current: f32,
    speed: f32,
    out: &mut Vec<(&'static str, String)>,
) {
    ui.label(label);
    let mut v = current;
    if ui.add(egui::DragValue::new(&mut v).speed(speed)).changed() {
        out.push((attr, format_inspector_scalar(v)));
    }
    ui.end_row();
}

fn int_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    current: i32,
    min: i32,
    max: i32,
    out: &mut Vec<(&'static str, String)>,
) {
    ui.label(label);
    let mut v = current;
    if ui
        .add(egui::DragValue::new(&mut v).speed(0.1).range(min..=max))
        .changed()
    {
        out.push((attr, v.to_string()));
    }
    ui.end_row();
}

/// Render scalar geometry-parameter rows for `kind`. Returns `true` when at
/// least one editable row was added, so the caller can paint a "(no editable
/// params)" hint when the kind has nothing to show. Each numeric attr falls
/// back to its primitive lowering default — see `mogen_dsl::lower::primitive`.
/// List-shaped attrs (`points`, `profile`, `path`, `holes`) are intentionally
/// skipped — they need a richer editor than a sidebar grid.
///
/// Out entries are pre-formatted `(attr_name, value_literal)` pairs so the
/// helper can emit list-shaped writes (`size=[1, 2, 3]`) without the caller
/// having to know which attrs are scalar vs. vector.
pub(in crate::app) fn geom_params_for_kind(
    ui: &mut egui::Ui,
    kind: &str,
    src: &str,
    span: mogen_core::Span,
    out: &mut Vec<(&'static str, String)>,
) -> bool {
    let mut shown = false;
    match kind {
        "box" | "slab" | "post" | "panel" | "wedge" | "ellipsoid" | "rounded_box"
        | "chamfered_box" | "inset_box" | "wall" | "prism" | "superellipsoid" => {
            let s = read_vec3(src, span, "size").unwrap_or([1.0, 1.0, 1.0]);
            ui.label("Size");
            ui.horizontal(|ui| {
                let mut sx = s[0];
                let mut sy = s[1];
                let mut sz = s[2];
                let mut emit = false;
                if ui.add(egui::DragValue::new(&mut sx).speed(0.02)).changed() { emit = true; }
                if ui.add(egui::DragValue::new(&mut sy).speed(0.02)).changed() { emit = true; }
                if ui.add(egui::DragValue::new(&mut sz).speed(0.02)).changed() { emit = true; }
                if emit {
                    out.push((
                        "size",
                        format!(
                            "[{}, {}, {}]",
                            format_inspector_scalar(sx),
                            format_inspector_scalar(sy),
                            format_inspector_scalar(sz),
                        ),
                    ));
                }
            });
            ui.end_row();
            shown = true;
            // Kind-specific extras
            match kind {
                "rounded_box" | "chamfered_box" => {
                    scalar_row(ui, "Radius", "radius",
                        read(src, span, "radius").unwrap_or(0.1), 0.005, out);
                    if kind == "rounded_box" {
                        let segs = read(src, span, "segments").unwrap_or(4.0) as i32;
                        int_row(ui, "Segments", "segments", segs, 1, 32, out);
                    }
                }
                "inset_box" => {
                    scalar_row(ui, "Amount", "amount",
                        read(src, span, "amount").unwrap_or(0.1), 0.005, out);
                    scalar_row(ui, "Depth", "depth",
                        read(src, span, "depth").unwrap_or(0.05), 0.005, out);
                }
                "superellipsoid" => {
                    scalar_row(ui, "Equator", "ew",
                        read(src, span, "ew").unwrap_or(1.0), 0.05, out);
                    scalar_row(ui, "Meridian", "ns",
                        read(src, span, "ns").unwrap_or(1.0), 0.05, out);
                    let r = read(src, span, "rings").unwrap_or(16.0) as i32;
                    let s = read(src, span, "segments").unwrap_or(24.0) as i32;
                    int_row(ui, "Rings", "rings", r, 2, 256, out);
                    int_row(ui, "Segments", "segments", s, 3, 256, out);
                }
                "ellipsoid" => {
                    let r = read(src, span, "rings").unwrap_or(16.0) as i32;
                    let s = read(src, span, "segments").unwrap_or(24.0) as i32;
                    int_row(ui, "Rings", "rings", r, 2, 256, out);
                    int_row(ui, "Segments", "segments", s, 3, 256, out);
                }
                _ => {}
            }
        }
        "plane" | "quad" | "decal" | "leaf_card" | "curved_plane" => {
            // plane uses [x,z], quad/decal/leaf_card uses [x,y], curved_plane uses [x,z].
            let s = read_vec2(src, span, "size").unwrap_or([1.0, 1.0]);
            ui.label("Size");
            ui.horizontal(|ui| {
                let mut su = s[0];
                let mut sv = s[1];
                let mut emit = false;
                if ui.add(egui::DragValue::new(&mut su).speed(0.05)).changed() { emit = true; }
                if ui.add(egui::DragValue::new(&mut sv).speed(0.05)).changed() { emit = true; }
                if emit {
                    out.push((
                        "size",
                        format!(
                            "[{}, {}]",
                            format_inspector_scalar(su),
                            format_inspector_scalar(sv),
                        ),
                    ));
                }
            });
            ui.end_row();
            shown = true;
            if kind == "decal" {
                scalar_row(ui, "Offset", "offset",
                    read(src, span, "offset").unwrap_or(0.001), 0.0005, out);
            }
        }
        "cylinder" | "cone" | "half_cylinder" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            let s = read(src, span, "segments").unwrap_or(24.0) as i32;
            int_row(ui, "Segments", "segments", s, 3, 256, out);
            shown = true;
        }
        "sphere" | "hemisphere" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            let r = read(src, span, "rings").unwrap_or(if kind == "hemisphere" { 8.0 } else { 16.0 }) as i32;
            let s = read(src, span, "segments").unwrap_or(24.0) as i32;
            int_row(ui, "Rings", "rings", r, 2, 256, out);
            int_row(ui, "Segments", "segments", s, 3, 256, out);
            shown = true;
        }
        "icosphere" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            let s = read(src, span, "subdivisions").unwrap_or(2.0) as i32;
            int_row(ui, "Subdivisions", "subdivisions", s, 0, 6, out);
            shown = true;
        }
        "capsule" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            let r = read(src, span, "rings").unwrap_or(8.0) as i32;
            let s = read(src, span, "segments").unwrap_or(24.0) as i32;
            int_row(ui, "Rings", "rings", r, 2, 64, out);
            int_row(ui, "Segments", "segments", s, 3, 256, out);
            shown = true;
        }
        "torus" => {
            scalar_row(ui, "Major", "major",
                read(src, span, "major").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Minor", "minor",
                read(src, span, "minor").unwrap_or(0.15), 0.02, out);
            let mj = read(src, span, "major_segments").unwrap_or(24.0) as i32;
            let mn = read(src, span, "minor_segments").unwrap_or(12.0) as i32;
            int_row(ui, "Major segs", "major_segments", mj, 3, 256, out);
            int_row(ui, "Minor segs", "minor_segments", mn, 3, 256, out);
            shown = true;
        }
        "torus_arc" => {
            scalar_row(ui, "Major", "major",
                read(src, span, "major").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Minor", "minor",
                read(src, span, "minor").unwrap_or(0.15), 0.02, out);
            scalar_row(ui, "Arc°", "arc",
                read(src, span, "arc").unwrap_or(90.0), 1.0, out);
            shown = true;
        }
        "pyramid" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            let s = read(src, span, "sides").unwrap_or(4.0) as i32;
            int_row(ui, "Sides", "sides", s, 3, 64, out);
            shown = true;
        }
        "disc" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            let s = read(src, span, "segments").unwrap_or(24.0) as i32;
            int_row(ui, "Segments", "segments", s, 3, 256, out);
            shown = true;
        }
        "tube" => {
            scalar_row(ui, "Outer", "outer",
                read(src, span, "outer").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Inner", "inner",
                read(src, span, "inner").unwrap_or(0.3), 0.02, out);
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            let s = read(src, span, "segments").unwrap_or(24.0) as i32;
            int_row(ui, "Segments", "segments", s, 3, 256, out);
            shown = true;
        }
        "coil" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            scalar_row(ui, "Turns", "turns",
                read(src, span, "turns").unwrap_or(3.0), 0.1, out);
            scalar_row(ui, "Profile r", "profile_radius",
                read(src, span, "profile_radius").unwrap_or(0.05), 0.005, out);
            shown = true;
        }
        "frustum" => {
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            shown = true;
        }
        _ => {}
    }
    shown
}
