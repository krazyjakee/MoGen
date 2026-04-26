use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use mogen_llm::textures::TextureStage;

use super::pricing::{format_usd, image_pricing, text_pricing};
use super::types::{EnhanceTarget, LlmErrorClass, LlmEvent, LlmEventTone, LlmKind, LlmProgress};
use super::util::{
    delete_material_textures, ellipsize_path, gather_texture_refs, resolve_for_check,
};
use super::MogenStudioApp;
use crate::settings::{thinking_level_label, THINKING_LEVELS};

impl MogenStudioApp {
    pub(super) fn ui_llm(&mut self, ui: &mut egui::Ui) {
        let has_key = self.resolve_api_key().is_some();
        if !has_key {
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                "no Gemini API key — set one in Edit → Preferences…",
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

        // Per-file thinking-budget override. Written into the `// mogen-generate
        // thinking=…` header so it sticks across reopens; the CLI honours the
        // same header. "Use default" falls back to the setting in Options.
        self.ui_llm_thinking_override(ui);

        // Generate lives in the File → New from Prompt modal now; the
        // inspector only exposes transformations of the current MOG file.
        ui.label("Modify current:");
        let mod_id = egui::Id::new(("mog_llm_mod_prompt", self.active));
        super::text_menu::text_edit_with_menu(
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
        super::text_menu::text_edit_with_menu(
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
        ui.label("Repair current:");
        ui.label(
            egui::RichText::new(
                "hand the current file's validation errors back to Gemini — \
                 each diagnostic is sent with a source excerpt, caret, and \
                 per-code fix hint",
            )
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
            "Feed the diagnostics (with spans and fix hints) back to Gemini".to_string()
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
                "generates a base_color PNG per material using Gemini Image, \
                 writes to ./textures/ next to the .mog, and splices the \
                 resulting paths into each material",
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
                        super::text_menu::text_edit_with_menu(
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

        // Texture thumbnail strip sits at the bottom so it doesn't push the
        // prompt boxes off-screen on narrow layouts. Only drawn when the
        // active file's scene actually references any albedo files on disk.
        self.ui_texture_thumbs(ui);
    }

    /// Small "Thinking (this file): [dropdown]" row. Writing a level splices a
    /// `// mogen-generate thinking=<level>` header into the .mog on the next
    /// LLM call, which both the CLI and Studio read back on subsequent runs.
    fn ui_llm_thinking_override(&mut self, ui: &mut egui::Ui) {
        let current = self.active().thinking_override;
        let global = self.settings.thinking_level();
        let preview = match current {
            Some(level) => thinking_level_label(level),
            None => "Use global default",
        };
        ui.horizontal(|ui| {
            ui.label("Thinking (this file):")
                .on_hover_text(
                    "Per-file cap on Gemini's reasoning budget. Saved into \
                     the .mog header so it applies to CLI runs too. Leave as \
                     \"Use global default\" to defer to Options.",
                );
            egui::ComboBox::from_id_salt(("mog_thinking_override", self.active))
                .selected_text(preview)
                .show_ui(ui, |ui| {
                    let default_label =
                        format!("Use global default ({})", thinking_level_label(global));
                    if ui
                        .selectable_label(current.is_none(), default_label)
                        .clicked()
                    {
                        self.active_mut().thinking_override = None;
                    }
                    for level in THINKING_LEVELS {
                        let selected = current == Some(level);
                        if ui
                            .selectable_label(selected, thinking_level_label(level))
                            .clicked()
                        {
                            self.active_mut().thinking_override = Some(level);
                        }
                    }
                });
            // Tiny hint about how the override is persisted. Quiet so it
            // doesn't shout at users who never touched the dropdown.
            if current.is_some() {
                ui.label(
                    egui::RichText::new("(saved in file header)")
                        .small()
                        .weak(),
                );
            }
        });
    }

    /// Bordered progress card shown while a Gemini call is running on the
    /// active file. Replaces the bare spinner+text with: a kind-coloured pill
    /// header, elapsed time, a stage-specific detail row (repair dots or a
    /// texture progress bar), and a short timeline of recent events.
    fn ui_llm_progress_card(&mut self, ui: &mut egui::Ui) {
        let Some(kind) = self.active().llm_in_flight else {
            return;
        };
        let accent = kind_color(kind);
        let started_at = self.active().llm_started_at;
        let progress = self.active().llm_progress.clone();
        let max_iters = self.settings.max_repair_iters();
        let events: Vec<LlmEvent> = self.active().llm_events.clone();

        let card_bg = ui.visuals().faint_bg_color;
        let mut cancel_clicked = false;

        egui::Frame::none()
            .fill(card_bg)
            .stroke(egui::Stroke::new(1.0, accent))
            .rounding(egui::Rounding::same(4.0))
            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
            .show(ui, |ui| {
                // ── header row: pill · stage caption · elapsed time ─────
                ui.horizontal(|ui| {
                    draw_kind_pill(ui, kind, accent);
                    ui.add_space(6.0);
                    ui.spinner();
                    ui.label(egui::RichText::new(stage_headline(&progress, kind)));
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if let Some(t0) = started_at {
                                ui.label(
                                    egui::RichText::new(format_elapsed(t0.elapsed()))
                                        .monospace()
                                        .weak(),
                                );
                            }
                        },
                    );
                });

                // ── stage detail: repair dots OR texture progress bar ───
                let repair = matches!(progress, Some(LlmProgress::Repair { .. }));
                let texture = matches!(progress, Some(LlmProgress::Texture { .. }));
                if repair || texture || kind == LlmKind::Textures {
                    ui.add_space(6.0);
                }
                if let Some(LlmProgress::Repair { iter, max, errors }) = &progress {
                    // `max` from the worker; fall back to settings if the
                    // worker reported 0 for any reason.
                    let max = if *max > 0 { *max } else { max_iters.max(1) };
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("repair loop").small().weak(),
                        );
                        ui.add_space(4.0);
                        draw_repair_dots(ui, *iter, max, accent);
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!("{iter}/{max}"))
                                .monospace()
                                .small(),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "· {errors} error{} to fix",
                                if *errors == 1 { "" } else { "s" }
                            ))
                            .small()
                            .color(ui.visuals().warn_fg_color),
                        );
                    });
                } else if let Some(LlmProgress::Texture {
                    current,
                    total,
                    material,
                    stage,
                }) = &progress
                {
                    let frac = if *total == 0 {
                        0.0
                    } else {
                        (*current as f32) / (*total as f32)
                    };
                    // Use a finite desired width — f32::INFINITY poisons the
                    // widget's interact rect and crashes egui's hit_test when
                    // comparing WidgetRect for equality (NaN != NaN).
                    let bar_width = ui.available_width().max(1.0);
                    ui.add(
                        egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                            .desired_width(bar_width)
                            .fill(accent)
                            .text(format!("{current}/{total}")),
                    );
                    let verb = match stage {
                        TextureStage::Generating => "generating",
                        TextureStage::Existing => "using existing PNG for",
                        TextureStage::Deriving => "deriving PBR for",
                        TextureStage::Done => "finished",
                    };
                    ui.label(
                        egui::RichText::new(format!("{verb} {material}"))
                            .small()
                            .weak(),
                    );
                }

                // ── timeline of recent events (oldest → newest) ─────────
                let now = Instant::now();
                let visible: Vec<&LlmEvent> = events
                    .iter()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                if !visible.is_empty() {
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(2.0);
                    // Freeze each finished event's timer at the moment the
                    // next event was logged; only the most recent event
                    // keeps ticking against `now`.
                    for i in 0..visible.len() {
                        let ev = visible[i];
                        let until = visible
                            .get(i + 1)
                            .map(|next| next.at)
                            .unwrap_or(now);
                        draw_timeline_row(ui, ev, until, accent);
                    }
                }

                // ── cancel button, right-aligned at the bottom ──────────
                // Wrap in ui.horizontal so the inner right_to_left(Center)
                // layout gets a finite row height. A bare with_layout here
                // inherits the vertical Frame's max_rect (y = INFINITY inside
                // a ScrollArea), and Center vertical-align + INFINITY height
                // produces a NaN frame rect in egui's layout math.
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui
                                .button("Cancel")
                                .on_hover_text(
                                    "Stop waiting and discard the result. \
                                     The background call may finish but its \
                                     output is dropped. (Esc)",
                                )
                                .clicked()
                            {
                                cancel_clicked = true;
                            }
                        },
                    );
                });
            });

        if cancel_clicked {
            self.cancel_active_llm();
        }

        // Keep the elapsed counter ticking even when the worker is quietly
        // waiting on an HTTP response (no Progress events to trigger paints).
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }

    /// Inline "Enhance" button shown directly under the four prompt inputs
    /// (Generate in the modal, Modify / Animate / Texture-Style in the
    /// inspector). Clicking kicks off a context-specific rewrite via the fast
    /// model; on success the rewritten text replaces the input in place, on
    /// failure the error is rendered alongside the button until the next
    /// enhance attempt. Disabled globally while another enhance is in flight
    /// or no API key is configured.
    pub(super) fn ui_enhance_button(
        &mut self,
        ui: &mut egui::Ui,
        target: EnhanceTarget,
        hover: &str,
    ) {
        let has_key = self.resolve_api_key().is_some();
        let any_busy = self.enhance_in_flight.is_some();
        let this_busy = matches!(
            self.enhance_in_flight.as_ref(),
            Some(s) if s.target == target
        );
        let can_run = has_key && !any_busy;
        let err_for_target = match &self.enhance_error {
            Some((t, msg)) if *t == target => Some(msg.clone()),
            _ => None,
        };

        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_run, egui::Button::new("✨ Enhance").small())
                .on_hover_text(hover)
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.start_prompt_enhance(ctx, target);
            }
            if this_busy {
                ui.spinner();
                ui.label(egui::RichText::new("enhancing…").small().weak());
            } else if let Some(msg) = err_for_target {
                ui.label(
                    egui::RichText::new(msg)
                        .small()
                        .color(ui.visuals().warn_fg_color),
                );
            } else if !has_key {
                ui.label(
                    egui::RichText::new("no API key")
                        .small()
                        .color(egui::Color32::from_rgb(230, 200, 100)),
                );
            }
        });
    }

    /// Classified error banner with Retry / Open Settings / Dismiss actions.
    /// No-op when the active file has no pending error.
    fn ui_llm_error_banner(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.active().llm_error.clone() else {
            return;
        };
        let accent = match info.class {
            LlmErrorClass::MissingKey | LlmErrorClass::InvalidKey => {
                egui::Color32::from_rgb(230, 150, 80)
            }
            LlmErrorClass::RateLimited
            | LlmErrorClass::Network
            | LlmErrorClass::ServerError => egui::Color32::from_rgb(230, 200, 100),
            LlmErrorClass::ContentBlocked => egui::Color32::from_rgb(200, 130, 200),
            LlmErrorClass::QuotaExceeded
            | LlmErrorClass::BadRequest
            | LlmErrorClass::Other => egui::Color32::from_rgb(230, 100, 100),
        };
        egui::Frame::none()
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, accent))
            .inner_margin(egui::Margin::same(6.0))
            .show(ui, |ui| {
                ui.colored_label(accent, egui::RichText::new(&info.headline).strong());
                ui.label(egui::RichText::new(&info.detail).small());
                ui.horizontal(|ui| {
                    let retry_label = if info.retryable { "Retry" } else { "Retry anyway" };
                    let has_last = self.active().llm_last_prompt.is_some();
                    if ui
                        .add_enabled(has_last, egui::Button::new(retry_label))
                        .on_hover_text(
                            "Re-submit the last prompt. Edit the prompt field above first \
                             if you want to tweak it before retrying.",
                        )
                        .clicked()
                    {
                        let ctx = ui.ctx().clone();
                        self.retry_active_llm(ctx);
                    }
                    if matches!(
                        info.class,
                        LlmErrorClass::MissingKey
                            | LlmErrorClass::InvalidKey
                            | LlmErrorClass::QuotaExceeded
                    ) && ui.button("Open Settings…").clicked()
                    {
                        self.show_options = true;
                    }
                    if ui
                        .small_button("Dismiss")
                        .on_hover_text("Hide this message without retrying")
                        .clicked()
                    {
                        self.active_mut().llm_error = None;
                    }
                });
            });
        ui.add_space(6.0);
    }

    /// Thumbnails of every generated base-colour texture referenced by the
    /// active scene. Only drawn when at least one PNG exists on disk.
    /// Right-clicking a thumb opens a per-material menu for regenerating
    /// just that texture or deleting the PNG group + clearing its `*_texture`
    /// attrs from the source.
    fn ui_texture_thumbs(&mut self, ui: &mut egui::Ui) {
        let i = self.active;
        let Some(result) = &self.files[i].last_result else {
            return;
        };
        let Some(scene) = &result.scene else {
            return;
        };

        let source_dir: Option<PathBuf> = self.files[i]
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());

        // Full slot list (not just albedo) so the per-material delete sweeps
        // every companion `*_texture` attr too. The thumbnail row itself
        // still only paints base_color — normal / MR / AO thumbnails read as
        // noise.
        let all_refs = gather_texture_refs(scene);
        let refs: Vec<(String, PathBuf)> = all_refs
            .iter()
            .filter(|(_, slot, _)| *slot == "base_color")
            .map(|(mat, _, rel)| {
                let abs = resolve_for_check(rel, source_dir.as_deref());
                (mat.clone(), abs)
            })
            .filter(|(_, abs)| abs.is_file())
            .collect();

        if refs.is_empty() {
            return;
        }

        ui.add_space(8.0);
        ui.label("Texture previews:");

        let ctx = ui.ctx().clone();
        let available = ui.available_width();
        // Four per row at inspector widths; shrink but don't fall below 48 px
        // or the previews lose all detail.
        let thumb_size = ((available - 24.0) / 4.0).clamp(48.0, 96.0);

        // Right-click action chosen during the loop. Applied after the UI
        // closures return so we can take a fresh `&mut self` borrow without
        // fighting `horizontal_wrapped`.
        enum ThumbAction {
            Regenerate,
            Delete,
        }
        let mut pending: Option<(String, ThumbAction)> = None;

        ui.horizontal_wrapped(|ui| {
            for (mat, abs) in &refs {
                let handle = match self.thumb_handle(&ctx, abs) {
                    Some(h) => h,
                    None => continue,
                };
                ui.vertical(|ui| {
                    let resp = ui
                        .add(
                            egui::Image::new((handle.id(), egui::vec2(thumb_size, thumb_size)))
                                .rounding(4.0)
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text(format!(
                            "{}\nright-click for actions",
                            ellipsize_path(abs, 60),
                        ));
                    resp.context_menu(|ui| {
                        ui.label(egui::RichText::new(mat).strong());
                        ui.separator();
                        if ui
                            .button("Regenerate")
                            .on_hover_text(
                                "Re-run the textures pipeline for just this material \
                                 (forces overwrite of existing PNGs)",
                            )
                            .clicked()
                        {
                            pending = Some((mat.clone(), ThumbAction::Regenerate));
                            ui.close_menu();
                        }
                        if ui
                            .button("Delete")
                            .on_hover_text(
                                "Remove the albedo + PBR companion PNGs and clear the \
                                 *_texture attrs on this material",
                            )
                            .clicked()
                        {
                            pending = Some((mat.clone(), ThumbAction::Delete));
                            ui.close_menu();
                        }
                    });
                    ui.add(
                        egui::Label::new(egui::RichText::new(mat).small().weak())
                            .truncate(),
                    );
                });
            }
        });

        if let Some((material, action)) = pending {
            match action {
                ThumbAction::Regenerate => {
                    self.start_llm_textures_for_material(ctx.clone(), material);
                }
                ThumbAction::Delete => {
                    let source = self.files[i].source.clone();
                    let (new_source, status) = delete_material_textures(
                        &source,
                        source_dir.as_deref(),
                        &material,
                        &all_refs,
                    );
                    let changed = new_source != source;
                    self.tex_exists_cache.clear();
                    self.thumb_cache.clear();
                    if changed {
                        {
                            let f = &mut self.files[i];
                            f.source = new_source;
                            f.dirty = f.source != f.last_saved_source;
                            f.needs_compile = true;
                            f.last_edit_at = Some(std::time::Instant::now());
                        }
                        // Texture-cleanup deletes PNGs from disk as a side
                        // effect, so this edit is non-undoable like the
                        // LLM completions. Break the coalesce chain so a
                        // subsequent gizmo / inspector edit doesn't merge
                        // into a pre-cleanup stack entry.
                        self.break_undo_chain(i);
                        self.compile_active();
                    }
                    self.active_mut().status = status;
                }
            }
        }
    }

    /// Look up or lazily upload a thumbnail for `abs`. `None` when the file
    /// isn't a readable PNG.
    fn thumb_handle(
        &mut self,
        ctx: &egui::Context,
        abs: &std::path::Path,
    ) -> Option<egui::TextureHandle> {
        use super::types::ThumbEntry;
        let mtime = std::fs::metadata(abs).ok().and_then(|m| m.modified().ok());
        if let Some(entry) = self.thumb_cache.get(abs) {
            if entry.mtime == mtime {
                return Some(entry.handle.clone());
            }
        }
        let bytes = std::fs::read(abs).ok()?;
        let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .ok()?
            .to_rgba8();
        // Downscale oversized albedos before upload — a 2048² RGBA8 texture
        // is 16 MB in VRAM per material, and thumbnails render at < 96 px.
        let image_rgba = if img.width().max(img.height()) > 128 {
            image::imageops::resize(&img, 128, 128, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let (w2, h2) = (image_rgba.width() as usize, image_rgba.height() as usize);
        let handle = ctx.load_texture(
            format!("thumb:{}", abs.display()),
            egui::ColorImage::from_rgba_unmultiplied([w2, h2], image_rgba.as_raw()),
            egui::TextureOptions::LINEAR,
        );
        self.thumb_cache
            .insert(abs.to_path_buf(), ThumbEntry { handle: handle.clone(), mtime });
        Some(handle)
    }

    /// Session meter right-aligned in the footer. Hidden when no calls have
    /// been made yet; the Reset button zeroes the counter.
    pub(super) fn ui_session_meter(&mut self, ui: &mut egui::Ui) {
        let u = self.session_usage.clone();
        if u.text_calls == 0 && u.image_calls == 0 {
            return;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Reset")
                .on_hover_text("Clear the session token / cost counters")
                .clicked()
            {
                self.session_usage = Default::default();
            }
            let tooltip = session_tooltip(&u, &self.settings.gemini_model());
            ui.label(format!(
                "· {} ({} tok, {})",
                calls_label(&u),
                u.prompt_tokens + u.response_tokens,
                format_usd(u.estimated_usd),
            ))
            .on_hover_text(tooltip);
        });
    }
}

