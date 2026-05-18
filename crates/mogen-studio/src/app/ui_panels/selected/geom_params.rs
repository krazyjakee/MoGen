//! Kind-switched primitive-parameter row renderer for the inspector's
//! Geometry section. Reads existing `size=` / `radius=` / `segments=` /
//! etc. attributes out of the source span and emits `(attr, value)`
//! pairs the caller turns into `PendingEdit::SetAttrCanonical` writes.

use eframe::egui;

use crate::app::util::format_inspector_scalar;
use crate::edit::get_attr;

/// Classified source value for one scalar attribute.
///
/// The DSL allows an `expr` (constant arithmetic like `(0.2 + 0.05)` *or* a
/// `$param` reference) anywhere a number is expected. The old code parsed the
/// raw attr text with `str::parse::<f32>()` and, on failure, silently fell
/// back to the primitive default — so a `radius=$r` / `size=[2, 2*1, 0.1]`
/// node showed a wrong number and the first DragValue drag overwrote the
/// expression with a literal, destroying it with no warning. We instead
/// classify the raw text and lock any non-literal field.
enum Field {
    /// Attribute absent — safe to edit; show the primitive default.
    Absent,
    /// Plain numeric literal — safe to edit.
    Num(f32),
    /// Expression / `$param` — render read-only so a drag can't clobber it.
    Locked(String),
}

fn field(src: &str, span: mogen_core::Span, attr: &str) -> Field {
    match get_attr(src, span, attr) {
        None => Field::Absent,
        Some(s) => {
            let t = s.trim();
            match t.parse::<f32>() {
                Ok(n) => Field::Num(n),
                Err(_) => Field::Locked(t.to_string()),
            }
        }
    }
}

/// String-valued attribute read (used by `enum_row` for combo-box attrs like
/// `style`, `roof`, `form`). Strips surrounding quotes so the raw enum token
/// is what feeds the combo's selected-text comparison.
fn read_str(src: &str, span: mogen_core::Span, attr: &str) -> Option<String> {
    get_attr(src, span, attr).map(|s| s.trim().trim_matches('"').to_string())
}

/// Vector form of [`Field`]. `Nums` carries the parsed components only when
/// *every* component is a plain literal; a single expression component locks
/// the whole vector (editing one axis would re-emit the others as literals).
enum VecField {
    Absent,
    Nums(Vec<f32>),
    Locked(String),
}

fn vec_field(src: &str, span: mogen_core::Span, attr: &str) -> VecField {
    match get_attr(src, span, attr) {
        None => VecField::Absent,
        Some(raw) => {
            let t = raw.trim();
            let inner = t.trim_start_matches('[').trim_end_matches(']');
            let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            let nums: Option<Vec<f32>> =
                parts.iter().map(|p| p.parse::<f32>().ok()).collect();
            match nums {
                Some(n) if !n.is_empty() => VecField::Nums(n),
                _ => VecField::Locked(t.to_string()),
            }
        }
    }
}

/// Read-only row for an attribute carrying an expression / `$param`. Spells
/// out *why* it can't be dragged and points the user at the text editor —
/// the source of truth for parametric values.
fn locked_row(ui: &mut egui::Ui, label: &str, raw: &str) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(raw).monospace().weak())
            .on_hover_text(
                "Parametric value (expression or $param). Edit it in the code \
                 view — dragging here would overwrite the expression with a \
                 plain number.",
            );
        ui.label("🔒");
    });
    ui.end_row();
}

fn scalar_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    src: &str,
    span: mogen_core::Span,
    default: f32,
    speed: f32,
    out: &mut Vec<(&'static str, String)>,
) {
    match field(src, span, attr) {
        Field::Locked(raw) => locked_row(ui, label, &raw),
        f => {
            let cur = match f {
                Field::Num(n) => n,
                _ => default,
            };
            ui.label(label);
            let mut v = cur;
            if ui.add(egui::DragValue::new(&mut v).speed(speed)).changed() {
                out.push((attr, format_inspector_scalar(v)));
            }
            ui.end_row();
        }
    }
}

fn int_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    src: &str,
    span: mogen_core::Span,
    default: i32,
    min: i32,
    max: i32,
    out: &mut Vec<(&'static str, String)>,
) {
    match field(src, span, attr) {
        Field::Locked(raw) => locked_row(ui, label, &raw),
        f => {
            let cur = match f {
                Field::Num(n) => n as i32,
                _ => default,
            };
            ui.label(label);
            let mut v = cur;
            if ui
                .add(egui::DragValue::new(&mut v).speed(0.1).range(min..=max))
                .changed()
            {
                out.push((attr, v.to_string()));
            }
            ui.end_row();
        }
    }
}

