use std::path::Path;

use eframe::egui;
use mogen_llm::Provider;

use crate::settings::{
    thinking_level_key, thinking_level_label, DEFAULT_MAX_REPAIR_ITERS, PROVIDERS,
    THINKING_LEVELS,
};
use crate::theme::{apply_theme, theme_label, Theme, THEMES};

use super::types::{
    EnhanceTarget, ExternalChangeKind, GenImageInput, MAX_GEN_IMAGE_BYTES, DOCS_URL,
    GITHUB_REPO_URL, LICENSE_URL,
};
use super::MogenStudioApp;

/// Decode the image bytes via the `image` crate, downscale to a thumbnail
/// (96 px on the long side), and upload to the GPU as an `egui::TextureHandle`
/// so the dialog can show a preview without re-decoding every frame. Returns
/// `None` if decoding fails — the dialog will still send the raw bytes to
/// Gemini in that case (the model can handle PNG/JPG/WEBP), it just won't
/// show a thumbnail.
fn make_image_thumbnail(
    ctx: &egui::Context,
    bytes: &[u8],
    name: &str,
) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    let thumb = img.thumbnail(96, 96);
    let rgba = thumb.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw());
    Some(ctx.load_texture(
        format!("gen_prompt_thumb:{name}"),
        color_image,
        egui::TextureOptions::LINEAR,
    ))
}

