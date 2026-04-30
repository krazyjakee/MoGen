//! Editor Find (Ctrl+F) — query bar above the code editor with prev/next
//! navigation, match counter, and case-sensitivity toggle.
//!
//! The find input keeps focus while the user navigates so they can keep
//! typing. To avoid fighting the TextEdit's "selection only paints when
//! focused" rule, every match is drawn as a translucent overlay rect by
//! `paint_overlays` after the editor has rendered (current match in a
//! brighter colour). The editor's actual cursor is also moved onto the
//! current match so closing the find bar leaves the user at a useful spot.

use eframe::egui;
use egui::text::CCursor;
use egui::widgets::text_edit::TextEditOutput;

use super::types::find_input_id;
use super::MogenStudioApp;

impl MogenStudioApp {
    /// Open the find bar (no-op if already open). Captures any selection in
    /// the active editor as the initial query so Ctrl+F-with-a-word-selected
    /// works the way every editor does it.
    pub(super) fn open_find(&mut self, ctx: &egui::Context) {
        if !self.find.open {
            self.find.open = true;
            // Re-running the search the user had open last time is friendly;
            // only overwrite the query if there's a fresh selection to use.
            let editor_id = self.active_editor_id();
            if let Some(state) = egui::TextEdit::load_state(ctx, editor_id) {
                if let Some(range) = state.cursor.char_range() {
                    let lo = range.primary.index.min(range.secondary.index);
                    let hi = range.primary.index.max(range.secondary.index);
                    if hi > lo {
                        let src = &self.files[self.active].source;
                        let snippet: String = src.chars().skip(lo).take(hi - lo).collect();
                        // Only adopt if the selection looks like a search term
                        // (single line, non-trivial). Long multi-line selects
                        // are usually copy targets, not search seeds.
                        if !snippet.contains('\n') && !snippet.trim().is_empty() {
                            self.find.query = snippet;
                        }
                    }
                }
            }
            self.find.current = 0;
            self.recompute_find_matches();
            self.find.scroll_pending = (!self.find.matches.is_empty()).then_some(0);
        }
        self.find.focus_pending = true;
    }

    pub(super) fn close_find(&mut self) {
        self.find.open = false;
        self.find.scroll_pending = None;
        self.find.matches.clear();
    }

    /// Handle Ctrl+F (open) and find-bar-only navigation keys (F3 / Shift+F3).
    /// Esc-to-close is handled inline in `ui_find_bar` because it must be
    /// gated on the find input owning focus.
    pub(super) fn dispatch_find_shortcuts(&mut self, ctx: &egui::Context) {
        let find_sc = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::F);
        if ctx.input_mut(|i| i.consume_shortcut(&find_sc)) {
            self.open_find(ctx);
        }

