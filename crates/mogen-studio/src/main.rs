mod app;
mod autocomplete;
mod crash;
mod edit;
mod gizmo;
mod highlight;
mod icon;
mod pick;
mod pipeline;
mod preview_shader;
mod settings;
mod splash;
mod theme;
mod viewer;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // Load settings before crash init so we can honour the persisted
    // crash-report consent — Sentry only attaches when the user has opted in
    // on a prior launch. The App constructor reloads the same file and
    // doesn't share this instance.
    let startup_settings = settings::Settings::load();
    let _sentry = crash::init(startup_settings.crash_reports_enabled);
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MoGen Studio")
            .with_icon(icon::load())
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0]),
        depth_buffer: 24,
        multisampling: 4,
        ..Default::default()
    };
    eframe::run_native(
        "MoGen Studio",
        native_options,
        Box::new(|cc| Ok(Box::new(app::MogenStudioApp::new(cc)))),
    )
}
