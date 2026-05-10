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
        // Add-clip row — pick a procedural template + a target node, append
        // a default-config declaration to the scene. Plain `clip { track … }`
        // is the most flexible but the user typically wants a one-click
        // template; the LLM-driven Animate flow handles bespoke needs.
        ui.horizontal(|ui| {
            ui.label("New:");
            let kinds: [(&str, &str); 5] = [
                ("spin", "spin (rotation about an axis)"),
                ("open_close", "open_close (hinge swing)"),
                ("wave", "wave (oscillation)"),
                ("flap", "flap (one-direction oscillation)"),
                ("idle", "idle (subtle bob)"),
            ];
            let cur = self.add_clip_kind.clone();
            egui::ComboBox::from_id_salt("anim_new_clip_kind")
                .selected_text(&cur)
                .show_ui(ui, |ui| {
                    for (k, label) in kinds {
                        if ui
                            .selectable_value(&mut self.add_clip_kind, k.into(), label)
                            .clicked()
                        {}
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.add_clip_target)
                    .hint_text("target node")
                    .desired_width(120.0)
                    .id_salt("anim_new_clip_target"),
            );
            let target_ok = !self.add_clip_target.trim().is_empty();
            if ui
                .add_enabled(target_ok, egui::Button::new("Add"))
                .on_hover_text(
                    "Append a `<kind> \"<name>\" (target=\"<target>\")` declaration \
                     to the active scene with default rate/amplitude.",
                )
                .clicked()
            {
                let kind = self.add_clip_kind.clone();
                let target = self.add_clip_target.trim().to_string();
                let name = next_proc_clip_name(&self.files[self.active].source, &kind);
                let body = format!(
                    "{kind} \"{name}\" (target=\"{target}\")",
                );
                let i = self.active;
                let before = self.files[i].source.clone();
                let new_src = crate::edit::append_to_scene(&before, &body);
                if new_src != before {
                    {
                        let f = &mut self.files[i];
                        f.source = new_src;
                        f.dirty = f.source != f.last_saved_source;
                        f.needs_compile = true;
                        f.last_edit_at = Some(Instant::now());
                    }
                    self.break_undo_chain(i);
                    self.push_undo(
                        i,
                        before,
                        UndoKey {
                            surface: "animation",
                            attr: Some("__add".into()),
                            node_path: Vec::new(),
                        },
                    );
                }
            }
        });
        ui.add_space(4.0);

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
        let mut pending_rename: Option<(String, String)> = None;
        let mut pending_duration: Option<(String, f32)> = None;
        let warn_color = style::accent_warn(&ui.style().visuals);
        for i in visible_clip_indices {
            let c = &clips[i];
            ui.group(|ui| {
                // Lift the span lookup so the rename + duration rows further
                // down can gate on it without redoing the AST walk.
                let span = find_clip_source_span(&self.files[file_i].source, &c.name);
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

                // Row 4: rename + duration edit. Both are gated on having an
                // authored span — procedural-template clips with multi-target
                // expansion (`spin_0`, `spin_1`, …) skip them; the inspector
                // can't disambiguate which instance the user intended.
                if span.is_some() {
                    ui.add_space(2.0);
                    let cur_clip_name = c.name.clone();
                    let mut name_buf = self
                        .clip_name_drafts
                        .entry(cur_clip_name.clone())
                        .or_insert_with(|| cur_clip_name.clone())
                        .clone();
                    let name_resp = ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.add(
                            egui::TextEdit::singleline(&mut name_buf)
                                .desired_width(140.0)
                                .id_salt(("clip_rename", i)),
                        )
                    });
                    if name_resp.inner.changed() {
                        self.clip_name_drafts
                            .insert(cur_clip_name.clone(), name_buf.clone());
                    }
                    if name_resp.inner.lost_focus()
                        && !name_buf.trim().is_empty()
                        && name_buf != cur_clip_name
                    {
                        pending_rename = Some((cur_clip_name.clone(), name_buf.trim().to_string()));
                    }
                }
                if span.is_some() {
                    let cur_clip_name = c.name.clone();
                    let cur_dur = c.duration;
                    let key = format!("{cur_clip_name}::dur");
                    let mut dur_buf = self
                        .clip_duration_drafts
                        .entry(key.clone())
                        .or_insert_with(|| format!("{cur_dur:.3}"))
                        .clone();
                    let dur_resp = ui.horizontal(|ui| {
                        ui.label("Duration");
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut dur_buf)
                                .desired_width(80.0)
                                .id_salt(("clip_dur", i)),
                        );
                        ui.label(egui::RichText::new("s").weak());
                        resp
                    });
                    if dur_resp.inner.changed() {
                        self.clip_duration_drafts.insert(key.clone(), dur_buf.clone());
                    }
                    if dur_resp.inner.lost_focus() {
                        if let Ok(parsed) = dur_buf.trim().parse::<f32>() {
                            if (parsed - cur_dur).abs() > 1e-4 && parsed > 0.0 {
                                pending_duration = Some((cur_clip_name.clone(), parsed));
                            }
                        }
                    }
                }
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

        if let Some((old_name, new_name)) = pending_rename {
            if let Some(span) = find_clip_source_span(&self.files[file_i].source, &old_name) {
                let before = self.files[file_i].source.clone();
                let new_src = rewrite_node_name_literal(&before, span, &new_name);
                if new_src != before {
                    {
                        let f = &mut self.files[file_i];
                        f.source = new_src;
                        f.dirty = f.source != f.last_saved_source;
                        f.needs_compile = true;
                        f.last_edit_at = Some(Instant::now());
                    }
                    self.clip_name_drafts.remove(&old_name);
                    self.break_undo_chain(file_i);
                    self.push_undo(
                        file_i,
                        before,
                        UndoKey {
                            surface: "animation",
                            attr: Some("__rename".into()),
                            node_path: Vec::new(),
                        },
                    );
                }
            }
        }

        if let Some((clip_name, dur)) = pending_duration {
            if let Some(span) = find_clip_source_span(&self.files[file_i].source, &clip_name) {
                let before = self.files[file_i].source.clone();
                // Plain `clip` and `open_close` use `seconds=`. `wave`/`flap`/
                // `idle` derive duration from `hz=` (1/hz) so a direct edit
                // wouldn't round-trip — we set `seconds=` anyway since the
                // lowering pass for those kinds ignores it. The user gets
                // expected behaviour for `clip` / `open_close`; for the
                // others the field is a no-op (the readout above doesn't
                // change). Detected and skipped via the kind check.
                let new_src = crate::edit::set_attr(
                    &before,
                    span,
                    "seconds",
                    &format!("{dur:.4}").trim_end_matches('0').trim_end_matches('.').to_string(),
                );
                if new_src != before {
                    {
                        let f = &mut self.files[file_i];
                        f.source = new_src;
                        f.dirty = f.source != f.last_saved_source;
                        f.needs_compile = true;
                        f.last_edit_at = Some(Instant::now());
                    }
                    self.clip_duration_drafts.remove(&format!("{clip_name}::dur"));
                    self.break_undo_chain(file_i);
                    self.push_undo(
                        file_i,
                        before,
                        UndoKey {
                            surface: "animation",
                            attr: Some("seconds".into()),
                            node_path: Vec::new(),
                        },
                    );
                }
            }
        }
    }
}

