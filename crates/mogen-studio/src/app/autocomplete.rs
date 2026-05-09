//! Glue between the pure [`crate::autocomplete`] provider and the editor panel.
//!
//! `handle_autocomplete_keys` runs before the TextEdit so the popup can swallow
//! Up/Down/Tab/Enter/Esc while visible. `update_autocomplete_after_textedit`
//! inspects the widget output to refresh the candidate list and compute the
//! popup's screen anchor. `render_autocomplete_popup` paints the floating list
//! and accepts mouse clicks. Accept logic in `accept_autocomplete` splices the
//! selected candidate into the active file's source and rewrites the TextEdit
//! cursor state.

use std::time::{Duration, Instant};

use eframe::egui;
use egui::text::{CCursor, CCursorRange};

use super::types::{AutocompleteKey, AutocompleteState};
use super::MogenStudioApp;
use crate::autocomplete::{compute_completions, Candidate, CandidateKind};

impl MogenStudioApp {
    /// Peek at the current frame's keyboard input and decode a popup action.
    /// Keys are consumed (so the underlying TextEdit won't also react) only
    /// when the popup is open.
    pub(super) fn autocomplete_key(&mut self, ui: &egui::Ui) -> AutocompleteKey {
        if !self.autocomplete.open {
            return AutocompleteKey::None;
        }
        let mut action = AutocompleteKey::None;
        ui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                action = AutocompleteKey::Cancel;
                return;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                action = AutocompleteKey::MoveDown;
                return;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                action = AutocompleteKey::MoveUp;
                return;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            {
                action = AutocompleteKey::Accept;
            }
        });
        action
    }

    /// After the TextEdit has painted, use its `cursor_range` + `galley` to
    /// refresh the candidate list and anchor position. Also processes the
    /// deferred `key_action` captured before the widget rendered.
    pub(super) fn update_autocomplete_after_textedit(
        &mut self,
        ui: &egui::Ui,
        output: &egui::widgets::text_edit::TextEditOutput,
        editor_id: egui::Id,
        key_action: AutocompleteKey,
    ) {
        // Editor focus lost? Close the popup — otherwise it would linger over
        // other UI as the user tabs away.
        let focused = ui.ctx().memory(|m| m.has_focus(editor_id));
        if !focused {
            self.autocomplete.close();
            return;
        }

        let Some(cursor_range) = output.cursor_range else {
            self.autocomplete.close();
            return;
        };
        // Char index of the primary cursor → byte offset into the buffer.
        let source = &self.files[self.active].source;
        let char_idx = cursor_range.primary.ccursor.index;
        let caret = source
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(source.len());

        // Esc-to-suppress: keep the popup hidden for a short window so the
        // user can finish typing without it popping back. Time-based rather
        // than source-length based — deleting and re-typing the same char
        // used to silently keep the popup hidden because lengths matched.
        if let Some(deadline) = self.autocomplete.suppressed_until {
            if Instant::now() >= deadline {
                self.autocomplete.suppressed_until = None;
            }
        }

        // Compute completions for the current caret and known material names
        // (so `mat=` can suggest declared materials).
        let mats: Vec<String> = self
            .files
            .get(self.active)
            .and_then(|f| f.last_result.as_ref())
            .and_then(|r| r.scene.as_ref())
            .map(|s| s.materials.iter().map(|m| m.name.clone()).collect())
            .unwrap_or_default();

        let completions = if self.autocomplete.suppressed_until.is_some() {
            None
        } else {
            compute_completions(source, caret, &mats)
        };

        let mut should_accept = false;
        match (completions, key_action) {
            (Some(c), action) => {
                // Reset `selected` to 0 whenever the candidate set changes
                // so the top match is always preselected. If it's the same
                // set, just clamp the existing index to the new bounds.
                let signature = candidate_signature(&c.candidates);
                let prev_selected = if Some(signature) == self.autocomplete.last_signature
                    && self.autocomplete.open
                {
                    self.autocomplete
                        .selected
                        .min(c.candidates.len().saturating_sub(1))
                } else {
                    0
                };
                self.autocomplete.open = true;
                self.autocomplete.candidates = c.candidates;
                self.autocomplete.range = Some(c.range);
                self.autocomplete.selected = prev_selected;
                self.autocomplete.last_signature = Some(signature);

                match action {
                    AutocompleteKey::MoveDown => {
                        let n = self.autocomplete.candidates.len();
                        if n > 0 {
                            self.autocomplete.selected =
                                (self.autocomplete.selected + 1) % n;
                        }
                    }
                    AutocompleteKey::MoveUp => {
                        let n = self.autocomplete.candidates.len();
                        if n > 0 {
                            self.autocomplete.selected =
                                (self.autocomplete.selected + n - 1) % n;
                        }
                    }
                    AutocompleteKey::Accept => {
                        should_accept = true;
                    }
                    AutocompleteKey::Cancel => {
                        // 600 ms is long enough that the next keystroke
                        // doesn't immediately re-pop, short enough that
                        // pausing to think then typing again works as
                        // expected.
                        self.autocomplete.suppressed_until =
                            Some(Instant::now() + Duration::from_millis(600));
                        self.autocomplete.close();
                    }
                    AutocompleteKey::None => {}
                }
            }
            (None, _) => {
                self.autocomplete.close();
            }
        }

        // Compute the popup anchor as the bottom-left of the caret glyph in
        // screen space. `pos_from_cursor` returns a rect local to the galley.
        if self.autocomplete.open {
            let cursor_rect = output.galley.pos_from_cursor(&cursor_range.primary);
            let anchor = output.galley_pos + cursor_rect.left_bottom().to_vec2();
            self.autocomplete.anchor = Some(anchor);
        }

        if should_accept {
            self.accept_autocomplete(ui, editor_id);
        }
    }

    /// Draw the candidate list below the caret. Returns `true` when a mouse
    /// click selected a candidate — the caller then splices source.
    pub(super) fn render_autocomplete_popup(
        &mut self,
        ctx: &egui::Context,
        editor_id: egui::Id,
    ) -> bool {
        if !self.autocomplete.open {
            return false;
        }
        let Some(anchor) = self.autocomplete.anchor else {
            return false;
        };
        if self.autocomplete.candidates.is_empty() {
            return false;
        }

        let mut clicked: Option<usize> = None;
        let selected = self.autocomplete.selected;
        let candidates = self.autocomplete.candidates.clone();

        egui::Area::new(egui::Id::new("mog_autocomplete_popup"))
            // Offset a few pixels so the popup doesn't overlap the caret glyph.
            .fixed_pos(anchor + egui::vec2(0.0, 2.0))
            .order(egui::Order::Foreground)
            // The popup shouldn't steal focus from the editor; without this
            // a click on it would cancel the TextEdit selection before we
            // had a chance to read it.
            .interactable(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::symmetric(4.0, 4.0))
                    .show(ui, |ui| {
                        ui.set_max_width(320.0);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        for (i, c) in candidates.iter().enumerate() {
                            if render_item(ui, c, i == selected) {
                                clicked = Some(i);
                            }
                        }
                    });
            });

        if let Some(idx) = clicked {
            self.autocomplete.selected = idx;
            // Give focus back to the editor so the caret move lands on the
            // right widget, then splice.
            ctx.memory_mut(|m| m.request_focus(editor_id));
            // We don't have an `&Ui` here, but `accept_autocomplete` only
            // needs the context via `ctx.memory_mut` internally.
            self.accept_autocomplete_ctx(ctx, editor_id);
            // Mark source as user-edited so the debounced recompile fires.
            self.files[self.active].dirty = self.files[self.active].source
                != self.files[self.active].last_saved_source;
            self.files[self.active].needs_compile = true;
            self.files[self.active].last_edit_at = Some(Instant::now());
            return true;
        }
        false
    }

    /// Splice the selected candidate into the active file's source and move
    /// the caret to the end of the inserted text. Wraps `accept_autocomplete_ctx`
    /// for the keyboard-accept path where an `&Ui` is handy.
    fn accept_autocomplete(&mut self, ui: &egui::Ui, editor_id: egui::Id) {
        self.accept_autocomplete_ctx(ui.ctx(), editor_id);
        self.files[self.active].dirty = self.files[self.active].source
            != self.files[self.active].last_saved_source;
        self.files[self.active].needs_compile = true;
        self.files[self.active].last_edit_at = Some(Instant::now());
    }

    fn accept_autocomplete_ctx(&mut self, ctx: &egui::Context, editor_id: egui::Id) {
        let Some(range) = self.autocomplete.range.clone() else {
            self.autocomplete.close();
            return;
        };
        let idx = self.autocomplete.selected;
        let Some(candidate) = self.autocomplete.candidates.get(idx).cloned() else {
            self.autocomplete.close();
            return;
        };
        let i = self.active;
        let source = &mut self.files[i].source;
        if range.end > source.len() || range.start > range.end {
            self.autocomplete.close();
            return;
        }
        source.replace_range(range.start..range.end, &candidate.insert);

        // Move the caret to end-of-insert.
        let new_byte = range.start + candidate.insert.len();
        let clamped = new_byte.min(source.len());
        let new_char = source[..clamped].chars().count();
        if let Some(mut st) = egui::TextEdit::load_state(ctx, editor_id) {
            st.cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(new_char))));
            st.store(ctx, editor_id);
        }
        // Suppress for a short window after accepting so the popup doesn't
        // immediately re-open on the just-inserted word.
        self.autocomplete.suppressed_until =
            Some(Instant::now() + Duration::from_millis(600));
        self.autocomplete.close();
    }
}

