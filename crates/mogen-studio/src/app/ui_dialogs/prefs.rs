use eframe::egui;
use mogen_llm::google_oauth::{ProviderConfig, ANTIGRAVITY_CONFIG, GEMINI_CLI_CONFIG};
use mogen_llm::Provider;

use crate::app::MogenStudioApp;
use crate::settings::ProviderSlot;
use crate::theme::{apply_theme, theme_label, Theme, THEMES};

mod llm;

/// Tab pages inside the Preferences window. Grouped by what the user is
/// trying to change: anything LLM-shaped (provider, key, models, sampling)
/// lives in one pane, look-and-feel in another, telemetry in a third.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Default)]
pub enum PrefsTab {
    #[default]
    Llm,
    Appearance,
    Privacy,
}

impl PrefsTab {
    fn label(self) -> &'static str {
        match self {
            PrefsTab::Llm => "LLM",
            PrefsTab::Appearance => "Appearance",
            PrefsTab::Privacy => "Privacy",
        }
    }
}

const PREFS_TABS: [PrefsTab; 3] = [PrefsTab::Llm, PrefsTab::Appearance, PrefsTab::Privacy];

/// Models surfaced in the Preferences dropdown for each slot. Free-form
/// text still wins if a user types one in, but these cover the tiers almost
/// every user will want. The Gemini slots split: API-key uses the public
/// `*-latest` aliases, OAuth pins to concrete preview tags because
/// `cloudcode-pa.googleapis.com/v1internal` 404s on the latest aliases.
pub(super) fn model_presets(slot: ProviderSlot) -> &'static [&'static str] {
    match slot {
        ProviderSlot::GeminiApiKey => &[
            "gemini-pro-latest",
            "gemini-flash-latest",
            "gemini-3.5-flash",
            "gemini-3.1-flash-lite",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
        ],
        ProviderSlot::GeminiOAuth => &[
            "gemini-3.1-pro-preview",
            "gemini-3.5-flash",
            "gemini-3-flash-preview",
            "gemini-3.1-flash-lite",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
        ],
        ProviderSlot::OpenAI => &[
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "gpt-5",
            "gpt-5-mini",
            "gpt-4.1",
            "o3",
            "o3-pro",
        ],
        ProviderSlot::Anthropic => &[
            "claude-opus-4-7",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
        ],
        ProviderSlot::Ollama => &[
            "llama3.3",
            "llama3.2",
            "qwen3",
            "qwen2.5",
            "deepseek-r1",
            "mistral",
            "phi4",
            "gemma3",
        ],
        ProviderSlot::ClaudeCode => &[
            "sonnet",
            "haiku",
            "opus",
        ],
        ProviderSlot::Fireworks => &[
            "accounts/fireworks/routers/kimi-k2p6",
            "accounts/fireworks/routers/kimi-k2p6-turbo",
            "accounts/fireworks/routers/kimi-k2p5-turbo",
        ],
        ProviderSlot::Zai => &[
            "glm-5.1",
            "glm-5v-turbo",
            "glm-4.6",
            "glm-4.5-air",
            "glm-4.5-flash",
        ],
        // Model ids are server-defined for a local OpenAI-compatible host —
        // there's no canonical preset list. `local-model` is the library
        // default that many single-model servers (llama.cpp, some LM Studio
        // setups) accept regardless of the loaded weights.
        ProviderSlot::OpenAiCompat => &["local-model"],
    }
}

