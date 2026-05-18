use eframe::egui;

use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Imposter-preview modal. Shows the baked yaw-grid spritesheet the
    /// `bundle_lods_and_imposter` export embeds — same cell size, view count,
    /// and pitch — so the user can judge the billboard before shipping it.
    pub(in crate::app) fn ui_imposter_preview(&mut self, ctx: &egui::Context) {
        if !self.show_imposter {
            return;
        }
        let in_flight = self.imposter_rx.is_some();
        let mut open = true;
        let mut rebake = false;

        egui::Window::new("Imposter preview")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(600.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "Scene-wide imposter billboard baked exactly as the \
                     `bundle_lods_and_imposter` export embeds it.",
                );
                ui.add_space(8.0);

                if in_flight {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("baking imposter atlas…");
                    });
                } else if let Some(err) = &self.imposter_err {
                    ui.colored_label(
                        egui::Color32::LIGHT_RED,
                        format!("imposter bake failed: {err}"),
                    );
                } else if let Some(p) = &self.imposter_preview {
                    // Scale the wide atlas down to fit the modal while
                    // keeping aspect; never upscale past 1:1.
                    let max_w = 560.0_f32;
                    let scale = (max_w / p.width as f32).min(1.0);
                    let size = egui::vec2(
                        p.width as f32 * scale,
                        p.height as f32 * scale,
                    );
                    egui::Frame::none()
                        .fill(egui::Color32::from_gray(40))
                        .inner_margin(egui::Margin::same(4.0))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Image::new(&p.texture)
                                    .fit_to_exact_size(size),
                            );
                        });
                    ui.add_space(6.0);
                    ui.label(format!(
                        "{} views · {}² px cells · {}×{} atlas",
                        p.view_count, p.cell_size, p.width, p.height
                    ));
                    ui.label(
                        egui::RichText::new(
                            "The companion godot-mog runtime picks the cell \
                             matching the camera yaw. Plain glTF viewers map \
                             the whole sheet across the quad (every angle \
                             tiled) — that's expected, not a bake error.",
                        )
                        .weak(),
                    );
                } else {
                    ui.label("(no imposter baked yet)");
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!in_flight, egui::Button::new("Re-bake"))
                        .on_hover_text("Bake the imposter again from the current scene")
                        .clicked()
                    {
                        rebake = true;
                    }
                    if ui.button("Close").clicked() {
                        self.show_imposter = false;
                    }
                });
            });

        if !open {
            self.show_imposter = false;
        }
        if rebake {
            self.start_imposter_preview(ctx);
        }
    }
}
