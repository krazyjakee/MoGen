use eframe::egui;
use mogen_llm::Provider;

use crate::app::MogenStudioApp;
use crate::settings::{
    thinking_level_key, thinking_level_label, DEFAULT_MAX_REPAIR_ITERS, PROVIDERS,
    THINKING_LEVELS,
};
use crate::theme::{apply_theme, theme_label, Theme, THEMES};

/// Tab pages inside the Preferences window. Grouped by what the user is
/// trying to change: anything LLM-shaped (provider, key, models, sampling)
/// lives in one pane, look-and-feel in another, telemetry in a third.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
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

/// Models surfaced in the Preferences dropdown for each provider. Free-form
/// text still wins if a user types one in, but these cover the tiers almost
/// every user will want and give pricing expectations a named anchor.
fn model_presets(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Gemini => &[
            "gemini-pro-latest",
            "gemini-flash-latest",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
        ],
        Provider::OpenAI => &[
            "gpt-5",
            "gpt-5-mini",
            "gpt-5-nano",
            "gpt-4.1",
            "gpt-4.1-mini",
            "o4-mini",
            "o3",
        ],
        Provider::Anthropic => &[
            "claude-opus-4-7",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
        ],
        Provider::Ollama => &[
            "llama3.3",
            "llama3.2",
            "qwen3",
            "qwen2.5",
            "deepseek-r1",
            "mistral",
            "phi4",
            "gemma3",
        ],
        Provider::ClaudeCode => &[
            "sonnet",
            "haiku",
            "opus",
        ],
    }
}

