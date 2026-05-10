use eframe::egui;

use crate::app::pricing::{format_usd, image_pricing, text_pricing};
use crate::app::types::SessionUsage;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Session meter for the footer. Hidden when no calls have been made
    /// yet; the Reset button zeroes the counter. Caller is expected to be
    /// in a `right_to_left` layout — items are added rightmost-first
    /// (Reset, then the summary label).
    pub(in crate::app) fn ui_session_meter(&mut self, ui: &mut egui::Ui) {
        let u = self.session_usage.clone();
        if u.text_calls == 0 && u.image_calls == 0 {
            return;
        }
        if ui
            .small_button("Reset")
            .on_hover_text("Clear the session token / cost counters")
            .clicked()
        {
            self.session_usage = Default::default();
        }
        let tooltip = session_tooltip(&u, &self.settings.gemini_model());
        ui.label(format!(
            "· {} ({} tok, {})",
            calls_label(&u),
            u.prompt_tokens + u.response_tokens,
            format_usd(u.estimated_usd),
        ))
        .on_hover_text(tooltip);
    }
}

fn calls_label(u: &SessionUsage) -> String {
    let mut parts = Vec::new();
    if u.text_calls > 0 {
        parts.push(format!(
            "{} text call{}",
            u.text_calls,
            if u.text_calls == 1 { "" } else { "s" }
        ));
    }
    if u.image_calls > 0 {
        parts.push(format!(
            "{} image{}",
            u.image_calls,
            if u.image_calls == 1 { "" } else { "s" }
        ));
    }
    parts.join(", ")
}

fn session_tooltip(u: &SessionUsage, model: &str) -> String {
    let price = text_pricing(model);
    let img = image_pricing(model);
    format!(
        "Session totals (model: {model})\n\n\
         text calls: {}\n\
         image calls: {}\n\
         prompt tokens: {}\n\
         response tokens: {}\n\
         cached tokens: {}\n\
         estimated cost: {}\n\n\
         rates: in ${:.2}/M · out ${:.2}/M · cached ${:.2}/M · img ${:.3}/call",
        u.text_calls,
        u.image_calls,
        u.prompt_tokens,
        u.response_tokens,
        u.cached_tokens,
        format_usd(u.estimated_usd),
        price.input_per_million_usd,
        price.output_per_million_usd,
        price.cached_input_per_million_usd,
        img.per_image_usd,
    )
}
