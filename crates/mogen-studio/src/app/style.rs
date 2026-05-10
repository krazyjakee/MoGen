//! Shared visual helpers for Studio dialogs and panels: accent colours
//! pulled from the active theme, primary-button factory, dim italic
//! placeholders, framed section blocks, collapsible-section helper, and a
//! seconds formatter that drops insignificant trailing zeros.
//!
//! Accents derive from the live `Visuals` so themes (Dark / Nord / Sunset /
//! Light / High-Contrast) keep ownership of their palette without a
//! per-theme constant table.

use eframe::egui;

/// Theme-derived primary-action colour. Mirrors the selection fill so that
/// "primary button" and "selected slider track / combo row" share a hue.
pub fn accent_primary(visuals: &egui::Visuals) -> egui::Color32 {
    visuals.selection.bg_fill
}

/// Theme-derived informational accent — used for ⓘ glyphs next to
/// section headings and quiet hints.
pub fn accent_info(visuals: &egui::Visuals) -> egui::Color32 {
    visuals.hyperlink_color
}

/// Theme-derived warning accent. Used for two-step confirm states.
pub fn accent_warn(visuals: &egui::Visuals) -> egui::Color32 {
    visuals.warn_fg_color
}

/// Filled accent button for the canonical action in a row (Save, Modify,
/// Animate, Refine, Generate, …). Keeps everything else as the default
/// outline button, so primary stands out.
pub fn primary_button(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    let visuals = &ui.style().visuals;
    let fill = accent_primary(visuals);
    let fg = visuals.selection.stroke.color;
    egui::Button::new(egui::RichText::new(text.to_owned()).color(fg)).fill(fill)
}

/// Dim italic placeholder text for `TextEdit::hint_text`. The default
/// hint colour can read as already-typed copy on multiline fields; italic
/// + weak shifts it visually into "this is a hint".
pub fn placeholder(text: &str) -> egui::WidgetText {
    egui::RichText::new(text).italics().weak().into()
}

/// Inspector-panel section header. `default_open` controls the initial
/// expansion state; the title renders in bold so the section reads above
/// body text without changing font size.
pub fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    default_open: bool,
    body: impl FnOnce(&mut egui::Ui) -> R,
) {
    ui.add_space(6.0);
    egui::CollapsingHeader::new(egui::RichText::new(title).strong())
        .id_salt(("studio_section", title))
        .default_open(default_open)
        .show(ui, |ui| {
            body(ui);
        });
}

/// Framed section block for the Preferences dialog. Adds a soft border,
/// a strong title, and an optional ⓘ tooltip that absorbs long-form copy
/// so the visible row is just title + controls.
pub fn framed_section<R>(
    ui: &mut egui::Ui,
    title: &str,
    hint: Option<&str>,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.add_space(6.0);
    let info_color = accent_info(&ui.style().visuals);
    let resp = egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(title).strong());
                if let Some(h) = hint {
                    ui.label(egui::RichText::new("ⓘ").color(info_color))
                        .on_hover_text(h);
                }
            });
            ui.add_space(4.0);
            body(ui)
        });
    resp.inner
}

/// Inline ⓘ + weak-text hint row. Replaces `colored_label` for
/// informational notes (env-var precedence, "Auto → resolved to …", etc.)
/// so only true alerts get colour.
pub fn info_row(ui: &mut egui::Ui, text: &str) {
    let info_color = accent_info(&ui.style().visuals);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("ⓘ").color(info_color));
        ui.label(egui::RichText::new(text).weak());
    });
}

/// Format a duration in seconds with at most two decimals, dropping
/// insignificant trailing zeros. `30.0` → `"30 s"`, `1.5` → `"1.5 s"`,
/// `5.59` → `"5.59 s"`.
pub fn format_seconds(s: f32) -> String {
    if !s.is_finite() {
        return "—".into();
    }
    if s.fract().abs() < 0.005 {
        format!("{:.0} s", s)
    } else if (s * 10.0).fract().abs() < 0.05 {
        format!("{:.1} s", s)
    } else {
        format!("{:.2} s", s)
    }
}
