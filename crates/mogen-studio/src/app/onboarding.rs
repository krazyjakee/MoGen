use eframe::egui;

use super::MogenStudioApp;

/// Where Google AI Studio surfaces the user's API key. Linked from the
/// onboarding modal so a brand-new user can mint a key without having to
/// figure out the Gemini console layout themselves.
const GEMINI_API_KEY_URL: &str = "https://aistudio.google.com/apikey";

impl MogenStudioApp {
    /// First-launch welcome flow. Shown once per install (gated on
    /// `Settings::onboarded`) after the splash drains. Explains what the
    /// Gemini key is used for, links out to AI Studio, and accepts a paste
    /// inline so a returning user doesn't have to hunt through Preferences.
    ///
    /// Both buttons (Get Started, Skip) latch `onboarded = true` so the
    /// modal never reappears — the inline "no Gemini API key" hints in the
    /// LLM panels and New from Prompt dialog still nudge the user later if
    /// they skipped without pasting one.
    pub(super) fn ui_onboarding(&mut self, ctx: &egui::Context) {
        if !self.show_onboarding {
            return;
        }
        // Defer the welcome flow while the privacy prompt is up so a brand-
        // new user sees them one at a time, not stacked on top of each other.
        if self.show_crash_consent {
            return;
        }

        let mut do_save = false;
        let mut do_skip = false;
        // Deliberately not `.open(&mut open)` — we want the user to make an
        // explicit choice (Get Started or Skip) so `onboarded` always flips
        // to true. An X close would leave the modal able to reappear next
        // launch, which is the wrong default for a first-run wizard.
        egui::Window::new("Welcome to MoGen Studio")
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "MoGen Studio compiles declarative .mog scenes into glTF 2.0 .glb \
                     assets, with a live 3D preview.",
                );
                ui.add_space(8.0);
                ui.label(
                    "Generate, Modify, Animate, and Textures need an LLM. MoGen \
                     supports Gemini, OpenAI, Anthropic, Ollama, and Claude Code — \
                     pick whichever you have a key for in Edit → Preferences. \
                     Gemini is a good starting point because it's also the backend \
                     used for texture image generation, so you can paste a Google \
                     AI Studio key below to get going quickly. The rest of the app \
                     — editor, viewer, build — works without any key.",
                );

                ui.add_space(12.0);
                ui.heading("1. Get a Gemini key (optional)");
                ui.horizontal(|ui| {
                    ui.label("Open");
                    ui.hyperlink_to("Google AI Studio", GEMINI_API_KEY_URL);
                    ui.label("and click \"Create API key\".");
                });

                ui.add_space(10.0);
                ui.heading("2. Paste it here");
                let key_id = egui::Id::new("onboard_api_key");
                super::text_menu::text_edit_with_menu(
                    ui,
                    key_id,
                    &mut self.onboarding_api_key_draft,
                    |ui, text| {
                        ui.add(
                            egui::TextEdit::singleline(text)
                                .password(true)
                                .hint_text("paste key (or skip to set it later)")
                                .desired_width(f32::INFINITY)
                                .id(key_id),
                        )
                    },
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Stored in your user config directory. You can change or \
                         clear it any time from Edit → Preferences…",
                    )
                    .weak(),
                );

                if std::env::var("GEMINI_API_KEY")
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false)
                {
                    ui.add_space(6.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(150, 180, 230),
                        "GEMINI_API_KEY is already set in your environment — leave \
                         this blank to use that, or paste a key to override it.",
                    );
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let has_draft = !self.onboarding_api_key_draft.trim().is_empty();
                    let button_label = if has_draft {
                        "Save key & get started"
                    } else {
                        "Get started"
                    };
                    if ui
                        .button(button_label)
                        .on_hover_text(
                            "Save the pasted key (if any) and close this welcome screen.",
                        )
                        .clicked()
                    {
                        do_save = true;
                    }
                    if ui
                        .button("Skip for now")
                        .on_hover_text(
                            "Close this welcome screen without saving a key. Add one \
                             later from Edit → Preferences…",
                        )
                        .clicked()
                    {
                        do_skip = true;
                    }
                });
            });

        if !do_save && !do_skip {
            return;
        }

        if do_save {
            let trimmed = self.onboarding_api_key_draft.trim().to_string();
            self.settings.gemini_api_key = trimmed.clone();
            // Keep the Preferences dialog's draft in sync so opening it next
            // doesn't show a stale (empty) field next to a saved key.
            self.options_api_key_draft = trimmed;
        }

        self.settings.onboarded = true;
        if let Err(e) = self.settings.save() {
            self.active_mut().status = format!("onboarding: save failed: {e}");
        } else if do_save && !self.settings.gemini_api_key.is_empty() {
            self.active_mut().status = "onboarding: API key saved".into();
        }

        self.onboarding_api_key_draft.clear();
        self.show_onboarding = false;
    }
}
