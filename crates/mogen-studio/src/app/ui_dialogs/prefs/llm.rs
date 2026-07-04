//! The Preferences → LLM tab. Provider / image-provider / auth / models /
//! thinking-budget / advanced sections, plus the per-call pricing breakdown.
//! Split out of `prefs.rs` because it dwarfs every other tab.

use eframe::egui;
use mogen_llm::Provider;

use crate::app::pricing::{format_per_million, image_pricing, text_pricing};
use crate::app::MogenStudioApp;
use crate::settings::{
    preview_fast_model, preview_thinking_model, thinking_level_key, thinking_level_label,
    ImageProvider, DEFAULT_MAX_REPAIR_ITERS, DEFAULT_OAUTH_GEMINI_FAST_MODEL,
    DEFAULT_OAUTH_GEMINI_MODEL, IMAGE_PROVIDERS, PROVIDER_SLOTS, THINKING_LEVELS,
};

use super::model_presets;

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
    pub(super) fn prefs_tab_llm(&mut self, ui: &mut egui::Ui) {
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
                            if ui.selectable_label(selected, slot.label()).clicked() && !selected {
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
                            if ui.selectable_label(selected, p.label()).clicked() && !selected {
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
                        Some(Credential::Zai(_)) => "Auto → Z.ai (unusual settings combination)",
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
                Provider::Xiaomi => (
                    "Xiaomi MiMo API key",
                    "Used by Generate / Modify / Animate / Ask. Default \
                     model is `mimo-v2.5-pro` via Xiaomi's OpenAI-compatible \
                     chat API. `mimo-v2.5` is used automatically for image \
                     inputs.",
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
                    Provider::Xiaomi => &mut self.settings.xiaomi_api_key,
                    Provider::OpenAiCompat => &mut self.settings.openai_compat_api_key,
                    Provider::ClaudeCode => unreachable!(),
                };
                crate::app::text_menu::text_edit_with_menu(ui, key_id, key_buf, |ui, text| {
                    ui.add(
                        egui::TextEdit::singleline(text)
                            .password(true)
                            .hint_text(style::placeholder("paste key (leave blank to clear)"))
                            .desired_width(f32::INFINITY)
                            .id(key_id),
                    )
                });

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
                    (
                        active_provider.default_model(),
                        active_provider.default_fast_model(),
                    )
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
                            if ui.selectable_label(thinking_draft == *m, *m).clicked() {
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
                ui.label("Fast model")
                    .on_hover_text("Used for low-stakes text rewrites like the Prompt Enhancer.");
                let mut fast_draft = self.settings.fast_model_field(active_provider).to_string();
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
                    let typical_cost = typical_in * thinking_price.input_per_million_usd
                        / 1_000_000.0
                        + typical_out * thinking_price.output_per_million_usd / 1_000_000.0;
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
                                        price_grid_row(ui, "Fast", &fast_model, fast_price, false);
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
                                        ui.label(format!("${:.3}/image", img_price.per_image_usd,));
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
                let resp = ui
                    .add(
                        egui::Slider::new(&mut temp, 0.0..=2.0)
                            .max_decimals(2)
                            .text("°"),
                    )
                    .on_hover_text("0 = deterministic, 2 = chaotic. Default 0.3.");
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
                .add(egui::DragValue::new(&mut iters).range(0..=5).speed(0.1))
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
                let parse_result = if trimmed.is_empty() {
                    Ok(None)
                } else {
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
                    self.settings.seed_override = Some(crate::app::util::pick_default_seed());
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
                        egui::RichText::new("not a valid u64").color(ui.visuals().warn_fg_color),
                    );
                }
            });
        });
    }
}