/// Kind-specific accent colour used for the card's stroke, pill fill, repair
/// dots, and texture progress bar. Chosen so each LLM kind reads distinctly
/// even at a glance and stays legible on both the dark and light themes.
fn kind_color(k: LlmKind) -> egui::Color32 {
    match k {
        LlmKind::Generate => egui::Color32::from_rgb(110, 170, 230),
        LlmKind::Modify => egui::Color32::from_rgb(120, 210, 180),
        LlmKind::Animate => egui::Color32::from_rgb(200, 140, 220),
        LlmKind::Repair => egui::Color32::from_rgb(220, 130, 130),
        LlmKind::Textures => egui::Color32::from_rgb(230, 160, 100),
    }
}

/// Rounded capsule tagging the card with which kind of call is running.
/// Uses manual painter calls rather than a Button so it can't be clicked by
/// mistake and so the fill colour tracks the kind accent exactly.
fn draw_kind_pill(ui: &mut egui::Ui, kind: LlmKind, accent: egui::Color32) {
    let text = kind.label().to_uppercase();
    let galley = ui.painter().layout_no_wrap(
        text,
        egui::FontId::proportional(11.0),
        egui::Color32::BLACK,
    );
    let pad = egui::vec2(8.0, 3.0);
    let desired = galley.size() + pad * 2.0;
    let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let rounding = egui::Rounding::same(rect.height() * 0.5);
    ui.painter().rect_filled(rect, rounding, accent);
    let pos = rect.center() - galley.size() * 0.5;
    ui.painter().galley(pos, galley, egui::Color32::BLACK);
}

