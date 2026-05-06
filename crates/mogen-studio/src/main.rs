mod app;
mod autocomplete;
mod crash;
mod docs;
mod edit;
mod gizmo;
mod highlight;
mod icon;
mod pick;
mod pipeline;
mod preview_shader;
mod protocol;
mod settings;
mod splash;
mod theme;
mod viewer;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // Bare-minimum argv parsing: a `--register-protocol` /
    // `--unregister-protocol` flag exits without raising the GUI, and a
    // `mogen://…` URL anywhere in argv is captured for the App to act on
    // after the splash drains. Anything else is forwarded as the
    // existing "open this file" path (handled by the App via
    // `pending_open_path` once we wire it up; today the same effect is
    // achieved by argv being passed to `eframe`'s default file-open
    // handling, which we leave alone).
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--register-protocol") {
        let outcome = protocol::register();
        println!("{}", outcome.note);
        std::process::exit(if outcome.ok { 0 } else { 1 });
    }
    if args.iter().any(|a| a == "--unregister-protocol") {
        let outcome = protocol::unregister();
        println!("{}", outcome.note);
        std::process::exit(if outcome.ok { 0 } else { 1 });
    }
    let pending_url = args.iter().find_map(|a| protocol::parse(a));

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
        Box::new(move |cc| {
            let mut app = app::MogenStudioApp::new(cc);
            if let Some(url) = pending_url {
                app.queue_protocol_url(url);
            }
            Ok(Box::new(app))
        }),
    )
}
