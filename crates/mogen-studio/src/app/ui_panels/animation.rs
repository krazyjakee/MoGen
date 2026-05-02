use std::time::Instant;

use eframe::egui;

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
            if ui
                .button("All")
                .on_hover_text("Activate every clip")
                .clicked()
            {
                self.viewer.set_all_clips_active(true);
            }
            if ui
                .button("None")
                .on_hover_text("Deactivate every clip")
                .clicked()
            {
                self.viewer.set_all_clips_active(false);
            }
        });

        ui.horizontal(|ui| {
            let mut speed = self.viewer.playback_speed();
            ui.label("Speed");
            let resp = ui.add(
                egui::Slider::new(&mut speed, -2.0..=4.0)
                    .suffix("×")
                    .fixed_decimals(2)
                    .clamping(egui::SliderClamping::Always),
            );
            if resp.changed() {
                self.viewer.set_playback_speed(speed);
            }
            if ui
                .button("1×")
                .on_hover_text("Reset playback speed to real time")
                .clicked()
                && (speed - 1.0).abs() > f32::EPSILON
            {
                self.viewer.set_playback_speed(1.0);
            }
        });

        ui.add_space(4.0);
        let file_i = self.active;
        let mut pending_delete: Option<String> = None;
        for i in visible_clip_indices {
            let c = &clips[i];
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    let mut on = active[i];
                    if ui
                        .checkbox(&mut on, "")
                        .on_hover_text("Include this clip in the active pose")
                        .changed()
                    {
                        self.viewer.set_clip_active(i, on);
                    }
                    ui.label(&c.name);
                    ui.label(
                        egui::RichText::new(format!("{:.2}s", c.duration))
                            .small()
                            .weak(),
                    );
                    if let Some(p) = &c.origin {
                        let stem = p
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("import");
                        ui.label(
                            egui::RichText::new(format!("⤴ {stem}"))
                                .small()
                                .weak(),
                        )
                        .on_hover_text(p.to_string_lossy());
                    }
                    // Delete: splice the authored clip (or procedural-template
                    // node that produced it) out of the source. Disabled when
                    // no matching AST node is found — multi-target templates
                    // produce `name_0`/`name_1`/… clips whose names don't map
                    // back to a single authored node.
                    let span = find_clip_source_span(&self.files[file_i].source, &c.name);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn = egui::Button::new("🗑").small();
                        let resp = ui.add_enabled(span.is_some(), btn).on_hover_text(
                            if span.is_some() {
                                "Delete this clip from the DSL source"
                            } else {
                                "This clip has no authored source to delete\n\
                                 (procedural template with multiple targets)"
                            },
                        );
                        if resp.clicked() {
                            pending_delete = Some(c.name.clone());
                        }
                    });
                });
                let dur = c.duration.max(0.001);
                let mut t = times[i].clamp(0.0, dur);
                let resp = ui.add(
                    egui::Slider::new(&mut t, 0.0..=dur)
                        .text("t")
                        .clamping(egui::SliderClamping::Always)
                        .fixed_decimals(2),
                );
                if resp.changed() {
                    self.viewer.seek_clip(i, t);
                }
            });
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