impl MogenStudioApp {
    pub(in crate::app) fn ui_options(&mut self, ctx: &egui::Context) {
        if !self.show_options {
            return;
        }
        let mut open = true;
        let mut close_after = false;
        egui::Window::new("Options")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for tab in PREFS_TABS {
                        let selected = self.prefs_active_tab == tab;
                        if ui.selectable_label(selected, tab.label()).clicked() {
                            self.prefs_active_tab = tab;
                        }
                    }
                });
                ui.separator();
                ui.add_space(4.0);

                match self.prefs_active_tab {
                    PrefsTab::Llm => self.prefs_tab_llm(ui),
                    PrefsTab::Appearance => self.prefs_tab_appearance(ui, ctx),
                    PrefsTab::Privacy => self.prefs_tab_privacy(ui),
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
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
                });
            });
        if !open || close_after {
            self.show_options = false;
        }
    }

    fn prefs_tab_llm(&mut self, ui: &mut egui::Ui) {
        ui.heading("LLM provider");
                ui.label(
                    "Backend used for Generate / Modify / Animate / Ask / Prompt \
                     Enhance. Texture image generation is always Gemini regardless \
                     of this setting (no other backend has an image API).",
                );
                ui.add_space(6.0);
                let current_provider = self.settings.provider();
                egui::ComboBox::from_id_salt("opts_provider")
                    .selected_text(current_provider.label())
                    .show_ui(ui, |ui| {
                        for p in PROVIDERS {
                            let selected = p == current_provider;
                            if ui
                                .selectable_label(selected, p.label())
                                .clicked()
                                && !selected
                            {
                                self.settings.set_provider(p);
                            }
                        }
                    });

                ui.add_space(12.0);
                let active_provider = self.settings.provider();
                if matches!(active_provider, Provider::ClaudeCode) {
                    // Claude Code authenticates through the user's local
                    // `claude` CLI install — no key to paste here. We only
                    // expose a binary-path override for non-PATH installs.
                    ui.heading("Claude Code binary");
                    ui.label(
                        "Auth is handled by your local `claude` install \
                         (run `claude /login` if you haven't yet). Used by \
                         Generate / Modify / Animate / Ask. Image generation \
                         (Textures) still requires Gemini.",
                    );
                    ui.add_space(6.0);
                    ui.label("Path (optional)").on_hover_text(
                        "Absolute path to the `claude` binary. Leave blank to \
                         resolve `claude` from PATH.",
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings.claude_code_path)
                            .hint_text("claude")
                            .desired_width(f32::INFINITY),
                    );
                } else {
                    let key_heading = match active_provider {
                        Provider::Gemini => "Gemini API key",
                        Provider::OpenAI => "OpenAI API key",
                        Provider::Anthropic => "Anthropic API key",
                        Provider::Ollama => "Ollama API key (optional)",
                        Provider::ClaudeCode => unreachable!(),
                    };
                    ui.heading(key_heading);
                    ui.label(match active_provider {
                        Provider::Gemini => {
                            "Used by Generate / Modify / Animate / Textures. Stored in your \
                             user config directory and persists between sessions."
                        }
                        Provider::OpenAI => {
                            "Used by Generate / Modify / Animate / Ask. Stored in your \
                             user config directory and persists between sessions."
                        }
                        Provider::Anthropic => {
                            "Used by Generate / Modify / Animate / Ask. Stored in your \
                             user config directory and persists between sessions."
                        }
                        Provider::Ollama => {
                            "Optional bearer token for an Ollama endpoint behind an \
                             authenticating proxy. Leave blank for a local install."
                        }
                        Provider::ClaudeCode => unreachable!(),
                    });
                    ui.add_space(6.0);
                    // Show only the field for the active provider to reduce clutter.
                    // Switching providers above swaps which field is visible here.
                    let key_id = egui::Id::new(("opts_api_key", active_provider.key()));
                    let key_buf: &mut String = match active_provider {
                        Provider::Gemini => &mut self.options_api_key_draft,
                        Provider::OpenAI => &mut self.settings.openai_api_key,
                        Provider::Anthropic => &mut self.settings.anthropic_api_key,
                        Provider::Ollama => &mut self.settings.ollama_api_key,
                        Provider::ClaudeCode => unreachable!(),
                    };
                    crate::app::text_menu::text_edit_with_menu(
                        ui,
                        key_id,
                        key_buf,
                        |ui, text| {
                            ui.add(
                                egui::TextEdit::singleline(text)
                                    .password(true)
                                    .hint_text("paste key (leave blank to clear)")
                                    .desired_width(f32::INFINITY)
                                    .id(key_id),
                            )
                        },
                    );

                    if matches!(active_provider, Provider::Ollama) {
                        ui.add_space(6.0);
                        ui.label("Ollama base URL (optional)").on_hover_text(
                            "Override for self-hosted Ollama. Leave blank for the \
                             library default (http://localhost:11434).",
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut self.settings.ollama_base_url)
                                .hint_text("http://localhost:11434")
                                .desired_width(f32::INFINITY),
                        );
                    }

                    let env_var = active_provider.env_var();
                    if !env_var.is_empty()
                        && std::env::var(env_var)
                            .map(|v| !v.trim().is_empty())
                            .unwrap_or(false)
                    {
                        ui.add_space(4.0);
                        ui.colored_label(
                            egui::Color32::from_rgb(150, 180, 230),
                            format!(
                                "{env_var} is also set in your environment — \
                                 the saved key here takes precedence when non-empty.",
                            ),
                        );
                    }
                }

                // --- Models (provider-aware: presets and target fields swap
                // when the LLM provider above changes) ---
                if self
                    .settings
                    .thinking_model_field_mut(active_provider)
                    .is_some()
                {
                    ui.add_space(12.0);
                    ui.heading("Models");
                    ui.label(format!(
                        "Thinking model runs the heavy DSL paths (generate / modify / \
                         animate). Fast model runs cheap rewrites like the Prompt \
                         Enhancer. Showing presets for {}.",
                        active_provider.label(),
                    ));
                    ui.add_space(6.0);

                    let thinking_default = active_provider.default_model();
                    let fast_default = active_provider.default_fast_model();
                    let presets = model_presets(active_provider);

                    // --- thinking model picker ---
                    ui.label("Thinking model").on_hover_text(
                        "Used for generate / modify / animate and their repair loops. \
                         Pick a preset or type a custom model id.",
                    );
                    let mut thinking_draft = self
                        .settings
                        .thinking_model_field(active_provider)
                        .to_string();
                    let thinking_selected = if thinking_draft.is_empty() {
                        format!("(library default: {thinking_default})")
                    } else {
                        thinking_draft.clone()
                    };
                    egui::ComboBox::from_id_salt((
                        "opts_model_thinking",
                        active_provider.key(),
                    ))
                    .selected_text(thinking_selected)
                    .show_ui(ui, |ui| {
                        for m in presets {
                            if ui
                                .selectable_label(thinking_draft == *m, *m)
                                .clicked()
                            {
                                thinking_draft = (*m).to_string();
                            }
                        }
                    });
                    ui.add(
                        egui::TextEdit::singleline(&mut thinking_draft)
                            .hint_text(thinking_default)
                            .desired_width(f32::INFINITY),
                    );
                    if let Some(buf) = self.settings.thinking_model_field_mut(active_provider) {
                        *buf = thinking_draft;
                    }

                    ui.add_space(6.0);

                    // --- fast model picker ---
                    ui.label("Fast model").on_hover_text(
                        "Used for low-stakes text rewrites like the Prompt Enhancer.",
                    );
                    let mut fast_draft = self
                        .settings
                        .fast_model_field(active_provider)
                        .to_string();
                    let fast_selected = if fast_draft.is_empty() {
                        format!("(library default: {fast_default})")
                    } else {
                        fast_draft.clone()
                    };
                    egui::ComboBox::from_id_salt((
                        "opts_model_fast",
                        active_provider.key(),
                    ))
                    .selected_text(fast_selected)
                    .show_ui(ui, |ui| {
                        for m in presets {
                            if ui.selectable_label(fast_draft == *m, *m).clicked() {
                                fast_draft = (*m).to_string();
                            }
                        }
                    });
                    ui.add(
                        egui::TextEdit::singleline(&mut fast_draft)
                            .hint_text(fast_default)
                            .desired_width(f32::INFINITY),
                    );
                    if let Some(buf) = self.settings.fast_model_field_mut(active_provider) {
                        *buf = fast_draft;
                    }
                }

                ui.add_space(12.0);
                ui.heading("Thinking budget");
                ui.label(
                    "Cap on the model's hidden reasoning tokens per call (Gemini, OpenAI \
                     reasoning models). Higher = better DSL on hard prompts but slower and \
                     more expensive. Ignored by providers that don't expose a budget.",
                );
                ui.add_space(6.0);
                let current = self.settings.thinking_level();
                egui::ComboBox::from_id_salt("opts_thinking_level")
                    .selected_text(thinking_level_label(current))
                    .show_ui(ui, |ui| {
                        for level in THINKING_LEVELS {
                            let selected = level == current;
                            if ui
                                .selectable_label(selected, thinking_level_label(level))
                                .clicked()
                                && !selected
                            {
                                self.settings.thinking_level =
                                    thinking_level_key(level).to_string();
                            }
                        }
                    });

                ui.add_space(12.0);
                egui::CollapsingHeader::new("Advanced (sampling, repair, seed)")
                    .id_salt("opts_advanced")
                    .default_open(false)
                    .show(ui, |ui| {

                        // --- temperature ---
                        ui.label("Temperature")
                            .on_hover_text(
                                "Sampling temperature (0 = deterministic, 2 = chaotic). \
                                 The DSL generator is happier at low values; raise it only \
                                 if you want stylistic variation.",
                            );
                        let mut temp =
                            self.settings.gemini_temperature.unwrap_or(
                                mogen_llm::gemini::DEFAULT_TEMPERATURE,
                            );
                        if ui
                            .add(
                                egui::Slider::new(&mut temp, 0.0..=2.0)
                                    .fixed_decimals(2)
                                    .text("°"),
                            )
                            .changed()
                        {
                            self.settings.gemini_temperature = Some(temp);
                        }
                        if ui
                            .small_button("Reset")
                            .on_hover_text("Restore the library default (0.3)")
                            .clicked()
                        {
                            self.settings.gemini_temperature = None;
                        }

                        ui.add_space(6.0);

                        // --- max repair iters ---
                        ui.label("Max repair iterations")
                            .on_hover_text(
                                "How many times to re-call the active provider when the \
                                 generated DSL fails validation. Higher = more API cost but \
                                 fewer invalid outputs.",
                            );
                        let mut iters =
                            self.settings.max_repair_iters.unwrap_or(DEFAULT_MAX_REPAIR_ITERS);
                        if ui
                            .add(
                                egui::DragValue::new(&mut iters)
                                    .range(0..=5)
                                    .speed(0.1),
                            )
                            .changed()
                        {
                            self.settings.max_repair_iters = Some(iters);
                        }

                        ui.add_space(6.0);

                        // --- seed override ---
                        ui.label("Seed")
                            .on_hover_text(
                                "Deterministic seed stamped on every generated .mog header. \
                                 Set to reproduce a prior run; clear for a fresh seed each call.",
                            );
                        ui.horizontal(|ui| {
                            let mut seed_str = self
                                .settings
                                .seed_override
                                .map(|s| s.to_string())
                                .unwrap_or_default();
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut seed_str)
                                    .hint_text("(random each call)")
                                    .desired_width(160.0),
                            );
                            if resp.changed() {
                                let trimmed = seed_str.trim();
                                self.settings.seed_override = if trimmed.is_empty() {
                                    None
                                } else {
                                    trimmed.parse::<u64>().ok()
                                };
                            }
                            if ui
                                .small_button("🎲")
                                .on_hover_text("Pick a random seed now")
                                .clicked()
                            {
                                self.settings.seed_override =
                                    Some(crate::app::util::pick_default_seed());
                            }
                            if ui
                                .small_button("Clear")
                                .on_hover_text("Use a fresh random seed on every call")
                                .clicked()
                            {
                                self.settings.seed_override = None;
                            }
                        });
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