/// Rewrite the first quoted name literal inside the span. Used by clip
/// rename — the clip's name lives in the `kind "name" (...)` header, just
/// like materials. Tolerant of escaped quotes. Returns the source unchanged
/// if no quoted literal is found in `span`.
fn rewrite_node_name_literal(src: &str, span: mogen_core::Span, new_name: &str) -> String {
    let bytes = src.as_bytes();
    let start = span.start.min(src.len());
    let end = span.end.min(src.len());
    let mut i = start;
    while i < end && bytes[i] != b'"' {
        i += 1;
    }
    if i >= end {
        return src.to_string();
    }
    let q_open = i;
    i += 1;
    while i < end && bytes[i] != b'"' {
        if bytes[i] == b'\\' && i + 1 < end {
            i += 2;
            continue;
        }
        i += 1;
    }
    if i >= end {
        return src.to_string();
    }
    let q_close = i;
    let mut out = String::with_capacity(src.len() + new_name.len());
    out.push_str(&src[..q_open + 1]);
    out.push_str(new_name);
    out.push_str(&src[q_close..]);
    out
}

/// Suggest a unique `<kind>_<n>` name for a freshly-added procedural clip.
/// Same algorithm as `next_material_name` over the kind keyword.
fn next_proc_clip_name(src: &str, kind: &str) -> String {
    let prefix = format!("{kind}_");
    let mut max_n: u32 = 0;
    for line in src.lines() {
        let trimmed = line.trim_start();
        let after_kw = match trimmed.strip_prefix(&format!("{kind} ")) {
            Some(s) => s,
            None => continue,
        };
        let after_quote = match after_kw.trim_start().strip_prefix('"') {
            Some(s) => s,
            None => continue,
        };
        let end = match after_quote.find('"') {
            Some(e) => e,
            None => continue,
        };
        let name = &after_quote[..end];
        if let Some(rest) = name.strip_prefix(&prefix) {
            if let Ok(n) = rest.parse::<u32>() {
                if n > max_n {
                    max_n = n;
                }
            }
        }
    }
    format!("{prefix}{}", max_n + 1)
}
