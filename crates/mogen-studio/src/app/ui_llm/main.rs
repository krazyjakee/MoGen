use eframe::egui;

use crate::app::types::EnhanceTarget;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    pub(in crate::app) fn ui_llm(&mut self, ui: &mut egui::Ui) {
        let has_key = self.resolve_api_key().is_some();
        if !has_key {
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                format!(
                    "no {} API key — set one in Edit → Preferences…",
                    self.settings.provider().display_name(),
                ),
            );
        }

        // Gate buttons on *this file's* in-flight state only — other files can
        // still have jobs running in parallel and the user can kick off a new
        // one here as long as this file isn't already busy.
        let busy = self.active().llm_in_flight.is_some();
        let src_empty = self.active().source.trim().is_empty();
        let has_path = self.active().path.is_some();

        // Show the classified error banner above the prompt fields so users
        // see the failure before deciding whether to Retry or edit.
        self.ui_llm_error_banner(ui);

        // Per-file thinking-budget override. Written into `meta(thinking=…)`
        // so it sticks across reopens; the CLI honours the same attribute.
        // "Use default" falls back to the setting in Options.
        self.ui_llm_thinking_override(ui);

        // Generate lives in the File → New from Prompt modal now; the
        // inspector only exposes transformations of the current MOG file.
        ui.label("Modify current:");
        let mod_id = egui::Id::new(("mog_llm_mod_prompt", self.active));
        crate::app::text_menu::text_edit_with_menu(
            ui,
            mod_id,
            &mut self.files[self.active].mod_prompt,
            |ui, text| {
                ui.add(
                    egui::TextEdit::multiline(text)
                        .hint_text("e.g. make the legs taller")
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .id(mod_id),
                )
            },
        );
        let mod_enabled = has_key && !busy && !src_empty;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(mod_enabled, egui::Button::new("Modify"))
                .on_hover_text("Smallest-edit rewrite of the current MOG file")
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.start_llm_modify(ctx);
            }
            self.ui_enhance_button(
                ui,
                EnhanceTarget::Modify,
                "Rewrite the Modify prompt with the fast model",
            );
        });

        ui.add_space(8.0);
        ui.label("Animate current:");
        let anim_id = egui::Id::new(("mog_llm_anim_prompt", self.active));
        crate::app::text_menu::text_edit_with_menu(
            ui,
            anim_id,
            &mut self.files[self.active].anim_prompt,
            |ui, text| {
                ui.add(
                    egui::TextEdit::multiline(text)
                        .hint_text("e.g. spin the rotor at 120 rpm")
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .id(anim_id),
                )
            },
        );
        let anim_enabled = has_key && !busy && !src_empty;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(anim_enabled, egui::Button::new("Animate"))
                .on_hover_text("Append joints/clips/skeleton to the current MOG file")
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.start_llm_animate(ctx);
            }
            self.ui_enhance_button(
                ui,
                EnhanceTarget::Animate,
                "Rewrite the Animate prompt with the fast model",
            );
        });

        ui.add_space(8.0);
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
        ui.label("Repair current:");
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
        let repair_tip = if src_empty {
            "Open or paste a .mog file first".to_string()
        } else if error_count == 0 {
            "No validation errors to repair".to_string()
        } else {
            format!("Feed the diagnostics (with spans and fix hints) back to {provider_name}")
        };
        if ui
            .add_enabled(repair_enabled, egui::Button::new(repair_label))
            .on_hover_text(repair_tip)
            .clicked()
        {
            let ctx = ui.ctx().clone();
            self.start_llm_repair(ctx);
        }

        ui.add_space(8.0);
        // Refine current — feeds the rendered scene back to the
        // reviewer agent (mirrors `mogen generate --auto-refine N` /
        // `mogen modify --auto-refine N` from the CLI). Gated on the
        // active provider supporting images: today only Gemini does.
        let provider = self.settings.provider();
        let provider_name = provider.display_name();
        let provider_supports_images = provider.supports_images();
        let scene_renderable = self
            .active()
            .last_result
            .as_ref()
            .map(|r| r.scene.is_some() && !mogen_core::has_errors(&r.diagnostics))
            .unwrap_or(false);
        ui.label("Refine current:");
        ui.label(
            egui::RichText::new(format!(
                "render the current scene, hand it back to {provider_name} with the \
                 original prompt, and let it propose corrections based on what it can see",
            ))
            .weak(),
        );
        let refine_enabled = has_key
            && !busy
            && !src_empty
            && scene_renderable
            && provider_supports_images;
        let refine_tip = if !provider_supports_images {
            format!(
                "Switch to Gemini in Edit → Preferences — {provider_name} cannot read \
                 images, so it cannot critique a render. Only vision-capable providers \
                 can refine."
            )
        } else if src_empty {
            "Open or paste a .mog file first".to_string()
        } else if !scene_renderable {
            "Fix validation errors first — the reviewer needs a renderable scene to look at"
                .to_string()
        } else if busy {
            "Another LLM call is already running on this file".to_string()
        } else {
            format!(
                "Render the current scene, hand the PNG back to {provider_name} as a \
                 self-critique pass, and apply the corrected DSL"
            )
        };
        ui.horizontal(|ui| {
            // Always-enabled spinbox so the user can pick the next-click
            // count even while a previous run is still in flight.
            ui.label("Iterations");
            ui.add(
                egui::DragValue::new(&mut self.files[self.active].refine_iters)
                    .range(
                        crate::app::llm::refine::MIN_REFINE_ITERS
                            ..=crate::app::llm::refine::MAX_REFINE_ITERS,
                    )
                    .speed(0.05),
            )
            .on_hover_text(
                "How many render → reviewer → repair passes to chain. Each \
                 iteration spends one round-trip plus any repair calls.",
            );
            // The button label echoes the iteration count so a glance
            // tells the user how much work the click queues — same
            // shape `Repair (N errors)` uses.
            let iters = self.files[self.active]
                .refine_iters
                .clamp(
                    crate::app::llm::refine::MIN_REFINE_ITERS,
                    crate::app::llm::refine::MAX_REFINE_ITERS,
                );
            let refine_label = if iters == 1 {
                "Refine".to_string()
            } else {
                format!("Refine {iters}×")
            };
            if ui
                .add_enabled(refine_enabled, egui::Button::new(refine_label))
                .on_hover_text(refine_tip)
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.start_llm_refine(ctx, iters);
            }
        });
        if !provider_supports_images {
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                format!(
                    "{provider_name} has no vision input — switch to Gemini to enable Refine",
                ),
            );
        }

        ui.add_space(8.0);
        ui.label("Textures:");
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
                                        .hint_text("photorealistic")
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
        if ui
            .add_enabled(tex_enabled, egui::Button::new("Generate Textures"))
            .on_hover_text(
                "Run the textures pipeline with the options above. \
                 Writes PNGs to ./textures/ next to the .mog.",
            )
            .clicked()
        {
            let ctx = ui.ctx().clone();
            self.start_llm_textures(ctx);
        }
        if !has_path && !src_empty {
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
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