/// Render `max` dots, filling `filled` of them with `accent` and leaving the
/// rest as faint outlines. The dot just before `filled` pulses so users can
/// tell the pipeline is live even when the count hasn't ticked yet.
fn draw_repair_dots(ui: &mut egui::Ui, filled: u32, max: u32, accent: egui::Color32) {
    let dot_size = 10.0;
    let gap = 4.0;
    let count = max.max(1);
    let width = (dot_size * count as f32) + (gap * (count.saturating_sub(1)) as f32);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, dot_size),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    let empty_stroke = egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color);
    // Pulse the "currently active" dot (index = filled, 0-based) using a sine
    // of time so users see the pipeline is alive between Progress events.
    let pulse = 0.5
        + 0.5
            * (ui.ctx().input(|i| i.time) as f32 * std::f32::consts::TAU * 1.5)
                .sin();
    let active_idx = filled; // about to run
    for i in 0..count {
        let cx = rect.left() + dot_size * 0.5 + (dot_size + gap) * i as f32;
        let center = egui::pos2(cx, rect.center().y);
        if i < filled {
            painter.circle_filled(center, dot_size * 0.5, accent);
        } else if i == active_idx {
            // Subtle pulse: ring + inner dot that fades between 25% and 70%.
            let alpha = (0.25 + 0.45 * pulse).clamp(0.0, 1.0);
            let c = egui::Color32::from_rgba_unmultiplied(
                accent.r(),
                accent.g(),
                accent.b(),
                (alpha * 255.0) as u8,
            );
            painter.circle_filled(center, dot_size * 0.5, c);
            painter.circle_stroke(center, dot_size * 0.5, empty_stroke);
        } else {
            painter.circle_stroke(center, dot_size * 0.5, empty_stroke);
        }
    }
}

