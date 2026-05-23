use eframe::egui;

use crate::app::style;
use crate::app::types::EnhanceTarget;
use crate::app::MogenStudioApp;

/// Build the hover text shown on a disabled button. Lists every gating
/// condition currently failing so the user knows which prerequisite to fix
/// without having to discover them one by one.
fn disabled_reason(reasons: &[(&str, bool)]) -> Option<String> {
    let blockers: Vec<&str> = reasons
        .iter()
        .filter_map(|(label, ok)| if !ok { Some(*label) } else { None })
        .collect();
    if blockers.is_empty() {
        None
    } else if blockers.len() == 1 {
        Some(format!("Disabled — {}", blockers[0]))
    } else {
        Some(format!("Disabled:\n  • {}", blockers.join("\n  • ")))
    }
}

impl MogenStudioApp {
    pub(in crate::app) fn ui_llm(&mut self, ui: &mut egui::Ui) {
        // Per-file spending pill — shows total spend on this scene to
        // date plus a per-model breakdown tooltip. Clicking opens the
        // Spending panel filtered to this scene. No-op when the spend
        // tracker isn't installed (e.g. read-only DB).
        self.ui_scene_spend_pill(ui);

        // Inline provider switcher + global LLM toggles. Mirrors the
        // Preferences pane so the user can flip provider / planning mode /
        // seed without leaving the inspector. Persisted via Settings::save().
        ui.horizontal(|ui| {
            ui.label("Provider");
            let current_slot = self.settings.provider_slot();
            let mut chosen: Option<crate::settings::ProviderSlot> = None;
            egui::ComboBox::from_id_salt("llm_panel_provider")
                .selected_text(current_slot.label())
                .show_ui(ui, |ui| {
                    for slot in crate::settings::PROVIDER_SLOTS {
                        let selected = slot == current_slot;
                        if ui
                            .selectable_label(selected, slot.label())
                            .clicked()
                            && !selected
                        {
                            chosen = Some(slot);
                        }
                    }
                });
            if let Some(slot) = chosen {
                self.settings.set_provider_slot(slot);
                let _ = self.settings.save();
            }
        });
        ui.horizontal(|ui| {
            let mut plan_first = self.settings.plan_first();
            if ui
                .checkbox(&mut plan_first, "Plan first")
                .on_hover_text(
                    "Run the Architect (planner) pass before the Coder pass. \
                     Mirrors `mogen generate --plan` from the CLI.",
                )
                .changed()
            {
                self.settings.set_plan_first(plan_first);
                let _ = self.settings.save();
            }
            // Seed override readout — clicking Reset clears the per-file
            // override so the next LLM run mints a fresh seed. Editing the
            // seed itself stays in Preferences.
            let cur_seed = self.settings.seed_override();
            ui.separator();
            ui.label("Seed");
            match cur_seed {
                Some(s) => {
                    ui.label(egui::RichText::new(s.to_string()).monospace())
                        .on_hover_text(
                            "Global RNG override applied to every LLM run on every \
                             file. Edit in Preferences → LLM.",
                        );
                }
                None => {
                    ui.label(egui::RichText::new("(auto)").italics().weak())
                        .on_hover_text("Each LLM call gets a fresh random seed.");
                }
            }
        });

        ui.add_space(6.0);

        let has_key = self.resolve_api_key().is_some();
        let mut request_open_prefs = false;
        if !has_key {
            // Treat the no-key banner as a real call-to-action: state which
            // provider needs a key and offer a one-click jump into the right
            // pane of Preferences. Multi-line so each path the user can take
            // (paste / env var) gets its own line.
            let provider = self.settings.provider().display_name();
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("{provider}: no API key — every LLM action below is disabled."),
            );
            ui.label(
                egui::RichText::new(
                    "Quickest path: click \"Open Preferences\" and paste a \
                     key. The matching environment variable also works if \
                     you'd rather not store the key on disk.",
                )
                .weak(),
            );
            ui.horizontal(|ui| {
                if ui.button("Open Preferences").clicked() {
                    request_open_prefs = true;
                }
                ui.label(
                    egui::RichText::new("(Edit → Preferences… → LLM)").weak(),
                );
            });
            ui.add_space(4.0);
        }
        if request_open_prefs {
            self.show_options = true;
        }

