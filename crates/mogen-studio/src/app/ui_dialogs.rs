use eframe::egui;

use crate::settings::{
    thinking_level_key, thinking_level_label, DEFAULT_MAX_REPAIR_ITERS, THINKING_LEVELS,
};
use crate::theme::{apply_theme, theme_label, Theme, THEMES};

use super::types::{EnhanceTarget, DOCS_URL, GITHUB_REPO_URL, LICENSE_URL};
use super::MogenStudioApp;

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
                ui.heading("Gemini API key");
                ui.label(
                    "Used by Generate / Modify / Animate / Textures. Stored in your user \
                     config directory and persists between sessions.",
                );
                ui.add_space(6.0);
                let key_id = egui::Id::new("opts_api_key");
                super::text_menu::text_edit_with_menu(
                    ui,
                    key_id,
                    &mut self.options_api_key_draft,
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
                if std::env::var("GEMINI_API_KEY")
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false)
                {
                    ui.add_space(4.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(150, 180, 230),
                        "GEMINI_API_KEY is also set in your environment — \
                         the saved key here takes precedence when non-empty.",
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
                        self.settings.gemini_api_key =
                            self.options_api_key_draft.trim().to_string();
                        match self.settings.save() {
                            Ok(()) => {
                                let msg = if self.settings.gemini_api_key.is_empty() {
                                    "options: cleared saved Gemini API key".to_string()
                                } else {
                                    "options: settings saved".to_string()
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

    /// "New from Prompt…" dialog — the only place the LLM *generator* is
    /// surfaced. Producing a new scene always spawns a fresh untitled tab
    /// so the active MOG file isn't silently overwritten.
    pub(super) fn ui_new_prompt(&mut self, ctx: &egui::Context) {
        if !self.show_new_prompt {
            return;
        }
        let mut open = true;
        let mut close_after = false;
        let mut spawn_now = false;
        let has_key = self.resolve_api_key().is_some();
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
                                .hint_text("e.g. a wooden stool with three legs")
                                .desired_rows(4)
                                .desired_width(f32::INFINITY)
                                .id(prompt_id),
                        )
                    },
                );
                self.ui_enhance_button(
                    ui,
                    EnhanceTarget::Generate,
                    "Rewrite the prompt with the fast model",
                );
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
                    let can_generate = has_key && prompt_ok;
                    if ui
                        .add_enabled(can_generate, egui::Button::new("Generate"))
                        .on_hover_text("Create a new tab and run the generator on it")
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
        if !open || close_after {
            self.show_new_prompt = false;
        }
        if spawn_now {
            let prompt = self.new_prompt_draft.trim().to_string();
            self.new_untitled();
            self.active_mut().gen_prompt = prompt;
            let ctx_clone = ctx.clone();
            self.start_llm_generate(ctx_clone);
            self.new_prompt_draft.clear();
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
                    // the "wrote X (N bytes)" or "export failed: …" summary —
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
