mod app;
mod icon;
mod pipeline;
mod settings;
mod viewer;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("mgen")
            .with_icon(icon::load())
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0]),
        depth_buffer: 24,
        multisampling: 4,
        ..Default::default()
    };
    eframe::run_native(
        "mgen",
        native_options,
        Box::new(|cc| Ok(Box::new(app::MgenApp::new(cc)))),
    )
}
