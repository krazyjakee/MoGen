use eframe::egui;

use crate::app::MogenStudioApp;
use crate::settings::{thinking_level_label, THINKING_LEVELS};

impl MogenStudioApp {
    /// Small "Thinking (this file): [dropdown]" row. Writing a level splices a
    /// `// mogen-generate thinking=<level>` header into the .mog on the next
    /// LLM call, which both the CLI and Studio read back on subsequent runs.
    pub(super) fn ui_llm_thinking_override(&mut self, ui: &mut egui::Ui) {
        let current = self.active().thinking_override;
        let global = self.settings.thinking_level();
        let preview = match current {
            Some(level) => thinking_level_label(level),
            None => "Use global default",
        };
        ui.horizontal(|ui| {
            ui.label("Thinking (this file):")
                .on_hover_text(
                    "Per-file cap on the model's reasoning budget (applies to \
                     providers that support a thinking budget — Gemini, OpenAI). \
                     Saved into the .mog header so it applies to CLI runs too. \
                     Leave as \"Use global default\" to defer to Options.",
                );
            egui::ComboBox::from_id_salt(("mog_thinking_override", self.active))
                .selected_text(preview)
                .show_ui(ui, |ui| {
                    let default_label =
                        format!("Use global default ({})", thinking_level_label(global));
                    if ui
                        .selectable_label(current.is_none(), default_label)
                        .clicked()
                    {
                        self.active_mut().thinking_override = None;
                    }
                    for level in THINKING_LEVELS {
                        let selected = current == Some(level);
                        if ui
                            .selectable_label(selected, thinking_level_label(level))
                            .clicked()
                        {
                            self.active_mut().thinking_override = Some(level);
                        }
                    }
                });
            // Tiny hint about how the override is persisted. Quiet so it
            // doesn't shout at users who never touched the dropdown.
            if current.is_some() {
                ui.label(
                    egui::RichText::new("(saved in file header)")
                        .small()
                        .weak(),
                );
            }
        });
    }
}