        // Gate buttons on *this file's* in-flight state only — other files can
        // still have jobs running in parallel and the user can kick off a new
        // one here as long as this file isn't already busy.
        let busy = self.active().llm_in_flight.is_some();
        let src_empty = self.active().source.trim().is_empty();
        let has_path = self.active().path.is_some();
        let provider_name_full = self.settings.provider().display_name();
        let no_key_msg = format!("set a {provider_name_full} API key in Preferences");
        let busy_msg = "another LLM call is already in flight on this tab";
        let no_src_msg = "open or paste a .mog file first";
        let no_path_msg = "save the file first — textures writes PNGs next to it";

        // Show the classified error banner above the prompt fields so users
        // see the failure before deciding whether to Retry or edit.
        self.ui_llm_error_banner(ui);

        // Per-file thinking-budget override. Written into `meta(thinking=…)`
        // so it sticks across reopens; the CLI honours the same attribute.
        // "Use default" falls back to the setting in Options.
        self.ui_llm_thinking_override(ui);

        // Generate lives in the File → New from Prompt modal now; the
        // inspector only exposes transformations of the current MOG file.
        // Each action below gets its own subsection header so the
        // sidebar reads as four distinct tasks instead of a flat list.
        style::sub_section_header(ui, "Modify");
        let mod_id = egui::Id::new(("mog_llm_mod_prompt", self.active));
        crate::app::text_menu::text_edit_with_menu(
            ui,
            mod_id,
            &mut self.files[self.active].mod_prompt,
            |ui, text| {
                ui.add(
                    egui::TextEdit::multiline(text)
                        .hint_text(style::placeholder("e.g. make the legs taller"))
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .id(mod_id),
                )
            },
        );
        let mod_enabled = has_key && !busy && !src_empty;
        let mod_reason = disabled_reason(&[
            (no_key_msg.as_str(), has_key),
            (busy_msg, !busy),
            (no_src_msg, !src_empty),
        ]);
        ui.horizontal(|ui| {
            let resp = ui
                .add_enabled(mod_enabled, style::primary_button(ui, "Modify"))
                .on_hover_text(
                    mod_reason
                        .as_deref()
                        .unwrap_or("Smallest-edit rewrite of the current MOG file"),
                );
            if resp.clicked() {
                let ctx = ui.ctx().clone();
                self.start_llm_modify(ctx);
            }
            self.ui_enhance_button(
                ui,
                EnhanceTarget::Modify,
                "Rewrite the Modify prompt with the fast model — \
                 the model expands and clarifies your prompt before sending.",
            );
        });

