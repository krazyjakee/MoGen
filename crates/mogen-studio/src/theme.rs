use eframe::egui::{self, Color32, Stroke};

/// Colour schemes the user can pick from in Preferences. Persisted by lowercase
/// label (see `theme_key`) so new variants can be added without a migration.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
    Sunset,
    Nord,
    HighContrast,
}

pub const THEMES: [Theme; 5] = [
    Theme::Dark,
    Theme::Light,
    Theme::Sunset,
    Theme::Nord,
    Theme::HighContrast,
];

pub const DEFAULT_THEME: Theme = Theme::Nord;

pub fn theme_key(t: Theme) -> &'static str {
    match t {
        Theme::Dark => "dark",
        Theme::Light => "light",
        Theme::Sunset => "sunset",
        Theme::Nord => "nord",
        Theme::HighContrast => "high-contrast",
    }
}

pub fn theme_label(t: Theme) -> &'static str {
    match t {
        Theme::Dark => "Dark",
        Theme::Light => "Light",
        Theme::Sunset => "Sunset (warm)",
        Theme::Nord => "Nord (cool)",
        Theme::HighContrast => "High Contrast",
    }
}

pub fn parse_theme(s: &str) -> Option<Theme> {
    match s.trim().to_ascii_lowercase().as_str() {
        "dark" => Some(Theme::Dark),
        "light" => Some(Theme::Light),
        "sunset" | "warm" => Some(Theme::Sunset),
        "nord" | "cool" => Some(Theme::Nord),
        "high-contrast" | "high_contrast" | "highcontrast" => Some(Theme::HighContrast),
        _ => None,
    }
}

pub fn apply_theme(ctx: &egui::Context, theme: Theme) {
    ctx.set_visuals(build_visuals(theme));
}

fn build_visuals(theme: Theme) -> egui::Visuals {
    match theme {
        Theme::Dark => egui::Visuals::dark(),
        Theme::Light => egui::Visuals::light(),
        Theme::Sunset => sunset(),
        Theme::Nord => nord(),
        Theme::HighContrast => high_contrast(),
    }
}

fn sunset() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(36, 28, 24);
    v.window_fill = Color32::from_rgb(44, 34, 30);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(96, 64, 44));
    v.extreme_bg_color = Color32::from_rgb(24, 18, 16);
    v.faint_bg_color = Color32::from_rgb(54, 42, 36);
    v.code_bg_color = Color32::from_rgb(28, 22, 18);
    v.selection.bg_fill = Color32::from_rgb(176, 96, 36);
    v.selection.stroke.color = Color32::from_rgb(255, 220, 170);
    v.hyperlink_color = Color32::from_rgb(240, 168, 88);
    v.warn_fg_color = Color32::from_rgb(232, 180, 64);
    v.error_fg_color = Color32::from_rgb(232, 96, 80);
    v.widgets.noninteractive.bg_fill = Color32::from_rgb(44, 34, 30);
    v.widgets.noninteractive.weak_bg_fill = Color32::from_rgb(44, 34, 30);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(72, 56, 46));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(220, 196, 168));
    v.widgets.inactive.bg_fill = Color32::from_rgb(64, 48, 40);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(54, 42, 36);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(232, 208, 176));
    v.widgets.hovered.bg_fill = Color32::from_rgb(96, 64, 44);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(80, 56, 40);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(160, 104, 56));
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::from_rgb(255, 232, 200));
    v.widgets.active.bg_fill = Color32::from_rgb(140, 88, 48);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(112, 72, 44);
    v.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    v.widgets.open.bg_fill = Color32::from_rgb(80, 56, 40);
    v.widgets.open.fg_stroke = Stroke::new(1.0, Color32::from_rgb(255, 220, 170));
    v
}

fn nord() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(46, 52, 64);
    v.window_fill = Color32::from_rgb(59, 66, 82);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(76, 86, 106));
    v.extreme_bg_color = Color32::from_rgb(36, 41, 51);
    v.faint_bg_color = Color32::from_rgb(67, 76, 94);
    v.code_bg_color = Color32::from_rgb(46, 52, 64);
    v.selection.bg_fill = Color32::from_rgb(94, 129, 172);
    v.selection.stroke.color = Color32::from_rgb(236, 239, 244);
    v.hyperlink_color = Color32::from_rgb(136, 192, 208);
    v.warn_fg_color = Color32::from_rgb(235, 203, 139);
    v.error_fg_color = Color32::from_rgb(191, 97, 106);
    v.widgets.noninteractive.bg_fill = Color32::from_rgb(59, 66, 82);
    v.widgets.noninteractive.weak_bg_fill = Color32::from_rgb(59, 66, 82);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(76, 86, 106));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(216, 222, 233));
    v.widgets.inactive.bg_fill = Color32::from_rgb(67, 76, 94);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(59, 66, 82);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(229, 233, 240));
    v.widgets.hovered.bg_fill = Color32::from_rgb(94, 105, 122);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(76, 86, 106);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(129, 161, 193));
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::from_rgb(236, 239, 244));
    // Active drives strong_text_color() — must contrast with the dark window
    // bg, not just the selected-button fill. Use nord10 + nord6 so RichText
    // .strong() stays legible on panels everywhere.
    v.widgets.active.bg_fill = Color32::from_rgb(94, 129, 172);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(94, 129, 172);
    v.widgets.active.fg_stroke = Stroke::new(1.5, Color32::from_rgb(236, 239, 244));
    v.widgets.open.bg_fill = Color32::from_rgb(76, 86, 106);
    v.widgets.open.fg_stroke = Stroke::new(1.0, Color32::from_rgb(216, 222, 233));
    v
}

fn high_contrast() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    let yellow = Color32::from_rgb(255, 213, 0);
    v.panel_fill = Color32::BLACK;
    v.window_fill = Color32::BLACK;
    v.window_stroke = Stroke::new(1.5, Color32::WHITE);
    v.extreme_bg_color = Color32::BLACK;
    v.faint_bg_color = Color32::from_rgb(20, 20, 20);
    v.code_bg_color = Color32::BLACK;
    v.override_text_color = Some(Color32::WHITE);
    v.selection.bg_fill = yellow;
    v.selection.stroke.color = Color32::BLACK;
    v.hyperlink_color = Color32::from_rgb(0, 200, 255);
    v.warn_fg_color = yellow;
    v.error_fg_color = Color32::from_rgb(255, 80, 80);
    v.widgets.noninteractive.bg_fill = Color32::BLACK;
    v.widgets.noninteractive.weak_bg_fill = Color32::BLACK;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.inactive.bg_fill = Color32::from_rgb(40, 40, 40);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(24, 24, 24);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.inactive.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    v.widgets.hovered.bg_fill = Color32::from_rgb(80, 80, 80);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(60, 60, 60);
    v.widgets.hovered.bg_stroke = Stroke::new(1.5, yellow);
    v.widgets.hovered.fg_stroke = Stroke::new(2.0, yellow);
    v.widgets.active.bg_fill = yellow;
    v.widgets.active.weak_bg_fill = Color32::from_rgb(180, 150, 0);
    v.widgets.active.bg_stroke = Stroke::new(2.0, Color32::WHITE);
    v.widgets.active.fg_stroke = Stroke::new(2.0, Color32::BLACK);
    v.widgets.open.bg_fill = Color32::from_rgb(40, 40, 40);
    v.widgets.open.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    v
}
