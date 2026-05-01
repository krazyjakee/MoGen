use eframe::egui;

use crate::app::types::EnhanceTarget;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Inline "Enhance" button shown directly under the four prompt inputs
    /// (Generate in the modal, Modify / Animate / Texture-Style in the
    /// inspector). Clicking kicks off a context-specific rewrite via the fast
    /// model; on success the rewritten text replaces the input in place, on
    /// failure the error is rendered alongside the button until the next
    /// enhance attempt. Disabled globally while another enhance is in flight
    /// or no API key is configured.
    pub(in crate::app) fn ui_enhance_button(
        &mut self,
        ui: &mut egui::Ui,
        target: EnhanceTarget,
        hover: &str,
    ) {
        let has_key = self.resolve_api_key().is_some();
        let any_busy = self.enhance_in_flight.is_some();
        let this_busy = matches!(
            self.enhance_in_flight.as_ref(),
            Some(s) if s.target == target
        );
        let can_run = has_key && !any_busy;
        let err_for_target = match &self.enhance_error {
            Some((t, msg)) if *t == target => Some(msg.clone()),
            _ => None,
        };

        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_run, egui::Button::new("✨ Enhance").small())
                .on_hover_text(hover)
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.start_prompt_enhance(ctx, target);
            }
            if this_busy {
                ui.spinner();
                ui.label(egui::RichText::new("enhancing…").small().weak());
            } else if let Some(msg) = err_for_target {
                ui.label(
                    egui::RichText::new(msg)
                        .small()
                        .color(ui.visuals().warn_fg_color),
                );
            } else if !has_key {
                ui.label(
                    egui::RichText::new("no API key")
                        .small()
                        .color(egui::Color32::from_rgb(230, 200, 100)),
                );
            }
        });
    }
}
