use eframe::egui;

use crate::app::types::{LlmErrorClass, LlmExtraAction};
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Classified error banner with Retry / Open Settings / Dismiss actions.
    /// No-op when the active file has no pending error.
    pub(super) fn ui_llm_error_banner(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.active().llm_error.clone() else {
            return;
        };
        let accent = match info.class {
            LlmErrorClass::MissingKey | LlmErrorClass::InvalidKey => {
                egui::Color32::from_rgb(230, 150, 80)
            }
            LlmErrorClass::RateLimited
            | LlmErrorClass::Network
            | LlmErrorClass::ServerError => egui::Color32::from_rgb(230, 200, 100),
            LlmErrorClass::ContentBlocked => egui::Color32::from_rgb(200, 130, 200),
            LlmErrorClass::QuotaExceeded
            | LlmErrorClass::BadRequest
            | LlmErrorClass::Other => egui::Color32::from_rgb(230, 100, 100),
        };
        egui::Frame::none()
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, accent))
            .inner_margin(egui::Margin::same(6.0))
            .show(ui, |ui| {
                ui.colored_label(accent, egui::RichText::new(&info.headline).strong());
                ui.label(egui::RichText::new(&info.detail).small());
                ui.horizontal(|ui| {
                    let retry_label = if info.retryable { "Retry" } else { "Retry anyway" };
                    let has_last = self.active().llm_last_prompt.is_some();
                    if ui
                        .add_enabled(has_last, egui::Button::new(retry_label))
                        .on_hover_text(
                            "Re-submit the last prompt. Edit the prompt field above first \
                             if you want to tweak it before retrying.",
                        )
                        .clicked()
                    {
                        let ctx = ui.ctx().clone();
                        self.retry_active_llm(ctx);
                    }
                    if matches!(
                        info.class,
                        LlmErrorClass::MissingKey
                            | LlmErrorClass::InvalidKey
                            | LlmErrorClass::QuotaExceeded
                    ) && ui.button("Open Settings…").clicked()
                    {
                        self.show_options = true;
                    }
                    if info.action == Some(LlmExtraAction::ForceRegenerateTextures)
                        && ui
                            .button("New textures")
                            .on_hover_text(
                                "Regenerate every material's PBR set from scratch, \
                                 ignoring existing PNGs and spliced texture paths.",
                            )
                            .clicked()
                    {
                        let ctx = ui.ctx().clone();
                        self.start_llm_textures_force(ctx);
                    }
                    if ui
                        .small_button("Dismiss")
                        .on_hover_text("Hide this message without retrying")
                        .clicked()
                    {
                        self.active_mut().llm_error = None;
                    }
                });
            });
        ui.add_space(6.0);
    }
}
