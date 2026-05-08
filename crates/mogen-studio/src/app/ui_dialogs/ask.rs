use eframe::egui;

use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// "Ask MoGen" modal raised from the editor context menu. Lets the user
    /// ask Gemini Flash a free-form question about the snippet they had
    /// selected (or the whole file if nothing was selected). Read-only — the
    /// answer is shown inline; the editor buffer is never touched.
    pub(in crate::app) fn ui_ask(&mut self, ctx: &egui::Context) {
        if !self.show_ask {
            return;
        }
        let mut open = true;
        let mut close_after = false;
        let mut submit_now = false;
        let in_flight = self.any_ask_in_flight();
        let has_key = self.resolve_api_key().is_some();
        let context_label = self.ask_context_label.clone();
        let snippet_preview = self.ask_code_context.clone();

        egui::Window::new("Ask MoGen")
            .id(egui::Id::new("ask_modal"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .default_height(480.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "Ask the fast model a question about your code. Useful for \
                     learning the DSL — \"why does this not validate?\", \"how \
                     do I make this part rotate?\", \"what does mirror do here?\".",
                );
                ui.add_space(8.0);

                ui.label(egui::RichText::new(&context_label).strong());
                ui.add_space(2.0);
                // Snippet preview — read-only, scrolls so a big file doesn't
                // blow out the dialog. Monospace because it's code.
                egui::CollapsingHeader::new("Show code being asked about")
                    .id_salt("ask_snippet_preview")
                    .default_open(false)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("ask_snippet_scroll")
                            .max_height(140.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                let mut text = snippet_preview.clone();
                                ui.add(
                                    egui::TextEdit::multiline(&mut text)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY)
                                        .interactive(false),
                                );
                            });
                    });

                ui.add_space(8.0);
                ui.label("Your question:");
                let q_id = egui::Id::new("ask_question_draft");
                crate::app::text_menu::text_edit_with_menu(
                    ui,
                    q_id,
                    &mut self.ask_question_draft,
                    |ui, text| {
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .hint_text("e.g. how does the mirror node work here?")
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .id(q_id),
                        )
                    },
                );
                if self.ask_focus_pending {
                    ui.ctx().memory_mut(|m| m.request_focus(q_id));
                    self.ask_focus_pending = false;
                }

                let provider = self.settings.provider();
                let provider_name = provider.display_name();
                if !has_key {
                    ui.add_space(4.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 200, 100),
                        format!(
                            "no {provider_name} API key — set one in Edit → Preferences…",
                        ),
                    );
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let question_ok = !self.ask_question_draft.trim().is_empty();
                    let can_ask = has_key && question_ok && !in_flight;
                    if ui
                        .add_enabled(can_ask, egui::Button::new("Ask"))
                        .on_hover_text(format!(
                            "Send the question to the active {provider_name} fast model",
                        ))
                        .clicked()
                    {
                        submit_now = true;
                    }
                    if in_flight {
                        ui.spinner();
                        ui.label(format!("asking {provider_name}…"));
                    }
                    if ui.button("Close").clicked() {
                        close_after = true;
                    }
                });

                // Answer pane — visible whenever there's something to show
                // (in-flight cleared, fresh result or error stashed).
                if let Some(result) = &self.ask_answer {
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(6.0);
                    match result {
                        Ok(text) => {
                            ui.label(egui::RichText::new("Answer").strong());
                            ui.add_space(2.0);
                            egui::ScrollArea::vertical()
                                .id_salt("ask_answer_scroll")
                                .max_height(260.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    // Selectable + wrapped so the user can
                                    // copy out paragraphs or code samples.
                                    let mut answer = text.clone();
                                    ui.add(
                                        egui::TextEdit::multiline(&mut answer)
                                            .desired_width(f32::INFINITY)
                                            .interactive(true),
                                    );
                                });
                        }
                        Err(err) => {
                            ui.colored_label(
                                egui::Color32::from_rgb(230, 120, 120),
                                format!("Ask failed: {err}"),
                            );
                        }
                    }
                }
            });

        if !open || close_after {
            self.show_ask = false;
            // Don't drop the receiver — let any in-flight call finish in the
            // background. Closing the modal just hides the answer; reopening
            // starts fresh.
            self.ask_answer = None;
        }

        if submit_now {
            self.start_ask(ctx.clone());
        }
    }
}
