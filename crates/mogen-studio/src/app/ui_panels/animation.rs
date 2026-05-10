use std::time::Instant;

use eframe::egui;

use crate::app::style;
use crate::app::types::UndoKey;
use crate::app::util::{find_clip_source_span, origin_in_visible_set, visible_origins};
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// True when at least one clip is currently in scope for the right
    /// sidebar — i.e. authored in the active file, or owned by the import
    /// the user just selected a node from. Drives the Animation panel's
    /// visibility so an all-import scene with nothing selected doesn't
    /// surface an empty header with playback controls.
    pub(in crate::app) fn has_visible_clips(&self) -> bool {
        let i = self.active;
        let Some(result) = &self.files[i].last_result else {
            return false;
        };
        let Some(scene) = &result.scene else {
            return false;
        };
        if scene.clips.is_empty() {
            return false;
        }
        let visible = visible_origins(scene, self.viewer.primary_selection());
        scene
            .clips
            .iter()
            .any(|c| origin_in_visible_set(&c.origin, &visible))
    }

    pub(in crate::app) fn ui_animation(&mut self, ui: &mut egui::Ui) {
        let clips = self.viewer.clips_snapshot();
        if clips.is_empty() {
            return;
        }

        let mut active = self.viewer.active_clips();
        let mut times = self.viewer.anim_times();
        // A fresh compile can briefly desync the snapshot lengths; pad so
        // iteration below is safe.
        if active.len() != clips.len() {
            active.resize(clips.len(), false);
        }
        if times.len() != clips.len() {
            times.resize(clips.len(), 0.0);
        }

        // Scope the listing to the active scene by default. The selection's
        // origin (when an imported node is picked) lets that import's clips
        // through too, so the user can scrub them in context. We hold onto
        // the original index so `set_clip_active` / `seek_clip` keep
        // addressing the viewer's full clip list.
        let visible_set: std::collections::HashSet<std::path::PathBuf> = {
            let i = self.active;
            self.files[i]
                .last_result
                .as_ref()
                .and_then(|r| r.scene.as_ref())
                .map(|s| visible_origins(s, self.viewer.primary_selection()))
                .unwrap_or_default()
        };
        let visible_clip_indices: Vec<usize> = clips
            .iter()
            .enumerate()
            .filter(|(_, c)| origin_in_visible_set(&c.origin, &visible_set))
            .map(|(idx, _)| idx)
            .collect();
        if visible_clip_indices.is_empty() {
            return;
        }

        let playing = self.viewer.is_playing();
        ui.horizontal(|ui| {
            let label = if playing { "⏸ Pause" } else { "▶ Play" };
            if ui
                .button(label)
                .on_hover_text("Toggle clip playback")
                .clicked()
            {
                self.viewer.set_playing(!playing);
            }
            if ui
                .button("Reset")
                .on_hover_text("Rewind every clip to t = 0")
                .clicked()
            {
                self.viewer.reset_anim_times();
            }
            // Visual divider between transport controls (Pause / Reset) and
            // the bulk-selection controls (All / None) so the two button
            // clusters don't read as one undifferentiated row.
            ui.separator();
            ui.label(egui::RichText::new("Enable:").weak());
            if ui
                .button("All")
                .on_hover_text("Enable every clip")
                .clicked()
            {
                self.viewer.set_all_clips_active(true);
            }
            if ui
                .button("None")
                .on_hover_text("Disable every clip")
                .clicked()
            {
                self.viewer.set_all_clips_active(false);
            }
        });

        ui.horizontal(|ui| {
            let mut speed = self.viewer.playback_speed();
            ui.label("Speed").on_hover_text(
                "Negative values play clips in reverse; 0 pauses; 1× is real \
                 time; 2× plays twice as fast.",
            );
            let resp = ui.add(
                egui::Slider::new(&mut speed, -2.0..=4.0)
                    .suffix("×")
                    .max_decimals(2)
                    .clamping(egui::SliderClamping::Always),
            );
            if resp.changed() {
                self.viewer.set_playback_speed(speed);
            }
            // Reset button only appears when the slider is off-default; at
            // 1× the readout already says so and a "1×" button next to a
            // "1.00×" readout is just visual redundancy.
            let at_default = (speed - 1.0).abs() < f32::EPSILON;
            if !at_default
                && ui
                    .button("Reset to 1×")
                    .on_hover_text("Reset playback speed to real time")
                    .clicked()
            {
                self.viewer.set_playback_speed(1.0);
            }
            // Tag what the current speed value actually does so users see at
            // a glance that −1× means rewind and 0× means paused-by-slider.
            let tag = if speed.abs() < 0.005 {
                "paused"
            } else if speed < 0.0 {
                "reverse"
            } else if at_default {
                "real time"
            } else if speed > 1.0 {
                "fast"
            } else {
                "slow"
            };
            ui.label(egui::RichText::new(format!("({tag})")).weak());
        });

        // Bigger gap separates the toolbar above from the clip list below;
        // inside the loop, a smaller add_space splits cards from each other.
        ui.add_space(8.0);
        let file_i = self.active;
        let mut pending_delete: Option<String> = None;
        let warn_color = style::accent_warn(&ui.style().visuals);
        for i in visible_clip_indices {
            let c = &clips[i];
            ui.group(|ui| {
                // Row 1: enable + name + (right-aligned) delete affordance.
                ui.horizontal(|ui| {
                    let mut on = active[i];
                    if ui
                        .checkbox(&mut on, "")
                        .on_hover_text("Include this clip in the active pose")
                        .changed()
                    {
                        self.viewer.set_clip_active(i, on);
                    }
                    ui.label(egui::RichText::new(&c.name).strong());
                    let span = find_clip_source_span(&self.files[file_i].source, &c.name);
                    let pending_for_this =
                        self.clip_delete_pending.as_deref() == Some(c.name.as_str());
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            // Two-step confirm: first click latches a pending
                            // state and turns the trash icon into a tinted
                            // "Delete?" chip; second click commits. Any other
                            // click in the panel clears the latch (handled
                            // below by resetting on the catch-all).
                            let (label, fill, tip) = if pending_for_this {
                                (
                                    egui::RichText::new("Delete?").strong(),
                                    Some(warn_color),
                                    "Click again to delete this clip",
                                )
                            } else if span.is_some() {
                                (
                                    egui::RichText::new("🗑"),
                                    None,
                                    "Delete this clip from the DSL source",
                                )
                            } else {
                                (
                                    egui::RichText::new("🗑"),
                                    None,
                                    "This clip has no authored source to delete\n\
                                     (procedural template with multiple targets)",
                                )
                            };
                            let mut btn = egui::Button::new(label).small();
                            if let Some(c) = fill {
                                btn = btn.fill(c);
                            }
                            let resp = ui
                                .add_enabled(span.is_some(), btn)
                                .on_hover_text(tip);
                            if resp.clicked() {
                                if pending_for_this {
                                    pending_delete = Some(c.name.clone());
                                    self.clip_delete_pending = None;
                                } else {
                                    self.clip_delete_pending = Some(c.name.clone());
                                }
                            }
                        },
                    );
                });

                // Row 2: meta line — duration and (optional) import origin
                // tag. Indented past the checkbox column so the eye reads
                // them as belonging to the clip name above.
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.label(
                        egui::RichText::new(style::format_seconds(c.duration)).weak(),
                    );
                    if let Some(p) = &c.origin {
                        let stem = p
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("import");
                        ui.label(
                            egui::RichText::new(format!("⤴ {stem}")).weak(),
                        )
                        .on_hover_text(p.to_string_lossy());
                    }
                });

                // Row 3: scrub slider. Outside-label pattern matches LOD
                // scale and Speed for visual consistency; suffix " s" so the
                // numeric readout is self-describing.
                ui.add_space(2.0);
                let dur = c.duration.max(0.001);
                let mut t = times[i].clamp(0.0, dur);
                ui.horizontal(|ui| {
                    ui.label("Time");
                    let resp = ui.add(
                        egui::Slider::new(&mut t, 0.0..=dur)
                            .suffix(" s")
                            .clamping(egui::SliderClamping::Always)
                            .max_decimals(2),
                    );
                    if resp.changed() {
                        self.viewer.seek_clip(i, t);
                    }
                });
            });
            ui.add_space(4.0);
        }
        if let Some(name) = pending_delete {
            use crate::edit;
            if let Some(span) = find_clip_source_span(&self.files[file_i].source, &name) {
                let before = self.files[file_i].source.clone();
                let new_src = edit::delete_node(&before, span);
                {
                    let f = &mut self.files[file_i];
                    f.source = new_src;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
                self.break_undo_chain(file_i);
                self.push_undo(
                    file_i,
                    before,
                    UndoKey {
                        surface: "animation",
                        attr: None,
                        node_path: Vec::new(),
                    },
                );
            }
        }
    }
}