impl MogenStudioApp {
    pub(in crate::app) fn ui_options(&mut self, ctx: &egui::Context) {
        if !self.show_options {
            return;
        }
        let mut open = true;
        let mut close_after = false;

        // Backdrop scrim. Sits in the Background order so it covers the
        // side panels (which would otherwise bleed into a non-resizable
        // window's edges) but stays under the modal Window itself.
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("prefs_scrim"))
            .order(egui::Order::Background)
            .fixed_pos(egui::pos2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    egui::Color32::from_black_alpha(96),
                );
            });

        // Adapt the dialog footprint to the host window. On a small
        // window we want a minimum usable size; on a big window we cap
        // at a comfortable reading width and let the body scroll for
        // height overflow.
        let avail_w = screen.width();
        let avail_h = screen.height();
        let max_w = (avail_w - 32.0).clamp(360.0, 720.0);
        let max_h = (avail_h - 64.0).max(320.0);
        let default_w = max_w.min(560.0);

        let active_tab_color =
            crate::app::style::accent_primary(&ctx.style().visuals);

        egui::Window::new("Options")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(default_w)
            .min_width(360.0)
            .max_width(max_w)
            .max_height(max_h)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                // Tab strip stays outside the scroll area so it always
                // sticks to the top of the dialog.
                ui.horizontal(|ui| {
                    for tab in PREFS_TABS {
                        let selected = self.prefs_active_tab == tab;
                        let label_text = if selected {
                            egui::RichText::new(tab.label()).strong()
                        } else {
                            egui::RichText::new(tab.label())
                        };
                        let resp = ui.selectable_label(selected, label_text);
                        if resp.clicked() {
                            self.prefs_active_tab = tab;
                        }
                        if selected {
                            // Underline the active tab with the accent so
                            // the selection is legible across themes.
                            let r = resp.rect;
                            ui.painter().line_segment(
                                [
                                    egui::pos2(r.left() + 4.0, r.bottom() + 1.0),
                                    egui::pos2(r.right() - 4.0, r.bottom() + 1.0),
                                ],
                                egui::Stroke::new(2.0, active_tab_color),
                            );
                        }
                    }
                });
                ui.separator();
                ui.add_space(4.0);

                // Body scrolls; footer below stays pinned. Without the
                // explicit height, the ScrollArea would expand to fit
                // content and re-introduce the off-screen-clip bug at
                // small window sizes.
                let body_height = (max_h - 130.0).max(160.0);
                egui::ScrollArea::vertical()
                    .id_salt(("prefs_body", self.prefs_active_tab))
                    .auto_shrink([false, false])
                    .max_height(body_height)
                    .show(ui, |ui| {
                        match self.prefs_active_tab {
                            PrefsTab::Llm => self.prefs_tab_llm(ui),
                            PrefsTab::Appearance => self.prefs_tab_appearance(ui, ctx),
                            PrefsTab::Privacy => self.prefs_tab_privacy(ui),
                        }
                    });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                // Right-aligned cluster, primary Save on the right.
                ui.horizontal(|ui| {
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let save = ui.add(
                                crate::app::style::primary_button(ui, "Save"),
                            );
                            if save.clicked() {
                        // The Gemini key uses an editor-buffered draft so the
                        // user can clear/re-paste without instantly mutating
                        // settings; commit it on Save. Other providers' keys
                        // are written directly into settings as the user
                        // types because they don't share that legacy draft.
                        self.settings.gemini_api_key =
                            self.options_api_key_draft.trim().to_string();
                        // Trim whitespace from per-provider keys / URL on
                        // save so users can paste with surrounding spaces.
                        self.settings.openai_api_key =
                            self.settings.openai_api_key.trim().to_string();
                        self.settings.anthropic_api_key =
                            self.settings.anthropic_api_key.trim().to_string();
                        self.settings.ollama_api_key =
                            self.settings.ollama_api_key.trim().to_string();
                        self.settings.ollama_base_url =
                            self.settings.ollama_base_url.trim().to_string();
                        self.settings.claude_code_path =
                            self.settings.claude_code_path.trim().to_string();
                        self.settings.zai_api_key =
                            self.settings.zai_api_key.trim().to_string();
                        self.settings.fireworks_api_key =
                            self.settings.fireworks_api_key.trim().to_string();
                        self.settings.openai_compat_api_key =
                            self.settings.openai_compat_api_key.trim().to_string();
                        self.settings.openai_compat_base_url =
                            self.settings.openai_compat_base_url.trim().to_string();
                        match self.settings.save() {
                            Ok(()) => {
                                let active = self.settings.provider();
                                let msg = match active {
                                    Provider::Gemini if self.settings.gemini_api_key.is_empty() => {
                                        "options: cleared saved Gemini API key".to_string()
                                    }
                                    _ => "options: settings saved".to_string(),
                                };
                                self.active_mut().status = msg;
                                close_after = true;
                            }
                            Err(e) => {
                                self.active_mut().status = format!("options: save failed: {e}");
                            }
                        }
                            }
                            if ui.button("Cancel").clicked() {
                                close_after = true;
                            }
                        },
                    );
                });
            });
        if !open || close_after {
            self.show_options = false;
        }
    }

    /// Gemini OAuth section. Shown when the active slot is `GeminiOAuth`.
    /// Renders the two OAuth flows (gemini-cli for text,
    /// `cloudcode-pa.googleapis.com/v1internal`; Antigravity for images,
    /// nano-banana / Gemini 3 Pro Image) side-by-side in two columns to
    /// keep the modal short. Each surface needs its own OAuth client —
    /// gemini-cli is rejected by the image API with 403, and Antigravity
    /// can't be reused for text — so the two stay separate. Tokens live
    /// in `google_auth.json` and `antigravity_auth.json` respectively, so
    /// signing in here also authenticates the CLI (and vice versa).
    fn prefs_gemini_oauth_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Google sign-in");
        ui.label(
            "OAuth slot uses the gemini-cli client for text and Antigravity \
             for images — each surface only accepts its own client. Switch \
             to \"Gemini (API key)\" above to skip OAuth.",
        );
        ui.add_space(8.0);
        ui.columns(2, |cols| {
            cols[0].label(egui::RichText::new("Text (gemini-cli)").strong());
            cols[0].label(
                "Routes generate / modify / animate through the paid Pro \
                 plan — gemini-3-pro-preview and 3.1-pro-preview without an \
                 API key.",
            );
            cols[0].add_space(6.0);
            self.render_oauth_provider_block(
                &mut cols[0],
                &GEMINI_CLI_CONFIG,
                "Sign in with Google",
            );

            cols[1].label(egui::RichText::new("Images (Antigravity)").strong());
            cols[1].label(
                "Required for `Generate textures` over OAuth (nano-banana / \
                 Gemini 3 Pro Image). An API key also works as a fallback.",
            );
            cols[1].add_space(6.0);
            self.render_oauth_provider_block(
                &mut cols[1],
                &ANTIGRAVITY_CONFIG,
                "Sign in with Antigravity",
            );
        });
    }

    /// Shared body for both OAuth provider sections — status line, in-flight
    /// status message, Login/Sign out buttons. Splitting the heading and
    /// description out of the helper keeps each section's wording specific.
    fn render_oauth_provider_block(
        &mut self,
        ui: &mut egui::Ui,
        config: &'static ProviderConfig,
        login_button_label: &str,
    ) {
        let stored = self.oauth_stored_status_for(config);
        if let Some(line) = &stored {
            ui.label(line);
        } else {
            ui.label("Not signed in.");
        }

        if let Some(msg) = self.oauth_status_message.clone() {
            ui.add_space(4.0);
            ui.colored_label(egui::Color32::from_rgb(150, 180, 230), msg);
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let any_in_flight = self.oauth_login_in_flight();
            let this_in_flight = self.oauth_login_in_flight_for(config);
            let login_label = if this_in_flight {
                "Signing in… (check browser)".to_string()
            } else if stored.is_some() {
                format!("Re-authenticate ({login_button_label})")
            } else {
                login_button_label.to_string()
            };
            let login_btn =
                ui.add_enabled(!any_in_flight, egui::Button::new(login_label));
            if login_btn.clicked() {
                let ctx = ui.ctx().clone();
                self.start_oauth_login_for(ctx, config);
            }
            if stored.is_some() {
                if ui
                    .add_enabled(!any_in_flight, egui::Button::new("Sign out"))
                    .on_hover_text(
                        "Delete the local OAuth token for this provider.",
                    )
                    .clicked()
                {
                    self.start_oauth_logout_for(config);
                }
            }
        });
    }

    fn prefs_tab_appearance(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("Theme");
        ui.label("Colour scheme for the editor and panels. Applies immediately.");
        ui.add_space(6.0);
        let current_theme = self.settings.theme();
        let mut new_theme: Option<Theme> = None;
        egui::ComboBox::from_id_salt("opts_theme")
            .selected_text(theme_label(current_theme))
            .show_ui(ui, |ui| {
                for t in THEMES {
                    let selected = t == current_theme;
                    if ui
                        .selectable_label(selected, theme_label(t))
                        .clicked()
                        && !selected
                    {
                        new_theme = Some(t);
                    }
                }
            });
        if let Some(t) = new_theme {
            self.settings.set_theme(t);
            apply_theme(ctx, t);
        }

        ui.add_space(12.0);
        ui.heading("Viewport");
        ui.label(
            "Maximum FPS for continuous redraws (animation playback, cinema \
             pan, gizmo drag). Lower values save battery and reduce thermals; \
             \"Unlimited\" defers to the display's vsync. Input-driven paints \
             are unaffected.",
        );
        ui.add_space(6.0);
        let current_fps = self.settings.max_fps();
        let current_label = match current_fps {
            None => "Unlimited".to_string(),
            Some(n) => format!("{n} FPS"),
        };
        let presets: [(Option<u32>, &str); 5] = [
            (None, "Unlimited"),
            (Some(30), "30 FPS"),
            (Some(60), "60 FPS"),
            (Some(120), "120 FPS"),
            (Some(144), "144 FPS"),
        ];
        let mut new_fps: Option<Option<u32>> = None;
        egui::ComboBox::from_id_salt("opts_max_fps")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                for (val, label) in presets {
                    let selected = val == current_fps;
                    if ui
                        .selectable_label(selected, label)
                        .clicked()
                        && !selected
                    {
                        new_fps = Some(val);
                    }
                }
            });
        if let Some(val) = new_fps {
            self.settings.set_max_fps(val);
            self.viewer.set_max_fps(val);
        }
    }

    fn prefs_tab_privacy(&mut self, ui: &mut egui::Ui) {
        ui.heading("Privacy");
        // Tri-state in storage (`None` = undecided) collapses to a
        // simple bool here: opening Preferences past first launch
        // implies the user has been asked, so any change away from
        // the default is an explicit decision worth persisting.
        let env_blocked = crate::crash::telemetry_blocked_by_env();
        let mut on = self.settings.crash_reports_enabled.unwrap_or(false);
        let resp = ui.add_enabled(
            !env_blocked,
            egui::Checkbox::new(&mut on, "Send anonymous crash reports"),
        );
        if resp.changed() {
            self.settings.crash_reports_enabled = Some(on);
        }
        let hover = if env_blocked {
            "Forced off by MOGEN_DISABLE_TELEMETRY / DO_NOT_TRACK in your \
             environment. Unset the variable to control this from here."
        } else {
            "Reports a stack trace, app version, and OS family when \
             MoGen Studio crashes. No source, .mog files, prompts, or \
             API keys. Takes effect on next launch."
        };
        resp.on_hover_text(hover);
    }
}
