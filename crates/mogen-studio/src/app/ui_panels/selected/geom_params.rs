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

fn read_str(src: &str, span: mogen_core::Span, attr: &str) -> Option<String> {
    get_attr(src, span, attr).map(|s| s.trim().trim_matches('"').to_string())
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

/// Enum row backed by a combo box. Emits the picked value quoted so the
/// caller's `SetAttrCanonical` writes a DSL string literal (e.g.
/// `style="apartment-block"`). `current` is the source value with quotes
/// already stripped; `fallback` is the lowering default shown when the attr
/// is absent so the combo never renders blank.
fn enum_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    current: Option<String>,
    fallback: &str,
    options: &[&str],
    out: &mut Vec<(&'static str, String)>,
) {
    ui.label(label);
    let shown = current.as_deref().unwrap_or(fallback);
    egui::ComboBox::from_id_salt(("geom_enum", attr))
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for opt in options {
                let selected = shown == *opt;
                if ui.selectable_label(selected, *opt).clicked() && !selected {
                    out.push((attr, format!("\"{opt}\"")));
                }
            }
        });
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
        "branch" => {
            // Procedural tree builder. The inner segments are non-editable;
            // these are the wrapper attrs the lowering pass reads (see
            // `mogen_dsl::lower::branch`). `form` seeds per-habit defaults,
            // so the numeric rows fall back to the generic lowering default
            // rather than the form-specific one when the attr is absent.
            enum_row(ui, "Form", "form",
                read_str(src, span, "form"), "decurrent",
                &["decurrent", "excurrent", "weeping", "shrub", "palm"], out);
            scalar_row(ui, "Length", "length",
                read(src, span, "length").unwrap_or(1.0), 0.02, out);
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.05), 0.005, out);
            int_row(ui, "Depth", "depth",
                read(src, span, "depth").unwrap_or(4.0) as i32, 1, 8, out);
            int_row(ui, "Splits", "splits",
                read(src, span, "splits").unwrap_or(2.0) as i32, 1, 8, out);
            scalar_row(ui, "Length falloff", "length_falloff",
                read(src, span, "length_falloff").unwrap_or(0.7), 0.01, out);
            scalar_row(ui, "Radius falloff", "radius_falloff",
                read(src, span, "radius_falloff").unwrap_or(0.6), 0.01, out);
            scalar_row(ui, "Branch angle°", "branch_angle",
                read(src, span, "branch_angle").unwrap_or(35.0), 1.0, out);
            scalar_row(ui, "Roll°", "roll",
                read(src, span, "roll").unwrap_or(137.5), 1.0, out);
            scalar_row(ui, "Tropism", "tropism",
                read(src, span, "tropism").unwrap_or(0.0), 0.02, out);
            scalar_row(ui, "Bend°", "bend",
                read(src, span, "bend").unwrap_or(10.0), 0.5, out);
            scalar_row(ui, "Leader bias", "leader_bias",
                read(src, span, "leader_bias").unwrap_or(0.0), 0.02, out);
            int_row(ui, "Multi-stem", "multi_stem",
                read(src, span, "multi_stem").unwrap_or(1.0) as i32, 1, 8, out);
            int_row(ui, "Segments", "segments",
                read(src, span, "segments").unwrap_or(8.0) as i32, 3, 64, out);
            int_row(ui, "Samples", "samples",
                read(src, span, "samples").unwrap_or(4.0) as i32, 1, 16, out);
            int_row(ui, "Seed", "seed",
                read(src, span, "seed").unwrap_or(1.0) as i32, 1, 1_000_000, out);
            scalar_row(ui, "Jitter", "jitter",
                read(src, span, "jitter").unwrap_or(0.2), 0.02, out);
            int_row(ui, "Leaves", "leaves",
                read(src, span, "leaves").unwrap_or(1.0) as i32, 0, 1, out);
            scalar_row(ui, "Leaf size", "leaf_size",
                read(src, span, "leaf_size").unwrap_or(0.35), 0.01, out);
            scalar_row(ui, "Leaf aspect", "leaf_aspect",
                read(src, span, "leaf_aspect").unwrap_or(1.0), 0.05, out);
            int_row(ui, "Leaf cards", "leaf_cards",
                read(src, span, "leaf_cards").unwrap_or(2.0) as i32, 1, 8, out);
            shown = true;
        }
        "building" => {
            // Procedural building-interior generator. The whole subtree is
            // stamped non-editable; these are the wrapper attrs the lowering
            // pass reads (see `mogen_dsl::lower::building`). Module-ref and
            // free-text style attrs are intentionally skipped — they need a
            // module/material picker, not a numeric grid.
            int_row(ui, "Seed", "seed",
                read(src, span, "seed").unwrap_or(1.0) as i32, 1, 1_000_000, out);
            enum_row(ui, "Style", "style",
                read_str(src, span, "style"), "grid",
                &["grid", "apartment-block", "hotel-corridor", "office-core", "radial", "organic", "maze"], out);
            enum_row(ui, "Roof", "roof",
                read_str(src, span, "roof"), "flat",
                &["flat", "gabled", "pitched", "hipped", "mansard", "shed"], out);
            scalar_row(ui, "Floor area m²", "floor_area",
                read(src, span, "floor_area").unwrap_or(120.0), 1.0, out);
            int_row(ui, "Rooms", "rooms",
                read(src, span, "rooms").unwrap_or(4.0) as i32, 1, 256, out);
            int_row(ui, "Floors above", "floors_above",
                read(src, span, "floors_above").unwrap_or(1.0) as i32, 1, 64, out);
            int_row(ui, "Floors below", "floors_below",
                read(src, span, "floors_below").unwrap_or(0.0) as i32, 0, 32, out);
            int_row(ui, "Windows", "windows",
                read(src, span, "windows").unwrap_or(0.0) as i32, 0, 1024, out);
            int_row(ui, "Skylights", "skylights",
                read(src, span, "skylights").unwrap_or(0.0) as i32, 0, 256, out);
            scalar_row(ui, "Ceiling height", "ceiling_height",
                read(src, span, "ceiling_height").unwrap_or(2.6), 0.05, out);
            scalar_row(ui, "Door W", "door_w",
                read(src, span, "door_w").unwrap_or(0.9), 0.02, out);
            scalar_row(ui, "Door H", "door_h",
                read(src, span, "door_h").unwrap_or(2.1), 0.02, out);
            scalar_row(ui, "Window W", "window_w",
                read(src, span, "window_w").unwrap_or(1.2), 0.02, out);
            scalar_row(ui, "Window H", "window_h",
                read(src, span, "window_h").unwrap_or(1.4), 0.02, out);
            scalar_row(ui, "Wall thickness", "wall_thickness",
                read(src, span, "wall_thickness").unwrap_or(0.12), 0.005, out);
            scalar_row(ui, "Ceiling thickness", "ceiling_thickness",
                read(src, span, "ceiling_thickness").unwrap_or(0.2), 0.005, out);
            int_row(ui, "Entrances", "entrances",
                read(src, span, "entrances").unwrap_or(1.0) as i32, 1, 64, out);
            int_row(ui, "Elevators", "elevators",
                read(src, span, "elevators").unwrap_or(0.0) as i32, 0, 32, out);
            int_row(ui, "Staircases", "staircases",
                read(src, span, "staircases").unwrap_or(0.0) as i32, 0, 32, out);
            shown = true;
        }
        _ => {}
    }
    shown
}
