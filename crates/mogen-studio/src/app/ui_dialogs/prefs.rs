use eframe::egui;
use mogen_llm::google_oauth::{ProviderConfig, ANTIGRAVITY_CONFIG, GEMINI_CLI_CONFIG};
use mogen_llm::Provider;

use crate::app::pricing::{format_per_million, image_pricing, text_pricing};
use crate::app::MogenStudioApp;
use crate::settings::{
    preview_fast_model, preview_thinking_model, thinking_level_key, thinking_level_label,
    ImageProvider, ProviderSlot, DEFAULT_MAX_REPAIR_ITERS, DEFAULT_OAUTH_GEMINI_FAST_MODEL,
    DEFAULT_OAUTH_GEMINI_MODEL, IMAGE_PROVIDERS, PROVIDER_SLOTS, THINKING_LEVELS,
};
use crate::theme::{apply_theme, theme_label, Theme, THEMES};

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

/// Single grid row of the Pricing breakdown table. `tier_long = true` reads
/// the >200k tier rates; `false` reads the headline rates. Caller is
/// responsible for `ui.end_row()`.
fn price_grid_row(
    ui: &mut egui::Ui,
    label: &str,
    model: &str,
    price: crate::app::pricing::TextPricing,
    tier_long: bool,
) {
    let (in_p, out_p, cached_p) = if tier_long {
        (
            price.input_per_million_usd_long,
            price.output_per_million_usd_long,
            price.cached_input_per_million_usd_long,
        )
    } else {
        (
            price.input_per_million_usd,
            price.output_per_million_usd,
            price.cached_input_per_million_usd,
        )
    };
    let cell = |s: String| -> egui::RichText {
        if tier_long {
            egui::RichText::new(s).weak()
        } else {
            egui::RichText::new(s)
        }
    };
    ui.label(cell(label.to_owned()));
    ui.label(cell(model.to_owned()));
    ui.label(cell(format_per_million(in_p)));
    ui.label(cell(format_per_million(out_p)));
    ui.label(cell(format_per_million(cached_p)));
    ui.end_row();
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

    fn prefs_tab_llm(&mut self, ui: &mut egui::Ui) {
        use crate::app::style;

        // ── Section 1: LLM provider ──
        style::framed_section(
            ui,
            "LLM provider",
            Some(
                "Backend used for Generate / Modify / Animate / Ask / Prompt \
                 Enhance. Texture image generation has its own provider \
                 picker below.",
            ),
            |ui| {
                let current_slot = self.settings.provider_slot();
                egui::ComboBox::from_id_salt("opts_provider")
                    .selected_text(current_slot.label())
                    .show_ui(ui, |ui| {
                        for slot in PROVIDER_SLOTS {
                            let selected = slot == current_slot;
                            if ui
                                .selectable_label(selected, slot.label())
                                .clicked()
                                && !selected
                            {
                                self.settings.set_provider_slot(slot);
                            }
                        }
                    });
            },
        );

        // ── Section 2: Image generation provider ──
        style::framed_section(
            ui,
            "Image generation provider",
            Some(
                "Which surface texture image generation hits.\n\n\
                 Auto: prefer Antigravity OAuth, fall back to Gemini API key.\n\
                 Antigravity OAuth: skip the API-key fallback.\n\
                 Gemini API key: bypass OAuth entirely.\n\
                 Z.ai (glm-image): swap the entire backend.\n\n\
                 Useful when one surface is rate-limited or quota-exhausted.",
            ),
            |ui| {
                let current_image = self.settings.image_provider();
                egui::ComboBox::from_id_salt("opts_image_provider")
                    .selected_text(current_image.label())
                    .show_ui(ui, |ui| {
                        for p in IMAGE_PROVIDERS {
                            let selected = p == current_image;
                            if ui
                                .selectable_label(selected, p.label())
                                .clicked()
                                && !selected
                            {
                                self.settings.set_image_provider(p);
                            }
                        }
                    });

                // Live indicator: which credential will Auto actually use
                // right now? The precedence chain is buried in the tooltip;
                // surface the current resolution as a quiet info row.
                if current_image == ImageProvider::Auto {
                    use crate::app::util::Credential;
                    let label = match self.resolve_gemini_credential() {
                        Some(Credential::AntigravityOAuth(_)) => {
                            "Auto → Antigravity OAuth (signed in)"
                        }
                        Some(Credential::ApiKey(_)) => {
                            "Auto → Gemini API key (saved or $GEMINI_API_KEY)"
                        }
                        Some(Credential::GeminiOAuth(_)) => {
                            "Auto → gemini-cli OAuth (last-resort fallback — \
                             will surface a 'wrong client' error if used for \
                             textures)"
                        }
                        Some(Credential::Zai(_)) => {
                            "Auto → Z.ai (unusual settings combination)"
                        }
                        None => {
                            "Auto → no credential resolved — texture \
                             generation will fail. Sign in to Antigravity or \
                             paste a Gemini API key."
                        }
                    };
                    style::info_row(ui, label);
                }

                if current_image == ImageProvider::ZAI {
                    ui.add_space(8.0);
                    ui.label("Z.ai API key").on_hover_text(
                        "Bearer key for `api.z.ai/api/paas/v4/images/generations`. \
                         Falls back to the ZAI_API_KEY environment variable when \
                         this field is blank.",
                    );
                    let zai_id = egui::Id::new("opts_zai_api_key");
                    crate::app::text_menu::text_edit_with_menu(
                        ui,
                        zai_id,
                        &mut self.settings.zai_api_key,
                        |ui, text| {
                            ui.add(
                                egui::TextEdit::singleline(text)
                                    .password(true)
                                    .hint_text(style::placeholder(
                                        "paste Z.ai key (leave blank to clear)",
                                    ))
                                    .desired_width(f32::INFINITY)
                                    .id(zai_id),
                            )
                        },
                    );
                    if std::env::var("ZAI_API_KEY")
                        .map(|v| !v.trim().is_empty())
                        .unwrap_or(false)
                    {
                        ui.add_space(4.0);
                        style::info_row(
                            ui,
                            "ZAI_API_KEY is set in your environment; the saved \
                             key above takes precedence when non-empty.",
                        );
                    }
                }
            },
        );

        // ── Section 3: Auth (provider-conditional) ──
        let active_slot = self.settings.provider_slot();
        let active_provider = active_slot.to_provider();
        if matches!(active_provider, Provider::ClaudeCode) {
            style::framed_section(
                ui,
                "Claude Code binary",
                Some(
                    "Auth is handled by your local `claude` install — \
                     run `claude` in a terminal and use the `/login` \
                     slash command if you haven't yet. Used by Generate \
                     / Modify / Animate / Ask.\n\n\
                     Image generation (Textures) always uses Gemini; \
                     set a Gemini API key on the Gemini slot above.",
                ),
                |ui| {
                    ui.label("Path (optional)").on_hover_text(
                        "Absolute path to the `claude` binary. Leave \
                         blank to resolve `claude` from PATH.",
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings.claude_code_path)
                            .hint_text(style::placeholder("claude"))
                            .desired_width(f32::INFINITY),
                    );
                },
            );
        } else if active_slot.is_gemini_oauth() {
            // OAuth-only Gemini slot: dedicated layout with sign-in flow.
            style::framed_section(ui, "Gemini OAuth", None, |ui| {
                self.prefs_gemini_oauth_section(ui);
            });
        } else {
            let (heading, hint) = match active_provider {
                Provider::Gemini => (
                    "Gemini API key",
                    "Used by Generate / Modify / Animate / Textures. \
                     Stored in your user config directory and persists \
                     between sessions. The $GEMINI_API_KEY environment \
                     variable is used when this field is blank.",
                ),
                Provider::OpenAI => (
                    "OpenAI API key",
                    "Used by Generate / Modify / Animate / Ask. Stored \
                     in your user config directory. The $OPENAI_API_KEY \
                     environment variable is used when this field is blank.",
                ),
                Provider::Anthropic => (
                    "Anthropic API key",
                    "Used by Generate / Modify / Animate / Ask. Stored \
                     in your user config directory. The $ANTHROPIC_API_KEY \
                     environment variable is used when this field is blank.",
                ),
                Provider::Ollama => (
                    "Ollama API key (optional)",
                    "Optional bearer token for an Ollama endpoint behind \
                     an authenticating proxy. Leave blank for a local install.",
                ),
                Provider::Fireworks => (
                    "Fireworks AI Firepass API key",
                    "Used by Generate / Modify / Animate / Ask. Default \
                     model is the Fire Pass `kimi-k2p6` router (zero \
                     per-token cost on Kimi K2 for personal agentic-coding \
                     use). Stored in your user config directory.",
                ),
                Provider::Zai => (
                    "Z.ai API key",
                    "Used by Generate / Modify / Animate / Ask. Default \
                     model is `glm-5.1` (Z.ai's GLM family via the \
                     OpenAI-compatible chat API). The same key drives \
                     the `glm-image` texture path when you set Image \
                     provider → Z.ai.",
                ),
                Provider::OpenAiCompat => (
                    "OpenAI-compatible API key (optional)",
                    "Optional bearer token for a local OpenAI-compatible \
                     server (LM Studio, llama.cpp, etc.). Leave blank for \
                     keyless local servers. Text generation only — image \
                     generation (Textures) always uses a cloud provider.",
                ),
                Provider::ClaudeCode => unreachable!(),
            };
            style::framed_section(ui, heading, Some(hint), |ui| {
                let key_id = egui::Id::new(("opts_api_key", active_provider.key()));
                let key_buf: &mut String = match active_provider {
                    Provider::Gemini => &mut self.options_api_key_draft,
                    Provider::OpenAI => &mut self.settings.openai_api_key,
                    Provider::Anthropic => &mut self.settings.anthropic_api_key,
                    Provider::Ollama => &mut self.settings.ollama_api_key,
                    Provider::Fireworks => &mut self.settings.fireworks_api_key,
                    Provider::Zai => &mut self.settings.zai_api_key,
                    Provider::OpenAiCompat => &mut self.settings.openai_compat_api_key,
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
                                .hint_text(style::placeholder(
                                    "paste key (leave blank to clear)",
                                ))
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
                            .hint_text(style::placeholder("http://localhost:11434"))
                            .desired_width(f32::INFINITY),
                    );
                }

                if matches!(active_provider, Provider::OpenAiCompat) {
                    ui.add_space(6.0);
                    ui.label("Base URL").on_hover_text(
                        "Base URL of your local OpenAI-compatible server. \
                         Requests go to {base}/chat/completions. Examples: \
                         http://localhost:1234/v1 (LM Studio), \
                         http://localhost:8080/v1 (llama.cpp).",
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings.openai_compat_base_url)
                            .hint_text(style::placeholder("http://localhost:1234/v1"))
                            .desired_width(f32::INFINITY),
                    );
                    if self.settings.openai_compat_base_url.trim().is_empty() {
                        ui.add_space(4.0);
                        style::info_row(
                            ui,
                            "Set a base URL — without one, requests fall back to \
                             the public OpenAI host and will fail.",
                        );
                    }
                }

                let env_var = active_provider.env_var();
                if !env_var.is_empty()
                    && std::env::var(env_var)
                        .map(|v| !v.trim().is_empty())
                        .unwrap_or(false)
                {
                    ui.add_space(4.0);
                    style::info_row(
                        ui,
                        &format!(
                            "{env_var} is set in your environment; the saved \
                             key above takes precedence when non-empty.",
                        ),
                    );
                }

                if matches!(active_provider, Provider::Zai) {
                    // GLM Coding Plan endpoint toggle (default-on).
                    ui.add_space(8.0);
                    let mut on = self.settings.zai_use_coding_plan();
                    let resp = ui.checkbox(&mut on, "Use GLM Coding Plan endpoint");
                    resp.on_hover_text(
                        "Routes Z.ai chat calls through `/api/coding/paas/v4` (the \
                         dedicated endpoint Z.ai documents for tools like Claude \
                         Code, Cline, Crush, MoGen Studio) instead of the general \
                         `/api/paas/v4` surface. Default on. Disable if you don't \
                         have a GLM Coding Plan subscription on this key.",
                    );
                    if on != self.settings.zai_use_coding_plan() {
                        self.settings.set_zai_use_coding_plan(on);
                    }
                }
            });
        }

        // ── Section 4: Models (provider-aware) ──
        if self
            .settings
            .thinking_model_field_mut(active_provider)
            .is_some()
        {
            let models_hint = format!(
                "Thinking model runs the heavy DSL paths (generate / modify / \
                 animate). Fast model runs cheap rewrites like the Prompt \
                 Enhancer. Showing presets for {}.",
                active_slot.label(),
            );
            style::framed_section(ui, "Models", Some(&models_hint), |ui| {
                // Preview / bleeding-edge toggle. Off by default; OAuth
                // slot pins to preview tags so the toggle is hidden there.
                if !active_slot.is_gemini_oauth()
                    && preview_thinking_model(active_provider).is_some()
                {
                    let mut on = self.settings.use_preview_models;
                    if ui
                        .checkbox(&mut on, "Use preview / bleeding-edge models")
                        .on_hover_text(
                            "When enabled (and no explicit override is set), \
                             default to the latest Gemini preview Pro / Flash \
                             IDs instead of the stable `*-latest` aliases. \
                             Preview models can be deprecated without notice.",
                        )
                        .changed()
                    {
                        self.settings.use_preview_models = on;
                    }
                    ui.add_space(6.0);
                }

                let (thinking_default, fast_default) = if active_slot.is_gemini_oauth() {
                    (DEFAULT_OAUTH_GEMINI_MODEL, DEFAULT_OAUTH_GEMINI_FAST_MODEL)
                } else if self.settings.use_preview_models {
                    (
                        preview_thinking_model(active_provider)
                            .unwrap_or_else(|| active_provider.default_model()),
                        preview_fast_model(active_provider)
                            .unwrap_or_else(|| active_provider.default_fast_model()),
                    )
                } else {
                    (active_provider.default_model(), active_provider.default_fast_model())
                };
                let presets = model_presets(active_slot);

                // --- thinking model: text field + presets dropdown in one row ---
                ui.label("Thinking model").on_hover_text(
                    "Used for generate / modify / animate and their repair loops. \
                     Pick a preset from the menu or type a custom model id.",
                );
                let mut thinking_draft = self
                    .settings
                    .thinking_model_field(active_provider)
                    .to_string();
                ui.horizontal(|ui| {
                    let preset_w = 96.0;
                    let avail = ui.available_width().max(preset_w + 60.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut thinking_draft)
                            .hint_text(style::placeholder(thinking_default))
                            .desired_width(avail - preset_w - 6.0),
                    );
                    egui::ComboBox::from_id_salt((
                        "opts_model_thinking_presets",
                        active_provider.key(),
                    ))
                    .selected_text("Presets ▾")
                    .width(preset_w)
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
                });
                if let Some(buf) = self.settings.thinking_model_field_mut(active_provider) {
                    *buf = thinking_draft;
                }

                ui.add_space(6.0);

                // --- fast model: same row pattern ---
                ui.label("Fast model").on_hover_text(
                    "Used for low-stakes text rewrites like the Prompt Enhancer.",
                );
                let mut fast_draft = self
                    .settings
                    .fast_model_field(active_provider)
                    .to_string();
                ui.horizontal(|ui| {
                    let preset_w = 96.0;
                    let avail = ui.available_width().max(preset_w + 60.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut fast_draft)
                            .hint_text(style::placeholder(fast_default))
                            .desired_width(avail - preset_w - 6.0),
                    );
                    egui::ComboBox::from_id_salt((
                        "opts_model_fast_presets",
                        active_provider.key(),
                    ))
                    .selected_text("Presets ▾")
                    .width(preset_w)
                    .show_ui(ui, |ui| {
                        for m in presets {
                            if ui.selectable_label(fast_draft == *m, *m).clicked() {
                                fast_draft = (*m).to_string();
                            }
                        }
                    });
                });
                if let Some(buf) = self.settings.fast_model_field_mut(active_provider) {
                    *buf = fast_draft;
                }

                // --- pricing block ---
                // Resolve currently-active model ids (override > preview >
                // default) and read their rates straight from the same
                // table the session meter uses.
                let thinking_model = self.settings.provider_model();
                let fast_model = self.settings.provider_fast_model();
                let thinking_price = text_pricing(&thinking_model);
                let fast_price = text_pricing(&fast_model);
                let has_pricing = thinking_price.input_per_million_usd > 0.0
                    || fast_price.input_per_million_usd > 0.0;

                if has_pricing {
                    // Always-visible typical cost so users see the headline
                    // figure without expanding the breakdown.
                    let typical_in = 5_000.0_f64;
                    let typical_out = 3_000.0_f64;
                    let typical_cost = typical_in
                        * thinking_price.input_per_million_usd
                        / 1_000_000.0
                        + typical_out
                            * thinking_price.output_per_million_usd
                            / 1_000_000.0;
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Typical generate (~5k in / ~3k out): ~${:.3}",
                            typical_cost,
                        ))
                        .weak(),
                    );

                    let img_model = match self.settings.image_provider() {
                        ImageProvider::Auto
                        | ImageProvider::ApiKey
                        | ImageProvider::Antigravity => "gemini-2.5-flash-image",
                        ImageProvider::ZAI => "",
                    };
                    let img_price = image_pricing(img_model);

                    egui::CollapsingHeader::new(egui::RichText::new("Pricing breakdown").weak())
                        .id_salt("opts_pricing_details")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "List rates from ai.google.dev/gemini-api/docs/pricing. \
                                     The session meter shows actual cost based on token usage.",
                                )
                                .weak(),
                            );
                            ui.add_space(4.0);
                            egui::Grid::new("opts_pricing_grid")
                                .num_columns(5)
                                .spacing([12.0, 4.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new("Tier").strong());
                                    ui.label(egui::RichText::new("Model").strong());
                                    ui.label(egui::RichText::new("In").strong());
                                    ui.label(egui::RichText::new("Out").strong());
                                    ui.label(egui::RichText::new("Cached").strong());
                                    ui.end_row();

                                    price_grid_row(
                                        ui,
                                        "Thinking",
                                        &thinking_model,
                                        thinking_price,
                                        false,
                                    );
                                    if thinking_price.is_tiered() {
                                        price_grid_row(
                                            ui,
                                            "  >200k",
                                            &thinking_model,
                                            thinking_price,
                                            true,
                                        );
                                    }
                                    if fast_price.input_per_million_usd > 0.0 {
                                        price_grid_row(
                                            ui,
                                            "Fast",
                                            &fast_model,
                                            fast_price,
                                            false,
                                        );
                                        if fast_price.is_tiered() {
                                            price_grid_row(
                                                ui,
                                                "  >200k",
                                                &fast_model,
                                                fast_price,
                                                true,
                                            );
                                        }
                                    }
                                    if img_price.per_image_usd > 0.0 {
                                        ui.label("Image");
                                        ui.label(img_model);
                                        ui.label(format!(
                                            "${:.3}/image",
                                            img_price.per_image_usd,
                                        ));
                                        ui.label("");
                                        ui.label("");
                                        ui.end_row();
                                    }
                                });
                        });
                }
            });
        }

        // ── Section 5: Thinking budget ──
        style::framed_section(
            ui,
            "Thinking budget",
            Some(
                "Cap on the model's hidden reasoning tokens per call. \
                 Higher = better DSL on hard prompts but slower and more \
                 expensive. Ignored by providers / models that don't \
                 expose a budget.",
            ),
            |ui| {
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
            },
        );

        // ── Section 6: Advanced (sampling, repair, seed) ──
        ui.add_space(6.0);
        egui::CollapsingHeader::new(
            egui::RichText::new("Advanced (sampling, repair, seed)").strong(),
        )
        .id_salt("opts_advanced")
        .default_open(false)
        .show(ui, |ui| {
            // --- temperature ---
            ui.label("Temperature").on_hover_text(
                "Sampling temperature (0 = deterministic, 2 = chaotic). \
                 The DSL generator is happier at low values; raise it only \
                 if you want stylistic variation.",
            );
            let mut temp = self
                .settings
                .gemini_temperature
                .unwrap_or(mogen_llm::gemini::DEFAULT_TEMPERATURE);
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::Slider::new(&mut temp, 0.0..=2.0)
                        .max_decimals(2)
                        .text("°"),
                )
                .on_hover_text(
                    "0 = deterministic, 2 = chaotic. Default 0.3.",
                );
                if resp.changed() {
                    self.settings.gemini_temperature = Some(temp);
                }
                if ui
                    .small_button("Reset")
                    .on_hover_text("Restore the library default (0.3)")
                    .clicked()
                {
                    self.settings.gemini_temperature = None;
                }
            });

            ui.add_space(6.0);

            // --- max repair iters ---
            ui.label("Max repair iterations").on_hover_text(
                "How many times to re-call the active provider when the \
                 generated DSL fails validation. Higher = more API cost \
                 but fewer invalid outputs. Range 0–5.",
            );
            let mut iters = self
                .settings
                .max_repair_iters
                .unwrap_or(DEFAULT_MAX_REPAIR_ITERS);
            if ui
                .add(
                    egui::DragValue::new(&mut iters)
                        .range(0..=5)
                        .speed(0.1),
                )
                .on_hover_text("Range 0–5.")
                .changed()
            {
                self.settings.max_repair_iters = Some(iters);
            }

            ui.add_space(6.0);

            // --- seed override ---
            ui.label("Seed").on_hover_text(
                "Deterministic seed stamped on every generated .mog header. \
                 Set to reproduce a prior run; clear for a fresh seed each \
                 call. Must be a non-negative integer.",
            );
            ui.horizontal(|ui| {
                let mut seed_str = self
                    .settings
                    .seed_override
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut seed_str)
                        .hint_text(style::placeholder("(random each call)"))
                        .desired_width(160.0),
                );
                let trimmed = seed_str.trim().to_string();
                let parse_result =
                    if trimmed.is_empty() { Ok(None) } else {
                        trimmed.parse::<u64>().map(Some)
                    };
                if resp.changed() {
                    self.settings.seed_override = match &parse_result {
                        Ok(v) => *v,
                        Err(_) => self.settings.seed_override,
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
                // Inline validation feedback so users know why a typo
                // didn't take effect — matches the new_prompt dialog.
                if !trimmed.is_empty() && parse_result.is_err() {
                    ui.label(
                        egui::RichText::new("not a valid u64")
                            .color(ui.visuals().warn_fg_color),
                    );
                }
            });
        });
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