        if self.find.open {
            // F3 / Shift+F3 cycle matches even when focus is elsewhere — match
            // the convention every Windows / Linux editor uses.
            let next_sc = egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::F3);
            let prev_sc = egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::F3);
            if ctx.input_mut(|i| i.consume_shortcut(&prev_sc)) {
                self.find_prev();
            } else if ctx.input_mut(|i| i.consume_shortcut(&next_sc)) {
                self.find_next();
            }
        }
    }

    fn find_next(&mut self) {
        if self.find.matches.is_empty() {
            return;
        }
        self.find.current = (self.find.current + 1) % self.find.matches.len();
        self.find.scroll_pending = Some(self.find.current);
    }

    fn find_prev(&mut self) {
        if self.find.matches.is_empty() {
            return;
        }
        self.find.current = if self.find.current == 0 {
            self.find.matches.len() - 1
        } else {
            self.find.current - 1
        };
        self.find.scroll_pending = Some(self.find.current);
    }

    /// Walk the active source and refill `find.matches` with char-index
    /// ranges. Cheap for typical .mog sizes; we just call this whenever the
    /// query, source, or case toggle could have changed.
    pub(super) fn recompute_find_matches(&mut self) {
        let src = &self.files[self.active].source;
        self.find.matches.clear();
        if self.find.query.is_empty() {
            return;
        }
        let needle: Vec<char> = self.find.query.chars().collect();
        let n = needle.len();
        if n == 0 {
            return;
        }
        let hay: Vec<char> = src.chars().collect();
        if hay.len() < n {
            return;
        }
        let cs = self.find.case_sensitive;
        for start in 0..=hay.len() - n {
            let mut ok = true;
            for j in 0..n {
                let a = hay[start + j];
                let b = needle[j];
                let eq = if cs {
                    a == b
                } else {
                    // `to_lowercase` returns an iterator because some chars
                    // map to multiple lowercase chars (German ß → ss). For
                    // search the simple comparison is good enough — fold both
                    // sides through the same iterator and check equality.
                    a.to_lowercase().eq(b.to_lowercase())
                };
                if !eq {
                    ok = false;
                    break;
                }
            }
            if ok {
                self.find.matches.push(start..start + n);
            }
        }
        if self.find.current >= self.find.matches.len() {
            self.find.current = 0;
        }
    }

    /// Paint the find bar above the editor. Returns nothing — direct mutation
    /// of `self.find` drives the next frame's match list and scroll target.
    pub(super) fn ui_find_bar(&mut self, ui: &mut egui::Ui) {
        let mut want_close = false;
        let mut want_next = false;
        let mut want_prev = false;
        let mut query_changed = false;

        let visuals = ui.visuals();
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(6.0, 4.0))
            .fill(visuals.faint_bg_color)
            .stroke(egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Find");

                    let input_id = find_input_id();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.find.query)
                            .id(input_id)
                            .desired_width(220.0)
                            .hint_text("type to search"),
                    );
                    if resp.changed() {
                        query_changed = true;
                    }
                    // singleline TextEdit consumes Enter as "lost focus"; map
                    // that back to next/prev so the user can drive navigation
                    // without leaving the keyboard.
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        if ui.input(|i| i.modifiers.shift) {
                            want_prev = true;
                        } else {
                            want_next = true;
                        }
                        ui.memory_mut(|m| m.request_focus(input_id));
                    }
                    if self.find.focus_pending {
                        ui.memory_mut(|m| m.request_focus(input_id));
                        // Move the cursor to the end so the prefilled selection
                        // doesn't immediately get blown away by the user typing.
                        if !self.find.query.is_empty() {
                            if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), input_id) {
                                let n = self.find.query.chars().count();
                                st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                                    egui::text::CCursor::new(n),
                                )));
                                st.store(ui.ctx(), input_id);
                            }
                        }
                        self.find.focus_pending = false;
                    }
                    // Esc inside the find input closes the bar. Gated on focus
                    // so we don't swallow Esc from elsewhere (LLM cancel etc).
                    if ui.memory(|m| m.has_focus(input_id))
                        && ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        want_close = true;
                    }

                    let label = if self.find.query.is_empty() {
                        String::new()
                    } else if self.find.matches.is_empty() {
                        "No results".to_string()
                    } else {
                        format!("{} of {}", self.find.current + 1, self.find.matches.len())
                    };
                    ui.add_sized(
                        [88.0, 16.0],
                        egui::Label::new(egui::RichText::new(label).weak()),
                    );

                    let nav_enabled = !self.find.matches.is_empty();
                    if ui
                        .add_enabled(nav_enabled, egui::Button::new("▲").small())
                        .on_hover_text("Previous match (Shift+Enter, Shift+F3)")
                        .clicked()
                    {
                        want_prev = true;
                    }
                    if ui
                        .add_enabled(nav_enabled, egui::Button::new("▼").small())
                        .on_hover_text("Next match (Enter, F3)")
                        .clicked()
                    {
                        want_next = true;
                    }
                    let mut cs = self.find.case_sensitive;
                    if ui
                        .toggle_value(&mut cs, "Aa")
                        .on_hover_text("Match case")
                        .changed()
                    {
                        self.find.case_sensitive = cs;
                        query_changed = true;
                    }
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui
                                .small_button("×")
                                .on_hover_text("Close find (Esc)")
                                .clicked()
                            {
                                want_close = true;
                            }
                        },
                    );
                });
            });

        if query_changed {
            self.recompute_find_matches();
            // Snap to the first match so the user sees something move as they
            // type. If their cursor was already inside a match they keep it,
            // otherwise jump to start.
            self.find.current = 0;
            self.find.scroll_pending = (!self.find.matches.is_empty()).then_some(0);
        }
        if want_next {
            self.find_next();
        }
        if want_prev {
            self.find_prev();
        }
        if want_close {
            self.close_find();
        }
    }

    /// Paint translucent overlay rects over every match, with the current
    /// match in a brighter colour. Called after the TextEdit has rendered
    /// (so we have its galley + screen position) but inside the same outer
    /// ScrollArea so the overlay scrolls with the text.
    pub(super) fn paint_find_overlays(
        &self,
        ui: &egui::Ui,
        output: &TextEditOutput,
    ) {
        if !self.find.open || self.find.matches.is_empty() {
            return;
        }
        let galley = &output.galley;
        let pos = output.galley_pos;
        let painter = ui.painter_at(output.text_clip_rect);

        // Colours tuned to read on both light + dark themes — yellow tinted
        // toward orange for the active match (same idea VS Code / Sublime use).
        let normal = egui::Color32::from_rgba_unmultiplied(255, 215, 0, 70);
        let active = egui::Color32::from_rgba_unmultiplied(255, 140, 0, 140);

        for (i, m) in self.find.matches.iter().enumerate() {
            let s = galley.from_ccursor(CCursor::new(m.start));
            let e = galley.from_ccursor(CCursor::new(m.end));
            let s_rect = galley.pos_from_cursor(&s);
            let e_rect = galley.pos_from_cursor(&e);
            let color = if i == self.find.current { active } else { normal };

            // Single-row match (the common case): one rect from start.x to
            // end.x. Multi-row matches degrade to a thin marker on the start
            // row — search queries that span lines are rare enough not to
            // warrant the row-by-row clipping code.
            if (s_rect.min.y - e_rect.min.y).abs() < 0.5 {
                let r = egui::Rect::from_min_max(
                    egui::pos2(s_rect.min.x + pos.x, s_rect.min.y + pos.y),
                    egui::pos2(e_rect.max.x + pos.x, s_rect.max.y + pos.y),
                );
                painter.rect_filled(r, 1.0, color);
            } else {
                let r = egui::Rect::from_min_max(
                    egui::pos2(s_rect.min.x + pos.x, s_rect.min.y + pos.y),
                    egui::pos2(s_rect.min.x + 4.0 + pos.x, s_rect.max.y + pos.y),
                );
                painter.rect_filled(r, 1.0, color);
            }
        }
    }

    /// If a scroll target is pending, scroll the outer ScrollArea so the
    /// match comes into view and update the editor's stored cursor to the
    /// match range — the latter so closing the find bar drops the user at a
    /// useful caret position. Must be called inside the editor's enclosing
    /// ScrollArea closure so `ui.scroll_to_rect` targets the right area.
    pub(super) fn drive_find_scroll(
        &mut self,
        ui: &mut egui::Ui,
        editor_id: egui::Id,
        output: &TextEditOutput,
    ) {
        let Some(idx) = self.find.scroll_pending.take() else {
            return;
        };
        let Some(m) = self.find.matches.get(idx).cloned() else {
            return;
        };

        let galley = &output.galley;
        let cur = galley.from_ccursor(CCursor::new(m.start));
        let r = galley.pos_from_cursor(&cur);
        // Pad horizontally so a match near the right edge isn't right-flush
        // against the scroll edge — gives the user some context.
        let pad_x = 80.0;
        let abs = egui::Rect::from_min_max(
            egui::pos2(r.min.x + output.galley_pos.x - pad_x, r.min.y + output.galley_pos.y),
            egui::pos2(
                r.max.x + output.galley_pos.x + pad_x,
                r.max.y + output.galley_pos.y,
            ),
        );
        ui.scroll_to_rect(abs, Some(egui::Align::Center));

        // Drop the editor's stored cursor on the match so that closing the
        // find bar (and re-focusing the editor) lands the caret at a useful
        // position. We don't request focus here — the find input keeps it.
        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), editor_id) {
            use egui::text::{CCursor as C, CCursorRange};
            st.cursor.set_char_range(Some(CCursorRange::two(
                C::new(m.start),
                C::new(m.end),
            )));
            st.store(ui.ctx(), editor_id);
        }
    }
}