impl AutocompleteState {
    pub(super) fn close(&mut self) {
        self.open = false;
        self.candidates.clear();
        self.range = None;
        self.anchor = None;
        self.selected = 0;
        self.last_signature = None;
    }
}

/// Cheap fingerprint of the candidate label set so we can detect when the
/// list has actually changed (and therefore reset the `selected` index back
/// to the top). Order-sensitive, which is what we want — same labels in a
/// different order are still a different list to the user.
fn candidate_signature(cands: &[Candidate]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cands.len().hash(&mut h);
    for c in cands {
        c.label.hash(&mut h);
    }
    h.finish()
}

fn render_item(ui: &mut egui::Ui, c: &Candidate, selected: bool) -> bool {
    let mut clicked = false;
    let tag = kind_tag(c.kind);
    let tag_color = kind_color(c.kind, ui.visuals());
    ui.horizontal(|ui| {
        let mut job = egui::text::LayoutJob::default();
        // Narrow, fixed-width tag column so the labels align.
        let tag_fmt = egui::TextFormat {
            color: tag_color,
            font_id: egui::TextStyle::Body.resolve(ui.style()),
            ..Default::default()
        };
        job.append(&format!("{tag:<5}"), 0.0, tag_fmt);
        let label_fmt = egui::TextFormat {
            color: ui.visuals().text_color(),
            font_id: egui::TextStyle::Monospace.resolve(ui.style()),
            ..Default::default()
        };
        job.append(&c.label, 0.0, label_fmt);
        if let Some(detail) = c.detail {
            let detail_fmt = egui::TextFormat {
                color: ui.visuals().weak_text_color(),
                font_id: egui::TextStyle::Body.resolve(ui.style()),
                ..Default::default()
            };
            job.append(&format!("   {detail}"), 0.0, detail_fmt);
        }
        let resp = ui.add(egui::SelectableLabel::new(selected, job));
        if resp.clicked() {
            clicked = true;
        }
    });
    clicked
}

fn kind_tag(k: CandidateKind) -> &'static str {
    match k {
        CandidateKind::NodeKind => "kind",
        CandidateKind::Attribute => "attr",
        CandidateKind::EnumValue => "enum",
        CandidateKind::Material => "mat",
    }
}

fn kind_color(k: CandidateKind, visuals: &egui::Visuals) -> egui::Color32 {
    if visuals.dark_mode {
        match k {
            CandidateKind::NodeKind => egui::Color32::from_rgb(86, 156, 214),
            CandidateKind::Attribute => egui::Color32::from_rgb(197, 134, 192),
            CandidateKind::EnumValue => egui::Color32::from_rgb(181, 206, 168),
            CandidateKind::Material => egui::Color32::from_rgb(206, 145, 120),
        }
    } else {
        match k {
            CandidateKind::NodeKind => egui::Color32::from_rgb(0, 0, 200),
            CandidateKind::Attribute => egui::Color32::from_rgb(128, 0, 128),
            CandidateKind::EnumValue => egui::Color32::from_rgb(9, 134, 88),
            CandidateKind::Material => egui::Color32::from_rgb(163, 21, 21),
        }
    }
}