        // "Include screenshot" toggle: when on, the next Modify click
        // renders the current scene to a thumbnail and attaches it to
        // the LLM call so the model can see what it's editing. Replaces
        // the old "Refine" button — same render-then-vision-LLM
        // pipeline, but driven by the user's prompt instead of a
        // built-in self-critique. Gated on the active provider
        // supporting images and the scene actually being renderable;
        // disabled (with explanatory tooltip) otherwise so users can
        // still see the feature exists.
        let provider = self.settings.provider();
        let provider_supports_images = provider.supports_images();
        let scene_renderable = self
            .active()
            .last_result
            .as_ref()
            .map(|r| r.scene.is_some() && !mogen_core::has_errors(&r.diagnostics))
            .unwrap_or(false);
        let provider_name = provider.display_name();
        let screenshot_tip = if !provider_supports_images {
            format!(
                "Switch to Gemini or Z.ai in Edit → Preferences — \
                 {provider_name} cannot read images, so it can't see a render."
            )
        } else if !scene_renderable {
            "Fix validation errors first — the model needs a renderable \
             scene to look at"
                .to_string()
        } else {
            "Render the current scene and attach the PNG so the model \
             can see what it's editing"
                .to_string()
        };
        let screenshot_enabled = provider_supports_images && scene_renderable;
        ui.add_enabled_ui(screenshot_enabled, |ui| {
            ui.checkbox(
                &mut self.files[self.active].mod_include_screenshot,
                "Include screenshot",
            )
            .on_hover_text(screenshot_tip);
        });
        if !provider_supports_images {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{provider_name} has no vision input — switch to \
                     Gemini or Z.ai to attach a screenshot",
                ),
            );
        }

        style::sub_section_header(ui, "Animate");
        let anim_id = egui::Id::new(("mog_llm_anim_prompt", self.active));
        crate::app::text_menu::text_edit_with_menu(
            ui,
            anim_id,
            &mut self.files[self.active].anim_prompt,
            |ui, text| {
                ui.add(
                    egui::TextEdit::multiline(text)
                        .hint_text(style::placeholder("e.g. spin the rotor at 120 rpm"))
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .id(anim_id),
                )
            },
        );
        let anim_enabled = has_key && !busy && !src_empty;
        let anim_reason = disabled_reason(&[
            (no_key_msg.as_str(), has_key),
            (busy_msg, !busy),
            (no_src_msg, !src_empty),
        ]);
        ui.horizontal(|ui| {
            let resp = ui
                .add_enabled(anim_enabled, style::primary_button(ui, "Animate"))
                .on_hover_text(
                    anim_reason
                        .as_deref()
                        .unwrap_or("Append joints/clips/skeleton to the current MOG file"),
                );
            if resp.clicked() {
                let ctx = ui.ctx().clone();
                self.start_llm_animate(ctx);
            }
            self.ui_enhance_button(
                ui,
                EnhanceTarget::Animate,
                "Rewrite the Animate prompt with the fast model — \
                 the model expands and clarifies your prompt before sending.",
            );
        });

        // Repair: no prompt — the validator diagnostics *are* the prompt.
        // Button only lights up when the current buffer has errors so users
        // don't burn tokens on a clean file.
        let error_count = self
            .active()
            .last_result
            .as_ref()
            .map(|r| {
                r.diagnostics
                    .iter()
                    .filter(|d| matches!(d.severity, mogen_core::Severity::Error))
                    .count()
            })
            .unwrap_or(0);
        let provider_name = self.settings.provider().display_name();
        style::sub_section_header(ui, "Repair");
        ui.label(
            egui::RichText::new(format!(
                "hand the current file's validation errors back to {provider_name} — \
                 each diagnostic is sent with a source excerpt, caret, and \
                 per-code fix hint",
            ))
            .weak(),
        );
        let repair_enabled = has_key && !busy && !src_empty && error_count > 0;
        let repair_label = if error_count > 0 {
            format!(
                "Repair ({error_count} error{})",
                if error_count == 1 { "" } else { "s" }
            )
        } else {
            "Repair".to_string()
        };
        let no_errors_msg =
            "no validation errors to fix — Repair lights up when the file fails to compile";
        let repair_reason = disabled_reason(&[
            (no_key_msg.as_str(), has_key),
            (busy_msg, !busy),
            (no_src_msg, !src_empty),
            (no_errors_msg, error_count > 0),
        ]);
        let repair_default_tip = format!(
            "Feed the diagnostics (with spans and fix hints) back to {provider_name}"
        );
        if ui
            .add_enabled(repair_enabled, style::primary_button(ui, &repair_label))
            .on_hover_text(repair_reason.as_deref().unwrap_or(&repair_default_tip))
            .clicked()
        {
            let ctx = ui.ctx().clone();
            self.start_llm_repair(ctx);
        }

        style::sub_section_header(ui, "Textures");
        ui.label(
            egui::RichText::new(
                "generates a base_color PNG per material, writes to ./textures/ \
                 next to the .mog, and splices the resulting paths into each \
                 material (uses Gemini Image regardless of the active provider)",
            )
            .weak(),
        );

        // Advanced texture knobs — the CLI exposes all of these and the GUI
        // used to silently pin them to defaults. Persisted per-file so users
        // can iterate.
        let cfg_open = self.active().texture_cfg.expanded;
        let style_id = egui::Id::new(("mog_llm_tex_style", self.active));
        let header = egui::CollapsingHeader::new("Texture options")
            .default_open(cfg_open)
            .id_salt(("tex_opts", self.active));
        let resp = header.show(ui, |ui| {
            // Scope the &mut cfg borrow so we can call self.ui_enhance_button
            // (which also needs &mut self) in between the grid and the
            // checkboxes.
            {
                let cfg = &mut self.files[self.active].texture_cfg;
                egui::Grid::new("tex_opts_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Style").on_hover_text(
                            "Free-form prompt suffix appended to every material's image prompt",
                        );
                        crate::app::text_menu::text_edit_with_menu(
                            ui,
                            style_id,
                            &mut cfg.style,
                            |ui, text| {
                                ui.add(
                                    egui::TextEdit::singleline(text)
                                        .hint_text(style::placeholder("photorealistic"))
                                        .desired_width(f32::INFINITY)
                                        .id(style_id),
                                )
                            },
                        );
                        ui.end_row();

                        ui.label("Texture size")
                            .on_hover_text("Cap on the longer side, in pixels (0 = keep native)");
                        ui.add(
                            egui::DragValue::new(&mut cfg.texture_size)
                                .range(0..=4096)
                                .speed(8.0),
                        );
                        ui.end_row();

                    });
            }

            self.ui_enhance_button(
                ui,
                EnhanceTarget::TextureStyle,
                "Rewrite the style hint with the fast model",
            );

            let cfg = &mut self.files[self.active].texture_cfg;
            ui.checkbox(&mut cfg.no_normal, "Skip normal map");
            ui.checkbox(&mut cfg.no_metallic_roughness, "Skip metallic/roughness");
            ui.checkbox(&mut cfg.no_occlusion, "Skip occlusion (AO)");
            ui.checkbox(&mut cfg.force, "Re-generate even if texture file exists");
        });
        // Persist whether the expander is open so it survives recompiles.
        self.files[self.active].texture_cfg.expanded = resp.openness > 0.5;

        let tex_enabled = has_key && !busy && !src_empty && has_path;
        let tex_reason = disabled_reason(&[
            (no_key_msg.as_str(), has_key),
            (busy_msg, !busy),
            (no_src_msg, !src_empty),
            (no_path_msg, has_path),
        ]);
        // Pre-flight cost estimate: count materials in the most recent
        // compile result, multiply by the per-image price, and surface the
        // estimate next to the button. Image-only price (the textures
        // pipeline always uses Gemini Flash Image regardless of the active
        // text provider).
        let material_count = self
            .active()
            .last_result
            .as_ref()
            .and_then(|r| r.scene.as_ref())
            .map(|s| s.materials.len())
            .unwrap_or(0);
        let image_price = crate::app::pricing::image_pricing("gemini-2.5-flash-image");
        let est_cost = material_count as f64 * image_price.per_image_usd;
        if material_count > 0 && image_price.per_image_usd > 0.0 {
            ui.label(
                egui::RichText::new(format!(
                    "Pre-flight: {material_count} material{} × {} ≈ {} per run \
                     (PBR maps are derived locally, no extra cost).",
                    if material_count == 1 { "" } else { "s" },
                    crate::app::pricing::format_usd(image_price.per_image_usd),
                    crate::app::pricing::format_usd(est_cost),
                ))
                .weak(),
            );
        }
        if ui
            .add_enabled(
                tex_enabled,
                style::primary_button(ui, "Generate Textures"),
            )
            .on_hover_text(tex_reason.as_deref().unwrap_or(
                "Run the textures pipeline with the options above. \
                 Writes PNGs to ./textures/ next to the .mog.",
            ))
            .clicked()
        {
            let ctx = ui.ctx().clone();
            self.start_llm_textures(ctx);
        }
        if !has_path && !src_empty {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "save the file first — textures writes PNGs next to it",
            );
        }

        if busy {
            ui.add_space(8.0);
            self.ui_llm_progress_card(ui);
        }

        // Per-material texture thumbnails + Generate/Regenerate/Delete/Reveal
        // actions live in the Materials inspector — see `ui_panels::materials`.
    }
}