/// Map an image's bytes (sniffed) and file extension to an `image/*` MIME
/// string Gemini accepts. Sniffing wins over the extension because phone
/// cameras frequently mislabel `.jpg` as `.heic` or vice versa; we only
/// fall back to the extension when sniffing fails.
fn guess_image_mime(bytes: &[u8], path: &Path) -> String {
    use image::ImageFormat;
    if let Ok(fmt) = image::guess_format(bytes) {
        return match fmt {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Bmp => "image/bmp",
            ImageFormat::Tiff => "image/tiff",
            _ => "application/octet-stream",
        }
        .to_string();
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Models surfaced in the Preferences dropdown. Free-form text still wins if
/// a user types one in, but these cover the tiers almost every user will
/// want and give pricing expectations a named anchor.
const MODEL_PRESETS: &[&str] = &[
    "gemini-pro-latest",
    "gemini-2.5-pro",
    "gemini-flash-latest",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
];

impl MogenStudioApp {
    pub(super) fn ui_options(&mut self, ctx: &egui::Context) {
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
                let key_heading = match active_provider {
                    Provider::Gemini => "Gemini API key",
                    Provider::OpenAI => "OpenAI API key",
                    Provider::Anthropic => "Anthropic API key",
                    Provider::Ollama => "Ollama API key (optional)",
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
                };
                super::text_menu::text_edit_with_menu(
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
                if std::env::var(env_var)
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

                ui.add_space(12.0);
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
                ui.heading("Thinking budget");
                ui.label(
                    "Cap on Gemini's hidden reasoning tokens per call. Higher = better DSL on \
                     hard prompts but slower and more expensive.",
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
                ui.heading("Models");
                ui.label(
                    "Thinking model runs the heavy DSL paths (generate / modify / animate). \
                     Fast model runs cheap rewrites like the Prompt Enhancer. Flash is \
                     ~4× cheaper than Pro — keep the two split so a polish call doesn't \
                     pay Pro rates.",
                );
                ui.add_space(6.0);

                // --- thinking model picker ---
                ui.label("Thinking model")
                    .on_hover_text(
                        "Used for generate / modify / animate and their repair loops. \
                         Pick a preset or type a custom model id.",
                    );
                let mut thinking_draft = self.settings.gemini_model.clone();
                egui::ComboBox::from_id_salt("opts_model_thinking")
                    .selected_text(if thinking_draft.is_empty() {
                        "(library default: gemini-pro-latest)"
                    } else {
                        thinking_draft.as_str()
                    })
                    .show_ui(ui, |ui| {
                        for m in MODEL_PRESETS {
                            if ui
                                .selectable_label(thinking_draft == *m, *m)
                                .clicked()
                            {
                                thinking_draft = m.to_string();
                            }
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut thinking_draft)
                        .hint_text("gemini-pro-latest")
                        .desired_width(f32::INFINITY),
                );
                self.settings.gemini_model = thinking_draft;

                ui.add_space(6.0);

                // --- fast model picker ---
                ui.label("Fast model")
                    .on_hover_text(
                        "Used for low-stakes text rewrites like the Prompt Enhancer. \
                         Default is gemini-flash-latest.",
                    );
                let mut fast_draft = self.settings.gemini_fast_model.clone();
                egui::ComboBox::from_id_salt("opts_model_fast")
                    .selected_text(if fast_draft.is_empty() {
                        "(library default: gemini-flash-latest)"
                    } else {
                        fast_draft.as_str()
                    })
                    .show_ui(ui, |ui| {
                        for m in MODEL_PRESETS {
                            if ui.selectable_label(fast_draft == *m, *m).clicked() {
                                fast_draft = m.to_string();
                            }
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut fast_draft)
                        .hint_text("gemini-flash-latest")
                        .desired_width(f32::INFINITY),
                );
                self.settings.gemini_fast_model = fast_draft;

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
                                "How many times to re-call Gemini when the generated DSL fails \
                                 validation. Higher = more API cost but fewer invalid outputs.",
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
                                    Some(super::util::pick_default_seed());
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

                ui.add_space(12.0);
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

    /// Confirmation shown when a window-close is requested while any buffer
    /// is dirty. Lists the unsaved files and offers Save All / Discard /
    /// Cancel. Save All walks every dirty buffer and invokes `save_index`,
    /// which opens a Save As dialog for untitled tabs — if the user cancels
    /// any of those, the modal stays open so no work is silently lost.
    pub(super) fn ui_quit_confirm(&mut self, ctx: &egui::Context) {
        if !self.show_quit_confirm {
            return;
        }
        let mut open = true;
        let mut do_save_all = false;
        let mut do_discard = false;
        let mut do_cancel = false;
        let dirty_names: Vec<String> = self
            .files
            .iter()
            .filter(|f| f.dirty)
            .map(|f| f.display_name())
            .collect();
        egui::Window::new("Unsaved changes")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} unsaved file{} will be lost if you quit now:",
                    dirty_names.len(),
                    if dirty_names.len() == 1 { "" } else { "s" },
                ));
                ui.add_space(4.0);
                for name in &dirty_names {
                    ui.label(format!("  • {name}"));
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Save All")
                        .on_hover_text(
                            "Save each unsaved MOG file, then quit. \
                             Untitled MOG files open a Save As dialog.",
                        )
                        .clicked()
                    {
                        do_save_all = true;
                    }
                    if ui
                        .button("Discard")
                        .on_hover_text("Quit without saving — unsaved edits are lost")
                        .clicked()
                    {
                        do_discard = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                });
            });

        if !open || do_cancel {
            self.show_quit_confirm = false;
            return;
        }

        if do_save_all {
            let mut all_clean = true;
            for i in 0..self.files.len() {
                if self.files[i].dirty && !self.save_index(i) {
                    all_clean = false;
                    break;
                }
            }
            if all_clean {
                self.show_quit_confirm = false;
                self.confirmed_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            // Otherwise leave the modal open so the user can retry or cancel.
            return;
        }

        if do_discard {
            self.show_quit_confirm = false;
            self.confirmed_quit = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Confirmation shown when the user tries to close a single dirty tab.
    /// Mirrors `ui_quit_confirm` but scoped to one buffer — Save invokes
    /// `save_index` (which may open a Save As dialog for untitled tabs; the
    /// modal stays open if the dialog is cancelled), Discard closes without
    /// saving, Cancel dismisses the modal.
    pub(super) fn ui_close_confirm(&mut self, ctx: &egui::Context) {
        let Some(i) = self.pending_close_index else {
            return;
        };
        if i >= self.files.len() {
            self.pending_close_index = None;
            return;
        }
        let name = self.files[i].display_name();
        let mut open = true;
        let mut do_save = false;
        let mut do_discard = false;
        let mut do_cancel = false;
        egui::Window::new("Unsaved changes")
            .id(egui::Id::new("close_tab_confirm"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "“{name}” has unsaved changes. Close this tab anyway?"
                ));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Save")
                        .on_hover_text(
                            "Save this MOG file, then close the tab. \
                             Untitled MOG files open a Save As dialog.",
                        )
                        .clicked()
                    {
                        do_save = true;
                    }
                    if ui
                        .button("Discard")
                        .on_hover_text("Close the tab without saving — unsaved edits are lost")
                        .clicked()
                    {
                        do_discard = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                });
            });

        if !open || do_cancel {
            self.pending_close_index = None;
            return;
        }

        if do_save {
            if self.save_index(i) {
                self.pending_close_index = None;
                self.close_file(i);
            }
            // Save As cancelled — leave the modal open so the user can retry.
            return;
        }

        if do_discard {
            self.pending_close_index = None;
            self.close_file(i);
        }
    }

    /// Help → About modal. Shows the crate version, the brand line, and
    /// links back out to the GitHub repo / docs / license.
    pub(super) fn ui_about(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let mut open = true;
        let mut close_after = false;
        egui::Window::new("About MoGen Studio")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("MoGen Studio");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.add_space(6.0);
                ui.label(
                    "Desktop frontend for the MoGen pipeline — compiles \
                     declarative .mog scenes into glTF 2.0 .glb assets, with \
                     a live 3D preview and Gemini-driven generate / modify / \
                     animate / texture flows.",
                );
                ui.add_space(10.0);
                ui.hyperlink_to("GitHub repository", GITHUB_REPO_URL);
                ui.hyperlink_to("Documentation (docs/dsl.md)", DOCS_URL);
                ui.hyperlink_to("License (MIT)", LICENSE_URL);
                ui.add_space(10.0);
                ui.label("© 2026 Jake Cattrall. Released under the MIT license.");
                ui.add_space(10.0);
                if ui.button("Close").clicked() {
                    close_after = true;
                }
            });
        if !open || close_after {
            self.show_about = false;
        }
    }

    /// "Ask MoGen" modal raised from the editor context menu. Lets the user
    /// ask Gemini Flash a free-form question about the snippet they had
    /// selected (or the whole file if nothing was selected). Read-only — the
    /// answer is shown inline; the editor buffer is never touched.
    pub(super) fn ui_ask(&mut self, ctx: &egui::Context) {
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

                ui.label(
                    egui::RichText::new(&context_label)
                        .strong()
                        .small(),
                );
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
                super::text_menu::text_edit_with_menu(
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

                if !has_key {
                    ui.add_space(4.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 200, 100),
                        "no Gemini API key — set one in Edit → Preferences…",
                    );
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let question_ok = !self.ask_question_draft.trim().is_empty();
                    let can_ask = has_key && question_ok && !in_flight;
                    if ui
                        .add_enabled(can_ask, egui::Button::new("Ask"))
                        .on_hover_text("Send the question to Gemini Flash")
                        .clicked()
                    {
                        submit_now = true;
                    }
                    if in_flight {
                        ui.spinner();
                        ui.label("asking Gemini…");
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
                            ui.label(
                                egui::RichText::new("Answer")
                                    .strong()
                                    .small(),
                            );
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

    /// "New from Prompt…" dialog — the only place the LLM *generator* is
    /// surfaced. Producing a new scene always spawns a fresh untitled tab
    /// so the active MOG file isn't silently overwritten.
    ///
    /// The dialog accepts an optional reference image alongside the text
    /// prompt (image-to-3D). Either input on its own is enough to enable
    /// the Generate button — the Pro vision model can interpret the image
    /// without supplementing text, and a text-only prompt keeps the legacy
    /// flow working unchanged.
    pub(super) fn ui_new_prompt(&mut self, ctx: &egui::Context) {
        if !self.show_new_prompt {
            return;
        }
        let mut open = true;
        let mut close_after = false;
        let mut spawn_now = false;
        let mut pick_image = false;
        let mut clear_image = false;
        let has_key = self.resolve_api_key().is_some();
        let mut image_status: Option<String> = None;
        egui::Window::new("New from Prompt")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(500.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Describe the scene you want Gemini to generate:");
                let prompt_id = egui::Id::new("new_prompt_draft");
                super::text_menu::text_edit_with_menu(
                    ui,
                    prompt_id,
                    &mut self.new_prompt_draft,
                    |ui, text| {
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .hint_text(
                                    "e.g. a wooden stool with three legs \
                                     (optional when an image is attached)",
                                )
                                .desired_rows(4)
                                .desired_width(f32::INFINITY)
                                .id(prompt_id),
                        )
                    },
                );
                if self.new_prompt_focus_pending {
                    ui.ctx().memory_mut(|m| m.request_focus(prompt_id));
                    self.new_prompt_focus_pending = false;
                }
                self.ui_enhance_button(
                    ui,
                    EnhanceTarget::Generate,
                    "Rewrite the prompt with the fast model",
                );

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label("Reference image (optional):");
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if let Some(img) = self.new_prompt_image.as_ref() {
                        if let Some(tex) = img.thumbnail.as_ref() {
                            // Letterbox into a 64×64 slot so the row height is
                            // stable regardless of the source image's aspect.
                            let aspect = {
                                let s = tex.size_vec2();
                                if s.y > 0.0 { s.x / s.y } else { 1.0 }
                            };
                            let h = 64.0;
                            let w = (h * aspect).clamp(24.0, 96.0);
                            ui.image((tex.id(), egui::vec2(w, h)));
                        }
                        let label = img
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| img.path.display().to_string());
                        ui.vertical(|ui| {
                            ui.label(label);
                            let kib = (img.data.len() as f64) / 1024.0;
                            let size_label = if kib >= 1024.0 {
                                format!("{:.1} MB · {}", kib / 1024.0, img.mime_type)
                            } else {
                                format!("{:.0} KB · {}", kib, img.mime_type)
                            };
                            ui.weak(size_label);
                        });
                        if ui.button("Remove").clicked() {
                            clear_image = true;
                        }
                        if ui.button("Replace…").clicked() {
                            pick_image = true;
                        }
                    } else if ui.button("Choose image…").clicked() {
                        pick_image = true;
                    }
                });

                if !has_key {
                    ui.add_space(4.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 200, 100),
                        "no Gemini API key — set one in Edit → Preferences…",
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let prompt_ok = !self.new_prompt_draft.trim().is_empty();
                    let image_ok = self.new_prompt_image.is_some();
                    let can_generate = has_key && (prompt_ok || image_ok);
                    let hover = if image_ok && !prompt_ok {
                        "Create a new tab and generate from the attached image"
                    } else if image_ok {
                        "Create a new tab and generate from the prompt + image"
                    } else {
                        "Create a new tab and run the generator on it"
                    };
                    if ui
                        .add_enabled(can_generate, egui::Button::new("Generate"))
                        .on_hover_text(hover)
                        .clicked()
                    {
                        spawn_now = true;
                        close_after = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close_after = true;
                    }
                });
            });

        if pick_image {
            if let Err(msg) = self.pick_gen_prompt_image(ctx) {
                image_status = Some(msg);
            }
        }
        if clear_image {
            self.new_prompt_image = None;
        }
        if let Some(msg) = image_status {
            // Surface picker errors (too large / unreadable) on the active
            // tab's status bar so the user sees them after the dialog closes.
            self.active_mut().status = msg;
        }

        if !open || close_after {
            self.show_new_prompt = false;
        }
        if spawn_now {
            let prompt = self.new_prompt_draft.trim().to_string();
            // Move the staged image (if any) into the new tab so Retry can
            // re-issue the same call without re-picking the file. Strip the
            // GPU thumbnail handle before transfer — the worker thread
            // doesn't need it and TextureHandle isn't worth shuttling around.
            let mut staged = self.new_prompt_image.take();
            if let Some(img) = staged.as_mut() {
                img.thumbnail = None;
            }
            self.new_untitled();
            self.active_mut().gen_prompt = prompt;
            self.active_mut().gen_image = staged;
            let ctx_clone = ctx.clone();
            self.start_llm_generate(ctx_clone);
            self.new_prompt_draft.clear();
        }
        if !self.show_new_prompt {
            // Drop any staged image when the dialog closes without submitting,
            // so re-opening starts fresh and the GPU thumbnail handle is freed.
            self.new_prompt_image = None;
        }
    }

    /// Open a native file dialog for an image and stash the result on
    /// `self.new_prompt_image`. Returns `Err(msg)` on read / size failures so
    /// the caller can surface the message to the user.
    fn pick_gen_prompt_image(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let dialog = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif", "bmp"])
            .set_directory(&self.project_root);
        let Some(path) = dialog.pick_file() else {
            return Ok(());
        };
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("couldn't read image: {e}"))?;
        if bytes.len() > MAX_GEN_IMAGE_BYTES {
            return Err(format!(
                "image too large ({:.1} MB); cap is {} MB — try a smaller file",
                (bytes.len() as f64) / (1024.0 * 1024.0),
                MAX_GEN_IMAGE_BYTES / (1024 * 1024),
            ));
        }
        let mime_type = guess_image_mime(&bytes, &path);
        if !mime_type.starts_with("image/") {
            return Err(format!(
                "unsupported file type ({}) — pick a PNG, JPG, WEBP, GIF, or BMP",
                mime_type,
            ));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".to_string());
        let thumbnail = make_image_thumbnail(ctx, &bytes, &name);
        self.new_prompt_image = Some(GenImageInput {
            path,
            mime_type,
            data: bytes,
            thumbnail,
        });
        Ok(())
    }

    /// Modal raised by the on-disk watcher when an open MOG file changed
    /// outside MoGen Studio while the buffer had unsaved edits. Offers three
    /// resolutions for `Modified` — Reload from disk (discard local edits),
    /// Keep mine (re-baseline the watcher so we stop prompting), Save over
    /// disk (overwrite). For `Deleted` the choices collapse to: Save (recreate
    /// at the original path), Keep buffer (treat the path as gone — buffer
    /// becomes effectively untitled-with-a-suggested-name), or Close tab.
    pub(super) fn ui_external_conflict(&mut self, ctx: &egui::Context) {
        let Some(conflict) = self.pending_external.as_ref() else {
            return;
        };
        let i = conflict.file_index;
        if i >= self.files.len() {
            self.pending_external = None;
            return;
        }
        let kind = conflict.kind;
        let name = self.files[i].display_name();
        let path_disp = self
            .files[i]
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        let mut open = true;
        let mut do_reload = false;
        let mut do_keep = false;
        let mut do_overwrite = false;
        let mut do_close = false;
        egui::Window::new(match kind {
            ExternalChangeKind::Modified => "File changed on disk",
            ExternalChangeKind::Deleted => "File deleted on disk",
        })
        .id(egui::Id::new("external_conflict"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            match kind {
                ExternalChangeKind::Modified => {
                    ui.label(format!(
                        "“{name}” was modified outside MoGen Studio while you have \
                         unsaved edits."
                    ));
                }
                ExternalChangeKind::Deleted => {
                    ui.label(format!(
                        "“{name}” no longer exists at its original path."
                    ));
                }
            }
            if !path_disp.is_empty() {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(&path_disp).monospace().weak());
            }
            ui.add_space(10.0);
            match kind {
                ExternalChangeKind::Modified => {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Reload from disk")
                            .on_hover_text(
                                "Discard your unsaved edits and load the on-disk version. \
                                 Cannot be undone from the editor's history.",
                            )
                            .clicked()
                        {
                            do_reload = true;
                        }
                        if ui
                            .button("Keep mine")
                            .on_hover_text(
                                "Keep your unsaved buffer as-is. Stops prompting for this \
                                 change; saving later will overwrite the disk version.",
                            )
                            .clicked()
                        {
                            do_keep = true;
                        }
                        if ui
                            .button("Save (overwrite disk)")
                            .on_hover_text("Write your buffer over the disk file now.")
                            .clicked()
                        {
                            do_overwrite = true;
                        }
                    });
                }
                ExternalChangeKind::Deleted => {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Save (recreate file)")
                            .on_hover_text("Write your buffer back to the original path.")
                            .clicked()
                        {
                            do_overwrite = true;
                        }
                        if ui
                            .button("Keep buffer")
                            .on_hover_text(
                                "Keep the in-memory buffer; the original path is treated as \
                                 gone. Save As to give it a new home.",
                            )
                            .clicked()
                        {
                            do_keep = true;
                        }
                        if ui
                            .button("Close tab")
                            .on_hover_text("Close this tab without saving.")
                            .clicked()
                        {
                            do_close = true;
                        }
                    });
                }
            }
        });

        if !open {
            // X button — same as Keep: dismiss without disk side effects but
            // re-baseline so we don't keep firing the modal every tick.
            do_keep = true;
        }

        if do_reload {
            // Replace buffer with the snapshot we read at detection time so
            // the user resolves against exactly what they were prompted on.
            let conflict = self.pending_external.take().expect("checked above");
            if let Some(disk_src) = conflict.disk_source {
                let f = &mut self.files[i];
                f.source = disk_src.clone();
                f.last_saved_source = disk_src;
                f.dirty = false;
                f.disk_mtime = conflict.disk_mtime;
                f.last_edit_at = None;
                f.needs_compile = false;
                f.status = format!("reloaded {name} (discarded unsaved edits)");
            }
            self.compile_file(i);
            if i == self.active {
                self.refresh_viewer_from_active();
            }
            return;
        }

        if do_keep {
            let conflict = self.pending_external.take().expect("checked above");
            let f = &mut self.files[i];
            // For Modified: re-baseline mtime so the next watcher tick treats
            // the *current* on-disk content as known and doesn't immediately
            // re-prompt. The buffer stays dirty against the new on-disk
            // content, which is what the user chose.
            // For Deleted: clear the mtime so a subsequent re-creation of the
            // file fires the watcher again (lets the user resolve the new
            // conflict explicitly instead of silently overwriting).
            f.disk_mtime = match conflict.kind {
                ExternalChangeKind::Modified => conflict.disk_mtime,
                ExternalChangeKind::Deleted => None,
            };
            f.dirty = f.source != f.last_saved_source;
            f.status = match conflict.kind {
                ExternalChangeKind::Modified => format!("kept buffer for {name} (disk diverged)"),
                ExternalChangeKind::Deleted => format!("kept buffer for {name} (file deleted)"),
            };
            return;
        }

        if do_overwrite {
            // Take the conflict before the borrow checker complains about
            // save_index using `self` mutably while we hold a reference.
            let _ = self.pending_external.take();
            self.save_index(i);
            return;
        }

        if do_close {
            let _ = self.pending_external.take();
            self.close_file(i);
        }
    }

    /// "Build GLB" dialog. Edits an `ExportOptions` draft and, when the user
    /// clicks Build, spawns a background worker that runs the merge / export
    /// pipeline off the UI thread. While a build is in flight the toggles
    /// lock out and the modal shows a spinner + the current stage. When done,
    /// the modal displays the result and a Close button.
    pub(super) fn ui_export_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_export {
            return;
        }

        let in_flight = self.build_rx.is_some();
        let mut open = true;
        let mut do_build = false;
        let mut do_cancel = false;
        let mut do_close = false;
        let i = self.active;
        let file_has_path = self.files[i].path.is_some();
        let last_status = self.files[i].status.clone();
        let current_stage = self
            .build_stage
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default();

        egui::Window::new("Build GLB")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Compile the active scene to glTF 2.0 binary (.glb).");
                if !file_has_path {
                    ui.add_space(4.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 200, 100),
                        "untitled MOG file — output will be written to the project root \
                         as untitled.glb. Save the MOG file first to export next to it.",
                    );
                }

                ui.add_space(10.0);
                ui.heading("Options");

                let opts = &mut self.export_opts_draft;
                ui.add_enabled_ui(!in_flight, |ui| {
                    ui.checkbox(
                        &mut opts.include_animations,
                        "Include animations",
                    )
                    .on_hover_text(
                        "Emit the scene's `animations[]` array. Off = bake a static GLB.",
                    );
                    ui.checkbox(&mut opts.include_textures, "Include textures")
                        .on_hover_text(
                            "Pack texture images into the GLB binary chunk and wire them to \
                             materials. Off = materials export with only PBR numeric factors.",
                        );
                    ui.checkbox(
                        &mut opts.merge_sibling_meshes,
                        "Merge overlapping meshes (CSG union)",
                    )
                    .on_hover_text(
                        "Collapse same-material, non-skinned sibling meshes under each parent \
                         into a single CSG-unioned mesh. Removes interior geometry where shapes \
                         overlap. Slow on complex scenes. UVs are preserved through the merge \
                         when all operands in a group have them.",
                    );
                });

                ui.add_space(12.0);

                if in_flight {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let label = if current_stage.is_empty() {
                            "working…".to_string()
                        } else {
                            format!("{current_stage}…")
                        };
                        ui.label(label);
                    });
                    ui.add_space(6.0);
                    if ui
                        .button("Cancel")
                        .on_hover_text(
                            "Stop waiting. The background worker may still finish but its \
                             output is discarded.",
                        )
                        .clicked()
                    {
                        do_cancel = true;
                    }
                } else {
                    // After a build completes, `self.files[i].status` carries
                    // the "wrote X (size)" or "export failed: …" summary —
                    // surface it so the user can see the result in-modal.
                    if last_status.starts_with("wrote ") || last_status.starts_with("export failed") {
                        ui.label(&last_status);
                        ui.add_space(6.0);
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .button("Build")
                            .on_hover_text("Compile + export to .glb with the options above")
                            .clicked()
                        {
                            do_build = true;
                        }
                        if ui.button("Close").clicked() {
                            do_close = true;
                        }
                    });
                }
            });

        if !open || do_close {
            self.show_export = false;
            return;
        }
        if do_cancel {
            self.cancel_build();
            return;
        }
        if do_build {
            let ctx_clone = ctx.clone();
            self.spawn_build(ctx_clone);
        }
    }
}
