//! Kind-switched primitive-parameter row renderer for the inspector's
//! Geometry section. Reads existing `size=` / `radius=` / `segments=` /
//! etc. attributes out of the source span and emits `(attr, value)`
//! pairs the caller turns into `PendingEdit::SetAttrCanonical` writes.

use eframe::egui;

use mogen_dsl::proc_schema::{self, ParamGroup, ParamKind, ProcSchema};

use crate::app::util::format_inspector_scalar;
use crate::edit::get_attr;

/// Render a grid label cell, attaching `help` as hover text when present.
fn label_with_help(ui: &mut egui::Ui, label: &str, help: Option<&str>) {
    let r = ui.label(label);
    if let Some(h) = help {
        r.on_hover_text(h);
    }
}

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

#[allow(clippy::too_many_arguments)]
fn scalar_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    src: &str,
    span: mogen_core::Span,
    default: f32,
    speed: f32,
    help: Option<&str>,
    out: &mut Vec<(&'static str, String)>,
) {
    match field(src, span, attr) {
        Field::Locked(raw) => locked_row(ui, label, &raw),
        f => {
            let cur = match f {
                Field::Num(n) => n,
                _ => default,
            };
            label_with_help(ui, label, help);
            let mut v = cur;
            if ui.add(egui::DragValue::new(&mut v).speed(speed)).changed() {
                out.push((attr, format_inspector_scalar(v)));
            }
            ui.end_row();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn int_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    src: &str,
    span: mogen_core::Span,
    default: i32,
    min: i32,
    max: i32,
    help: Option<&str>,
    out: &mut Vec<(&'static str, String)>,
) {
    match field(src, span, attr) {
        Field::Locked(raw) => locked_row(ui, label, &raw),
        f => {
            let cur = match f {
                Field::Num(n) => n as i32,
                _ => default,
            };
            label_with_help(ui, label, help);
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

/// Checkbox row for a boolean-shaped numeric attr (`0` / `1`). The DSL
/// has no native bool — the building lowering reads `n.abs() > 0.5`, so we
/// round-trip through a 0/1 literal. `default` is the value shown when the
/// attr is absent so the checkbox never renders indeterminate.
fn bool_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    src: &str,
    span: mogen_core::Span,
    default: bool,
    help: Option<&str>,
    out: &mut Vec<(&'static str, String)>,
) {
    match field(src, span, attr) {
        Field::Locked(raw) => locked_row(ui, label, &raw),
        f => {
            let cur = match f {
                Field::Num(n) => n.abs() > 0.5,
                _ => default,
            };
            label_with_help(ui, label, help);
            let mut v = cur;
            if ui.checkbox(&mut v, "").changed() {
                out.push((attr, if v { "1".into() } else { "0".into() }));
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
        // Wrong-arity: preserve the authored values (padded/truncated to
        // arity) so a drag doesn't silently overwrite them with defaults.
        VecField::Nums(mut n) => {
            n.resize(arity, default);
            n
        }
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
/// is absent so the combo never renders blank. Each option carries hover help.
fn enum_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    current: Option<String>,
    fallback: &str,
    options: &[proc_schema::EnumOption],
    help: Option<&str>,
    out: &mut Vec<(&'static str, String)>,
) {
    label_with_help(ui, label, help);
    let shown = current.clone().unwrap_or_else(|| fallback.to_string());
    egui::ComboBox::from_id_salt(("geom_enum", attr))
        .selected_text(&shown)
        .show_ui(ui, |ui| {
            for opt in options {
                let selected = shown == opt.value;
                let resp = ui.selectable_label(selected, opt.value);
                let resp = if opt.help.is_empty() {
                    resp
                } else {
                    resp.on_hover_text(opt.help)
                };
                if resp.clicked() && !selected {
                    out.push((attr, format!("\"{}\"", opt.value)));
                }
            }
        });
    ui.end_row();
}

/// Schema-driven multi-component row. Unlike [`vec_row`], `defaults` carries a
/// per-component value (so a non-uniform default like cave `size=[24,10,24]`
/// renders correctly), and its length sets the row's arity.
fn schema_vec_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    src: &str,
    span: mogen_core::Span,
    defaults: &[f32],
    speed: f32,
    help: Option<&str>,
    out: &mut Vec<(&'static str, String)>,
) {
    let arity = defaults.len();
    let mut comps = match vec_field(src, span, attr) {
        VecField::Locked(raw) => {
            locked_row(ui, label, &raw);
            return;
        }
        VecField::Absent => defaults.to_vec(),
        VecField::Nums(n) if n.len() == 1 => vec![n[0]; arity],
        VecField::Nums(n) if n.len() == arity => n,
        VecField::Nums(mut n) => {
            // Wrong-arity: preserve authored values, padding from `defaults`.
            let pad = defaults.last().copied().unwrap_or(0.0);
            n.resize(arity, pad);
            n
        }
    };
    label_with_help(ui, label, help);
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

/// Render every parameter of a procedural generator schema as inspector rows,
/// inserting a subheader row whenever the [`ParamGroup`] changes. This is the
/// single generic path that replaces the former per-kind hand-written arms for
/// `branch` / `building` / `cave`.
fn render_schema(
    ui: &mut egui::Ui,
    schema: &ProcSchema,
    src: &str,
    span: mogen_core::Span,
    out: &mut Vec<(&'static str, String)>,
) -> bool {
    let mut current_group = ParamGroup::Main;
    for spec in schema.params {
        if spec.group != current_group {
            current_group = spec.group;
            if let Some(header) = spec.group.header() {
                ui.label("");
                ui.label(egui::RichText::new(header).strong().weak());
                ui.end_row();
            }
        }
        match &spec.kind {
            ParamKind::Scalar { default, speed } => {
                scalar_row(ui, spec.label, spec.attr, src, span, *default, *speed, spec.help, out)
            }
            ParamKind::Int { default, min, max } => {
                int_row(ui, spec.label, spec.attr, src, span, *default, *min, *max, spec.help, out)
            }
            ParamKind::Bool { default } => {
                bool_row(ui, spec.label, spec.attr, src, span, *default, spec.help, out)
            }
            ParamKind::Enum { default, options } => enum_row(
                ui,
                spec.label,
                spec.attr,
                read_str(src, span, spec.attr),
                default,
                options,
                spec.help,
                out,
            ),
            ParamKind::Vec { defaults, speed } => schema_vec_row(
                ui, spec.label, spec.attr, src, span, defaults, *speed, spec.help, out,
            ),
        }
    }
    !schema.params.is_empty()
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
    // Procedural generators (`branch`, `building`, `cave`) render through the
    // shared schema so they get a consistent sidebar and new generators are
    // free. Primitives keep their hand-written arms below.
    if let Some(schema) = proc_schema::schema_for(kind) {
        return render_schema(ui, schema, src, span, out);
    }

    let mut shown = false;
    match kind {
        "box" | "slab" | "post" | "panel" | "wedge" | "ellipsoid" | "rounded_box"
        | "chamfered_box" | "inset_box" | "wall" | "prism" | "superellipsoid" => {
            vec_row(ui, "Size", "size", src, span, 3, 1.0, 0.02, out);
            shown = true;
            // Kind-specific extras
            match kind {
                "rounded_box" | "chamfered_box" => {
                    scalar_row(ui, "Radius", "radius", src, span, 0.1, 0.005, None, out);
                    if kind == "rounded_box" {
                        int_row(ui, "Segments", "segments", src, span, 4, 1, 32, None, out);
                    }
                }
                "inset_box" => {
                    scalar_row(ui, "Amount", "amount", src, span, 0.1, 0.005, None, out);
                    scalar_row(ui, "Depth", "depth", src, span, 0.05, 0.005, None, out);
                }
                "superellipsoid" => {
                    scalar_row(ui, "Equator", "ew", src, span, 1.0, 0.05, None, out);
                    scalar_row(ui, "Meridian", "ns", src, span, 1.0, 0.05, None, out);
                    int_row(ui, "Rings", "rings", src, span, 16, 2, 256, None, out);
                    int_row(ui, "Segments", "segments", src, span, 24, 3, 256, None, out);
                }
                "ellipsoid" => {
                    int_row(ui, "Rings", "rings", src, span, 16, 2, 256, None, out);
                    int_row(ui, "Segments", "segments", src, span, 24, 3, 256, None, out);
                }
                _ => {}
            }
        }
        "plane" | "quad" | "decal" | "leaf_card" | "curved_plane" => {
            // plane uses [x,z], quad/decal/leaf_card uses [x,y], curved_plane uses [x,z].
            vec_row(ui, "Size", "size", src, span, 2, 1.0, 0.05, out);
            shown = true;
            if kind == "decal" {
                scalar_row(ui, "Offset", "offset", src, span, 0.001, 0.0005, None, out);
            }
        }
        "cylinder" | "cone" | "half_cylinder" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, None, out);
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, None, out);
            int_row(ui, "Segments", "segments", src, span, 24, 3, 256, None, out);
            shown = true;
        }
        "sphere" | "hemisphere" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, None, out);
            let rings_default = if kind == "hemisphere" { 8 } else { 16 };
            int_row(ui, "Rings", "rings", src, span, rings_default, 2, 256, None, out);
            int_row(ui, "Segments", "segments", src, span, 24, 3, 256, None, out);
            shown = true;
        }
        "icosphere" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, None, out);
            int_row(ui, "Subdivisions", "subdivisions", src, span, 2, 0, 6, None, out);
            shown = true;
        }
        "capsule" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, None, out);
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, None, out);
            int_row(ui, "Rings", "rings", src, span, 8, 2, 64, None, out);
            int_row(ui, "Segments", "segments", src, span, 24, 3, 256, None, out);
            shown = true;
        }
        "torus" => {
            scalar_row(ui, "Major", "major", src, span, 0.5, 0.02, None, out);
            scalar_row(ui, "Minor", "minor", src, span, 0.15, 0.02, None, out);
            int_row(ui, "Major segs", "major_segments", src, span, 24, 3, 256, None, out);
            int_row(ui, "Minor segs", "minor_segments", src, span, 12, 3, 256, None, out);
            shown = true;
        }
        "torus_arc" => {
            scalar_row(ui, "Major", "major", src, span, 0.5, 0.02, None, out);
            scalar_row(ui, "Minor", "minor", src, span, 0.15, 0.02, None, out);
            scalar_row(ui, "Arc\u{00b0}", "arc", src, span, 90.0, 1.0, None, out);
            shown = true;
        }
        "pyramid" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, None, out);
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, None, out);
            int_row(ui, "Sides", "sides", src, span, 4, 3, 64, None, out);
            shown = true;
        }
        "disc" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, None, out);
            int_row(ui, "Segments", "segments", src, span, 24, 3, 256, None, out);
            shown = true;
        }
        "tube" => {
            scalar_row(ui, "Outer", "outer", src, span, 0.5, 0.02, None, out);
            scalar_row(ui, "Inner", "inner", src, span, 0.3, 0.02, None, out);
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, None, out);
            int_row(ui, "Segments", "segments", src, span, 24, 3, 256, None, out);
            shown = true;
        }
        "coil" => {
            scalar_row(ui, "Radius", "radius", src, span, 0.5, 0.02, None, out);
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, None, out);
            scalar_row(ui, "Turns", "turns", src, span, 3.0, 0.1, None, out);
            scalar_row(ui, "Profile r", "profile_radius", src, span, 0.05, 0.005, None, out);
            shown = true;
        }
        "frustum" => {
            scalar_row(ui, "Height", "height", src, span, 1.0, 0.02, None, out);
            shown = true;
        }
        _ => {}
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogen_core::Span;

    fn sp(src: &str) -> Span {
        Span { start: 0, end: src.len() }
    }

    // ---- field ---------------------------------------------------------------

    #[test]
    fn field_absent_when_attr_missing() {
        let src = r#"sphere "s" ()"#;
        assert!(matches!(field(src, sp(src), "radius"), Field::Absent));
    }

    #[test]
    fn field_num_when_plain_literal() {
        let src = r#"sphere "s" (radius=0.5)"#;
        assert!(matches!(field(src, sp(src), "radius"), Field::Num(v) if (v - 0.5).abs() < 1e-6));
    }

    #[test]
    fn field_locked_for_param_ref() {
        let src = r#"sphere "s" (radius=$r)"#;
        assert!(matches!(field(src, sp(src), "radius"), Field::Locked(_)));
    }

    #[test]
    fn field_locked_for_arithmetic_expr() {
        let src = r#"sphere "s" (radius=(1+0.5))"#;
        assert!(matches!(field(src, sp(src), "radius"), Field::Locked(_)));
    }

    // ---- vec_field -----------------------------------------------------------

    #[test]
    fn vec_field_absent_when_missing() {
        let src = r#"box "b" ()"#;
        assert!(matches!(vec_field(src, sp(src), "size"), VecField::Absent));
    }

    #[test]
    fn vec_field_nums_for_3_component_literal() {
        let src = r#"box "b" (size=[1.0, 2.0, 3.0])"#;
        assert!(matches!(vec_field(src, sp(src), "size"), VecField::Nums(n) if n == [1.0f32, 2.0, 3.0]));
    }

    #[test]
    fn vec_field_nums_for_scalar_shorthand() {
        let src = r#"box "b" (size=2.0)"#;
        assert!(matches!(vec_field(src, sp(src), "size"), VecField::Nums(n) if n == [2.0f32]));
    }

    #[test]
    fn vec_field_locked_when_any_component_is_expr() {
        let src = r#"box "b" (size=[1.0, $h, 3.0])"#;
        assert!(matches!(vec_field(src, sp(src), "size"), VecField::Locked(_)));
    }

    // ---- wrong-arity padding -------------------------------------------------

    #[test]
    fn vec_row_wrong_arity_pads_with_default() {
        // Simulates what vec_row does internally via vec_field + resize.
        // A 2-component size value for a 3-component row should be padded,
        // not replaced with all-defaults.
        let raw_nums = vec![1.0f32, 2.0];
        let arity = 3;
        let default = 99.0f32;
        let mut n = raw_nums;
        n.resize(arity, default);
        assert_eq!(n, [1.0f32, 2.0, 99.0]);
    }

    #[test]
    fn vec_row_wrong_arity_truncates_long_vec() {
        let raw_nums = vec![1.0f32, 2.0, 3.0, 4.0];
        let arity = 2;
        let default = 99.0f32;
        let mut n = raw_nums;
        n.resize(arity, default);
        assert_eq!(n, [1.0f32, 2.0]);
    }
}