/// One line in the card's activity log. Shows a coloured leading bullet,
/// the stage message, and the duration of the step on the right
/// (`until - ev.at`). For finished events the caller passes the timestamp
/// of the next event so the timer freezes; for the in-flight event the
/// caller passes `now` so it keeps ticking.
fn draw_timeline_row(
    ui: &mut egui::Ui,
    ev: &LlmEvent,
    until: Instant,
    accent: egui::Color32,
) {
    let bullet_color = match ev.tone {
        LlmEventTone::Info => ui.visuals().widgets.noninteractive.fg_stroke.color,
        LlmEventTone::Repair => ui.visuals().warn_fg_color,
        LlmEventTone::Texture => accent,
        LlmEventTone::Done => egui::Color32::from_rgb(120, 200, 140),
    };
    ui.horizontal(|ui| {
        // Bullet.
        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(dot_rect.center(), 3.0, bullet_color);
        ui.add(
            egui::Label::new(egui::RichText::new(&ev.text).small())
                .truncate(),
        );
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let age = until.saturating_duration_since(ev.at);
                ui.label(
                    egui::RichText::new(format_age(age))
                        .small()
                        .weak()
                        .monospace(),
                );
            },
        );
    });
}

/// Headline string shown next to the spinner. Prefers the most recent
/// progress event; falls back to a kind-appropriate "starting…" when the
/// worker hasn't emitted anything yet.
fn stage_headline(p: &Option<LlmProgress>, kind: LlmKind) -> String {
    match p {
        Some(LlmProgress::Status(s)) => s.clone(),
        Some(LlmProgress::Repair { iter, max, errors }) => format!(
            "repair {iter}/{max} — {errors} error{} → re-calling Gemini",
            if *errors == 1 { "" } else { "s" }
        ),
        Some(LlmProgress::Texture {
            current,
            total,
            material,
            stage,
        }) => {
            let verb = match stage {
                TextureStage::Generating => "generating",
                TextureStage::Existing => "using existing PNG for",
                TextureStage::Deriving => "deriving PBR for",
                TextureStage::Done => "finished",
            };
            format!("{current}/{total} — {verb} {material}")
        }
        None => match kind {
            LlmKind::Generate => "waiting for Gemini…".into(),
            LlmKind::Modify => "waiting for Gemini…".into(),
            LlmKind::Animate => "waiting for Gemini…".into(),
            LlmKind::Repair => "waiting for Gemini…".into(),
            LlmKind::Textures => "preparing texture plan…".into(),
        },
    }
}

