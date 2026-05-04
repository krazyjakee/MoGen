use eframe::egui;

use crate::app::types::{DOCS_URL, GITHUB_REPO_URL, LICENSE_URL};
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Help → About modal. Shows the crate version, the brand line, and
    /// links back out to the GitHub repo / docs / license.
    pub(in crate::app) fn ui_about(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let mut open = true;
        let mut close_after = false;
        egui::Window::new("About MoGen Studio")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("MoGen Studio");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.add_space(6.0);
                ui.label(
                    "Desktop frontend for the MoGen pipeline — compiles \
                     declarative .mog scenes into glTF 2.0 .glb assets, with \
                     a live 3D preview and LLM-driven generate / modify / \
                     animate / texture flows.",
                );
                ui.add_space(10.0);
                ui.hyperlink_to("GitHub repository", GITHUB_REPO_URL);
                ui.hyperlink_to("Documentation (docs/dsl.md)", DOCS_URL);
                ui.hyperlink_to("License (MIT)", LICENSE_URL);
                ui.add_space(10.0);
                ui.label("© 2026 Jake Cattrall. Released under the MIT license.");
                ui.add_space(10.0);
                if ui.button("Close").clicked() {
                    close_after = true;
                }
            });
        if !open || close_after {
            self.show_about = false;
        }
    }
}
