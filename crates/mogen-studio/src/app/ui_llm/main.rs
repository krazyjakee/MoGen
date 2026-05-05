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