/// Elapsed time in `0.3s` / `12.4s` / `1m 05s` style. Tuned so the header
/// never wraps and the unit is easy to scan.
fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 10.0 {
        format!("{:.1}s", secs)
    } else if secs < 60.0 {
        format!("{:.0}s", secs)
    } else {
        let m = (secs / 60.0).floor() as u64;
        let s = (secs % 60.0) as u64;
        format!("{m}m {s:02}s")
    }
}

/// Shorter relative-age format for timeline entries ("0.4s", "3s", "47s", "2m").
/// Sub-second precision under 10s so very fast steps don't all collapse to "now".
fn format_age(d: Duration) -> String {
    let secs_f = d.as_secs_f64();
    if secs_f < 10.0 {
        format!("{:.1}s", secs_f)
    } else if secs_f < 60.0 {
        format!("{:.0}s", secs_f)
    } else {
        let m = (secs_f / 60.0).floor() as u64;
        format!("{m}m")
    }
}

fn calls_label(u: &super::types::SessionUsage) -> String {
    let mut parts = Vec::new();
    if u.text_calls > 0 {
        parts.push(format!(
            "{} text call{}",
            u.text_calls,
            if u.text_calls == 1 { "" } else { "s" }
        ));
    }
    if u.image_calls > 0 {
        parts.push(format!(
            "{} image{}",
            u.image_calls,
            if u.image_calls == 1 { "" } else { "s" }
        ));
    }
    parts.join(", ")
}

fn session_tooltip(u: &super::types::SessionUsage, model: &str) -> String {
    let price = text_pricing(model);
    let img = image_pricing(model);
    format!(
        "Session totals (model: {model})\n\n\
         text calls: {}\n\
         image calls: {}\n\
         prompt tokens: {}\n\
         response tokens: {}\n\
         cached tokens: {}\n\
         estimated cost: {}\n\n\
         rates: in ${:.2}/M · out ${:.2}/M · cached ${:.2}/M · img ${:.3}/call",
        u.text_calls,
        u.image_calls,
        u.prompt_tokens,
        u.response_tokens,
        u.cached_tokens,
        format_usd(u.estimated_usd),
        price.input_per_million_usd,
        price.output_per_million_usd,
        price.cached_input_per_million_usd,
        img.per_image_usd,
    )
}