/// Editable/locked multi-component row (`size`). `arity` is how many
/// DragValues to show; `default` fills missing/Absent components.
fn vec_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    src: &str,
    span: mogen_core::Span,
    arity: usize,
    default: f32,
    speed: f32,
    out: &mut Vec<(&'static str, String)>,
) {
    // Absent → all defaults. A single literal expands uniformly (matches the
    // DSL's scalar-size shorthand). Wrong-arity numeric literals fall back to
    // defaults rather than locking — they're still plain numbers, just an
    // unusual shape, and editing them is harmless.
    let mut comps = match vec_field(src, span, attr) {
        VecField::Locked(raw) => {
            locked_row(ui, label, &raw);
            return;
        }
        VecField::Absent => vec![default; arity],
        VecField::Nums(n) if n.len() == 1 => vec![n[0]; arity],
        VecField::Nums(n) if n.len() == arity => n,
        VecField::Nums(_) => vec![default; arity],
    };
    ui.label(label);
    ui.horizontal(|ui| {
        let mut emit = false;
        for c in comps.iter_mut() {
            if ui.add(egui::DragValue::new(c).speed(speed)).changed() {
                emit = true;
            }
        }
        if emit {
            let body = comps
                .iter()
                .map(|v| format_inspector_scalar(*v))
                .collect::<Vec<_>>()
                .join(", ");
            out.push((attr, format!("[{body}]")));
        }
    });
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
    let shown = current.clone().unwrap_or_else(|| fallback.to_string());
    egui::ComboBox::from_id_salt(("geom_enum", attr))
        .selected_text(&shown)
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
            vec_row(ui, "Size", "size", src, span, 3, 1.0, 0.02, out);
            shown = true;
            // Kind-specific extras
            match kind {
                "rounded_box" | "chamfered_box" => {
                    scalar_row(ui, "Radius", "radius", src, span, 0.1, 0.005, out);
                    if kind == "rounded_box" {
                        int_row(ui, "Segments", "segments", src, span, 4, 1, 32, out);
                    }
                }
                "inset_box" => {
                    scalar_row(ui, "Amount", "amount", src, span, 0.1, 0.005, out);
                    scalar_row(ui, "Depth", "depth", src, span, 0.05, 0.005, out);
                }
                "superellipsoid" => {
                    scalar_row(ui, "Equator", "ew", src, span, 1.0, 0.05, out);
                    scalar_row(ui, "Meridian", "ns", src, span, 1.0, 0.05, out);
                    int_row(ui, "Rings", "rings", src, span, 16, 2, 256, out);
                    int_row(ui, "Segments", "segments", src, span, 24, 3, 256, out);
                }
                "ellipsoid" => {
                    int_row(ui, "Rings", "rings", src, span, 16, 2, 256, out);
                    int_row(ui, "Segments", "segments", src, span, 24, 3, 256, out);
                }
                _ => {}
            }
        }
        "plane" | "quad" | "decal" | "leaf_card" | "curved_plane" => {
            // plane uses [x,z], quad/decal/leaf_card uses [x,y], curved_plane uses [x,z].
            vec_row(ui, "Size", "size", src, span, 2, 1.0, 0.05, out);
            shown = true;
            if kind == "decal" {
                scalar_row(ui, "Offset", "offset", src, span, 0.001, 0.0005, out);
            }
        }
        "cylinder" | "cone" | "half_cylinder" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, out);
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, out);
            int_row(ui, "Segments", "segments", src, span, 24, 3, 256, out);
            shown = true;
        }
        "sphere" | "hemisphere" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, out);
            let rings_default = if kind == "hemisphere" { 8 } else { 16 };
            int_row(ui, "Rings", "rings", src, span, rings_default, 2, 256, out);
            int_row(ui, "Segments", "segments", src, span, 24, 3, 256, out);
            shown = true;
        }
        "icosphere" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, out);
            int_row(ui, "Subdivisions", "subdivisions", src, span, 2, 0, 6, out);
            shown = true;
        }
        "capsule" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, out);
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, out);
            int_row(ui, "Rings", "rings", src, span, 8, 2, 64, out);
            int_row(ui, "Segments", "segments", src, span, 24, 3, 256, out);
            shown = true;
        }
        "torus" => {
            scalar_row(ui, "Major", "major", src, span, 0.5, 0.02, out);
            scalar_row(ui, "Minor", "minor", src, span, 0.15, 0.02, out);
            int_row(ui, "Major segs", "major_segments", src, span, 24, 3, 256, out);
            int_row(ui, "Minor segs", "minor_segments", src, span, 12, 3, 256, out);
            shown = true;
        }
        "torus_arc" => {
            scalar_row(ui, "Major", "major", src, span, 0.5, 0.02, out);
            scalar_row(ui, "Minor", "minor", src, span, 0.15, 0.02, out);
            scalar_row(ui, "Arc\u{00b0}", "arc", src, span, 90.0, 1.0, out);
            shown = true;
        }
        "pyramid" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, out);
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, out);
            int_row(ui, "Sides", "sides", src, span, 4, 3, 64, out);
            shown = true;
        }
        "disc" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, out);
            int_row(ui, "Segments", "segments", src, span, 24, 3, 256, out);
            shown = true;
        }
        "tube" => {
            scalar_row(ui, "Outer", "outer", src, span, 0.5, 0.02, out);
            scalar_row(ui, "Inner", "inner", src, span, 0.3, 0.02, out);
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, out);
            int_row(ui, "Segments", "segments", src, span, 24, 3, 256, out);
            shown = true;
        }
        "coil" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, out);
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, out);
            scalar_row(ui, "Turns", "turns", src, span, 3.0, 0.1, out);
            scalar_row(ui, "Profile r", "profile_radius", src, span, 0.05, 0.005, out);
            shown = true;
        }
        "frustum" => {
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, out);
            shown = true;
        }
        "branch" => {
            enum_row(ui, "Form", "form",
                read_str(src, span, "form"), "decurrent",
                &["decurrent", "excurrent", "weeping", "shrub", "palm"], out);
            scalar_row(ui, "Length", "length", src, span, 1.0, 0.02, out);
            scalar_row(ui, "Radius", "radius", src, span, 0.05, 0.005, out);
            int_row(ui, "Depth", "depth", src, span, 4, 1, 8, out);
            int_row(ui, "Splits", "splits", src, span, 2, 1, 8, out);
            scalar_row(ui, "Length falloff", "length_falloff", src, span, 0.7, 0.01, out);
            scalar_row(ui, "Radius falloff", "radius_falloff", src, span, 0.6, 0.01, out);
            scalar_row(ui, "Branch angle\u{00b0}", "branch_angle", src, span, 35.0, 1.0, out);
            scalar_row(ui, "Roll\u{00b0}", "roll", src, span, 137.5, 1.0, out);
            scalar_row(ui, "Tropism", "tropism", src, span, 0.0, 0.02, out);
            scalar_row(ui, "Bend\u{00b0}", "bend", src, span, 10.0, 0.5, out);
            scalar_row(ui, "Leader bias", "leader_bias", src, span, 0.0, 0.02, out);
            int_row(ui, "Multi-stem", "multi_stem", src, span, 1, 1, 8, out);
            int_row(ui, "Segments", "segments", src, span, 8, 3, 64, out);
            int_row(ui, "Samples", "samples", src, span, 4, 1, 16, out);
            int_row(ui, "Seed", "seed", src, span, 1, 1, 1_000_000, out);
            scalar_row(ui, "Jitter", "jitter", src, span, 0.2, 0.02, out);
            int_row(ui, "Leaves", "leaves", src, span, 1, 0, 1, out);
            scalar_row(ui, "Leaf size", "leaf_size", src, span, 0.35, 0.01, out);
            scalar_row(ui, "Leaf aspect", "leaf_aspect", src, span, 1.0, 0.05, out);
            int_row(ui, "Leaf cards", "leaf_cards", src, span, 2, 1, 8, out);
            shown = true;
        }
        "building" => {
            int_row(ui, "Seed", "seed", src, span, 1, 1, 1_000_000, out);
            enum_row(ui, "Style", "style",
                read_str(src, span, "style"), "grid",
                &["grid", "apartment-block", "hotel-corridor", "office-core",
                  "radial", "organic", "maze"], out);
            enum_row(ui, "Roof", "roof",
                read_str(src, span, "roof"), "flat",
                &["flat", "gabled", "pitched", "hipped", "mansard", "shed"], out);
            scalar_row(ui, "Floor area m\u{00b2}", "floor_area", src, span, 120.0, 1.0, out);
            int_row(ui, "Rooms", "rooms", src, span, 4, 1, 256, out);
            int_row(ui, "Floors above", "floors_above", src, span, 1, 1, 64, out);
            int_row(ui, "Floors below", "floors_below", src, span, 0, 0, 32, out);
            int_row(ui, "Windows", "windows", src, span, 0, 0, 1024, out);
            int_row(ui, "Skylights", "skylights", src, span, 0, 0, 256, out);
            scalar_row(ui, "Ceiling height", "ceiling_height", src, span, 2.6, 0.05, out);
            scalar_row(ui, "Door W", "door_w", src, span, 0.9, 0.02, out);
            scalar_row(ui, "Door H", "door_h", src, span, 2.1, 0.02, out);
            scalar_row(ui, "Window W", "window_w", src, span, 1.2, 0.02, out);
            scalar_row(ui, "Window H", "window_h", src, span, 1.4, 0.02, out);
            scalar_row(ui, "Wall thickness", "wall_thickness", src, span, 0.12, 0.005, out);
            scalar_row(ui, "Ceiling thickness", "ceiling_thickness", src, span, 0.2, 0.005, out);
            int_row(ui, "Entrances", "entrances", src, span, 1, 1, 64, out);
            int_row(ui, "Elevators", "elevators", src, span, 0, 0, 32, out);
            int_row(ui, "Staircases", "staircases", src, span, 0, 0, 32, out);
            shown = true;
        }
        _ => {}
    }
    shown
}
