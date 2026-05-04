//! VS Code–style multi-cursor extension to the code editor.
//!
//! The TextEdit owns one primary cursor; this module layers an additional
//! `Vec<CaretRange>` on top so Cmd+D can build a fan-out selection set. When
//! the user types, deletes, copies, cuts, or pastes, every range receives
//! the same edit. Ranges are stored as char indices and sorted low → high
//! (with the primary slotted in at runtime so we never store it twice).
//!
//! Bindings (extras-active mode):
//!   - typing / Backspace / Delete / Enter   → fan-out edit
//!   - Cmd+C / Cmd+X / Cmd+V                 → fan-out clipboard ops
//!   - Esc                                   → clear extras
//!   - mouse click, arrow nav, other shortcut → clear extras, pass through
//!
//! Pure mutation only — repaint, dirty flagging, and recompile scheduling
//! happen in the editor render path that calls into this module.

use eframe::egui;
use egui::text::{CCursor, CCursorRange};
use egui::widgets::text_edit::TextEditOutput;
use egui::{Event, Key};

use super::types::CaretRange;
use super::MogenStudioApp;

impl MogenStudioApp {
    pub(super) fn clear_multi_caret(&mut self) {
        let i = self.active;
        if !self.files[i].extra_carets.is_empty() {
            self.files[i].extra_carets.clear();
        }
    }

    /// Drop any extra range that no longer fits inside the source. Called
    /// before every read so out-of-band edits (gizmo, inspector, LLM, undo)
    /// can't leave stale indices behind. The remaining ranges may still point
    /// at text that has since changed character-for-character — a tolerable
    /// degradation that the user can resolve with Esc.
    pub(super) fn prune_invalid_extras(&mut self) {
        let i = self.active;
        if self.files[i].extra_carets.is_empty() {
            return;
        }
        let total = self.files[i].source.chars().count();
        self.files[i].extra_carets.retain(|r| r.hi <= total);
    }

    /// Push the current TextEdit primary into the extras list and advance the
    /// primary to the next occurrence of the selected text after the caret.
    /// Wraps if nothing is found below the caret. Returns `true` when a new
    /// occurrence was added (caller wires the primary cursor + repaint).
    pub(super) fn multi_caret_add_next(
        &mut self,
        ctx: &egui::Context,
        editor_id: egui::Id,
    ) -> bool {
        let Some(state) = egui::TextEdit::load_state(ctx, editor_id) else {
            return false;
        };
        let Some(range) = state.cursor.char_range() else {
            return false;
        };
        let primary_lo = range.primary.index.min(range.secondary.index);
        let primary_hi = range.primary.index.max(range.secondary.index);
        if primary_hi == primary_lo {
            return false;
        }

        let i = self.active;
        let chars: Vec<char> = self.files[i].source.chars().collect();
        if primary_hi > chars.len() {
            return false;
        }
        let needle: Vec<char> = chars[primary_lo..primary_hi].to_vec();
        let n = needle.len();
        if n == 0 || needle.contains(&'\n') {
            return false;
        }
        if chars.len() < n {
            return false;
        }
        let last_start = chars.len() - n;

        // Existing positions to skip when scanning, in source-char order.
        let mut occupied: Vec<(usize, usize)> = self.files[i]
            .extra_carets
            .iter()
            .map(|r| (r.lo, r.hi))
            .collect();
        occupied.push((primary_lo, primary_hi));

        // Search after the latest cursor first, then wrap from the start.
        let scan_after = primary_hi..=last_start;
        let scan_wrap = 0..primary_hi.min(last_start + 1);
        let mut found: Option<(usize, usize)> = None;
        for start in scan_after.chain(scan_wrap) {
            if start + n > chars.len() {
                continue;
            }
            if chars[start..start + n] != needle[..] {
                continue;
            }
            let cand = (start, start + n);
            // Skip exact duplicates of an existing range. Touching is fine
            // (the new selection just abuts an existing one).
            if occupied.iter().any(|(lo, hi)| cand.0 < *hi && cand.1 > *lo) {
                continue;
            }
            found = Some(cand);
            break;
        }

        let Some((new_lo, new_hi)) = found else {
            return false;
        };

        self.files[i]
            .extra_carets
            .push(CaretRange::new(primary_lo, primary_hi));
        sort_dedupe(&mut self.files[i].extra_carets);

        let mut st = state;
        st.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(new_lo),
            CCursor::new(new_hi),
        )));
        st.store(ctx, editor_id);
        true
    }

    /// Intercept text-affecting events when multi-cursor extras are active.
    /// Returns `true` if the source string was mutated (caller flags dirty +
    /// recompile). Always runs before the TextEdit so consumed events never
    /// reach it.
    pub(super) fn handle_multi_caret_events(
        &mut self,
        ui: &egui::Ui,
        editor_id: egui::Id,
    ) -> bool {
        let i = self.active;
        if self.files[i].extra_carets.is_empty() {
            return false;
        }
        if !ui.ctx().memory(|m| m.has_focus(editor_id)) {
            // Lost focus → drop the multi-caret state so a stray click into a
            // dialog doesn't leave stale overlays painted forever.
            self.clear_multi_caret();
            return false;
        }

        let Some(state) = egui::TextEdit::load_state(ui.ctx(), editor_id) else {
            return false;
        };
        let Some(range) = state.cursor.char_range() else {
            return false;
        };
        let primary = CaretRange::new(
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );

        enum Action {
            InsertText(String),
            Backspace,
            Delete,
            Enter,
            Copy,
            Cut,
            Paste(String),
            Clear,
        }
        let mut actions: Vec<Action> = Vec::new();
        ui.ctx().input_mut(|inp| {
            let mut keep = Vec::with_capacity(inp.events.len());
            for ev in inp.events.drain(..) {
                match &ev {
                    Event::Text(t) => actions.push(Action::InsertText(t.clone())),
                    Event::Key {
                        key: Key::Enter,
                        pressed: true,
                        ..
                    } => actions.push(Action::Enter),
                    Event::Key {
                        key: Key::Backspace,
                        pressed: true,
                        ..
                    } => actions.push(Action::Backspace),
                    Event::Key {
                        key: Key::Delete,
                        pressed: true,
                        ..
                    } => actions.push(Action::Delete),
                    Event::Key {
                        key: Key::Escape,
                        pressed: true,
                        ..
                    } => actions.push(Action::Clear),
                    Event::Copy => actions.push(Action::Copy),
                    Event::Cut => actions.push(Action::Cut),
                    Event::Paste(s) => actions.push(Action::Paste(s.clone())),
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        // Arrow nav, Tab, Home/End, etc. drop us out of
                        // multi-cursor mode but still pass through so the
                        // TextEdit can handle the primary on its own.
                        if is_navigation_key(*key) {
                            actions.push(Action::Clear);
                        }
                        // Cmd+A — let TextEdit do select-all and clear extras.
                        // Cmd+Z / Cmd+Y / Cmd+D / Cmd+C / Cmd+X / Cmd+V are
                        // either handled by the multi-caret path itself or by
                        // their own line-op handler.
                        if modifiers.command
                            && !matches!(
                                key,
                                Key::C | Key::X | Key::V | Key::D | Key::A | Key::Z | Key::Y
                            )
                        {
                            actions.push(Action::Clear);
                        }
                        if *key == Key::A && modifiers.command {
                            actions.push(Action::Clear);
                        }
                        keep.push(ev);
                    }
                    Event::PointerButton { pressed: true, .. } => {
                        actions.push(Action::Clear);
                        keep.push(ev);
                    }
                    _ => keep.push(ev),
                }
            }
            inp.events = keep;
        });

        if actions.is_empty() {
            return false;
        }

        let mut mutated = false;
        let mut current_primary = primary;
        let mut current_extras = std::mem::take(&mut self.files[i].extra_carets);

        for action in actions {
            match action {
                Action::Clear => {
                    current_extras.clear();
                }
                Action::Copy => {
                    let text = collect_selections_text(
                        &self.files[i].source,
                        &current_extras,
                        current_primary,
                    );
                    if !text.is_empty() {
                        ui.ctx().copy_text(text);
                    }
                }
                Action::Cut => {
                    let text = collect_selections_text(
                        &self.files[i].source,
                        &current_extras,
                        current_primary,
                    );
                    if !text.is_empty() {
                        ui.ctx().copy_text(text);
                    }
                    let edits =
                        build_edits_replace(&current_extras, current_primary, |_| String::new());
                    let (new_src, new_primary, new_extras) =
                        apply_edits(&self.files[i].source, edits);
                    self.files[i].source = new_src;
                    current_primary = new_primary;
                    current_extras = new_extras;
                    mutated = true;
                }
                Action::Paste(text) => {
                    let chunks = split_for_paste(&text, current_extras.len() + 1);
                    let edits = build_edits_paste(&current_extras, current_primary, &chunks);
                    let (new_src, new_primary, new_extras) =
                        apply_edits(&self.files[i].source, edits);
                    self.files[i].source = new_src;
                    current_primary = new_primary;
                    current_extras = new_extras;
                    mutated = true;
                }
                Action::InsertText(t) => {
                    let edits =
                        build_edits_replace(&current_extras, current_primary, |_| t.clone());
                    let (new_src, new_primary, new_extras) =
                        apply_edits(&self.files[i].source, edits);
                    self.files[i].source = new_src;
                    current_primary = new_primary;
                    current_extras = new_extras;
                    mutated = true;
                }
                Action::Enter => {
                    let edits =
                        build_edits_replace(&current_extras, current_primary, |_| "\n".into());
                    let (new_src, new_primary, new_extras) =
                        apply_edits(&self.files[i].source, edits);
                    self.files[i].source = new_src;
                    current_primary = new_primary;
                    current_extras = new_extras;
                    mutated = true;
                }
                Action::Backspace => {
                    let edits = build_edits_backspace(&current_extras, current_primary);
                    let (new_src, new_primary, new_extras) =
                        apply_edits(&self.files[i].source, edits);
                    self.files[i].source = new_src;
                    current_primary = new_primary;
                    current_extras = new_extras;
                    mutated = true;
                }
                Action::Delete => {
                    let edits =
                        build_edits_delete(&self.files[i].source, &current_extras, current_primary);
                    let (new_src, new_primary, new_extras) =
                        apply_edits(&self.files[i].source, edits);
                    self.files[i].source = new_src;
                    current_primary = new_primary;
                    current_extras = new_extras;
                    mutated = true;
                }
            }
        }

        // Re-sync TextEdit state with the new primary position. Even when
        // nothing mutated (e.g. Esc / Copy alone) we may have collapsed extras
        // and need to leave the primary where it was.
        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), editor_id) {
            let new_range = if current_primary.is_caret() {
                CCursorRange::one(CCursor::new(current_primary.lo))
            } else {
                CCursorRange::two(
                    CCursor::new(current_primary.lo),
                    CCursor::new(current_primary.hi),
                )
            };
            st.cursor.set_char_range(Some(new_range));
            st.store(ui.ctx(), editor_id);
        }

        sort_dedupe(&mut current_extras);
        // Drop any extra that has collapsed onto the primary — happens after
        // a fan-out edit when adjacent ranges meet at the boundary.
        current_extras
            .retain(|r| !(r.lo == current_primary.lo && r.hi == current_primary.hi));

        self.files[i].extra_carets = current_extras;
        mutated
    }

    /// Paint extra-selection overlays after the TextEdit has rendered. Mirrors
    /// `paint_find_overlays` but uses the visuals' selection colour so the
    /// extras visually match a real TextEdit selection.
    pub(super) fn paint_multi_caret_overlays(
        &self,
        ui: &egui::Ui,
        output: &TextEditOutput,
    ) {
        let i = self.active;
        if self.files[i].extra_carets.is_empty() {
            return;
        }
        let galley = &output.galley;
        let pos = output.galley_pos;
        let painter = ui.painter_at(output.text_clip_rect);
        let visuals = &ui.style().visuals;
        let sel_color = visuals.selection.bg_fill.gamma_multiply(0.6);
        let caret_color = visuals.text_cursor.stroke.color;

        for r in &self.files[i].extra_carets {
            let s = galley.from_ccursor(CCursor::new(r.lo));
            let e = galley.from_ccursor(CCursor::new(r.hi));
            let s_rect = galley.pos_from_cursor(&s);
            let e_rect = galley.pos_from_cursor(&e);

            if r.is_caret() {
                let rect = egui::Rect::from_min_max(
                    egui::pos2(s_rect.min.x + pos.x - 0.5, s_rect.min.y + pos.y),
                    egui::pos2(s_rect.min.x + pos.x + 1.0, s_rect.max.y + pos.y),
                );
                painter.rect_filled(rect, 0.0, caret_color);
                continue;
            }

            // Single-row selection — one rect. Multi-row degrades to a marker
            // on the first row; Cmd+D picks word-shaped selections so this is
            // a tiny corner-case in practice.
            if (s_rect.min.y - e_rect.min.y).abs() < 0.5 {
                let rect = egui::Rect::from_min_max(
                    egui::pos2(s_rect.min.x + pos.x, s_rect.min.y + pos.y),
                    egui::pos2(e_rect.max.x + pos.x, s_rect.max.y + pos.y),
                );
                painter.rect_filled(rect, 1.0, sel_color);
            } else {
                let rect = egui::Rect::from_min_max(
                    egui::pos2(s_rect.min.x + pos.x, s_rect.min.y + pos.y),
                    egui::pos2(s_rect.min.x + 4.0 + pos.x, s_rect.max.y + pos.y),
                );
                painter.rect_filled(rect, 1.0, sel_color);
            }
        }
    }
}

fn is_navigation_key(k: Key) -> bool {
    matches!(
        k,
        Key::ArrowUp
            | Key::ArrowDown
            | Key::ArrowLeft
            | Key::ArrowRight
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
    )
}

fn sort_dedupe(ranges: &mut Vec<CaretRange>) {
    ranges.sort_by_key(|r| (r.lo, r.hi));
    ranges.dedup();
}

fn collect_selections_text(source: &str, extras: &[CaretRange], primary: CaretRange) -> String {
    let mut all: Vec<CaretRange> = extras.to_vec();
    all.push(primary);
    all.sort_by_key(|r| r.lo);
    let mut out = String::new();
    let mut wrote_anything = false;
    for r in all.iter() {
        if r.is_caret() {
            continue;
        }
        if wrote_anything && !out.ends_with('\n') {
            out.push('\n');
        }
        let s: String = source.chars().skip(r.lo).take(r.len()).collect();
        out.push_str(&s);
        wrote_anything = true;
    }
    out
}

#[derive(Clone, Copy)]
enum EditOwner {
    Primary,
    Extra(usize),
}

struct PendingEdit {
    /// Char range in the original source (lo <= hi).
    lo: usize,
    hi: usize,
    /// Replacement text.
    text: String,
    /// Which selection this edit was generated for. Drives where the
    /// post-edit caret lands.
    owner: EditOwner,
}

/// Splice every edit into `source`. Edits are sorted by `lo` (asc) and must be
/// non-overlapping; the resulting caret for each owner is placed at the END of
/// that edit's inserted text (collapsed selection — same convention VS Code
/// uses after typing into a multi-selection).
fn apply_edits(source: &str, mut edits: Vec<PendingEdit>) -> (String, CaretRange, Vec<CaretRange>) {
    edits.sort_by_key(|e| e.lo);

    let extras_count = edits
        .iter()
        .filter(|e| matches!(e.owner, EditOwner::Extra(_)))
        .map(|e| match e.owner {
            EditOwner::Extra(j) => j + 1,
            _ => 0,
        })
        .max()
        .unwrap_or(0);

    let mut new_primary = CaretRange::caret(0);
    let mut new_extras: Vec<CaretRange> = vec![CaretRange::caret(0); extras_count];

    let chars_to_byte = build_char_to_byte(source);
    let mut out = String::with_capacity(source.len());
    let mut prev_byte = 0usize;
    let mut out_chars_so_far = 0usize;

    for e in &edits {
        let lo_byte = char_to_byte_with(&chars_to_byte, source, e.lo);
        let hi_byte = char_to_byte_with(&chars_to_byte, source, e.hi);
        let between = &source[prev_byte..lo_byte];
        out.push_str(between);
        out_chars_so_far += between.chars().count();
        out.push_str(&e.text);
        let inserted_chars = e.text.chars().count();
        out_chars_so_far += inserted_chars;
        let caret_at = out_chars_so_far;
        let new_range = CaretRange::caret(caret_at);
        match e.owner {
            EditOwner::Primary => {
                new_primary = new_range;
            }
            EditOwner::Extra(j) => {
                new_extras[j] = new_range;
            }
        }
        prev_byte = hi_byte;
    }
    out.push_str(&source[prev_byte..]);

    (out, new_primary, new_extras)
}

fn build_edits_replace<F>(
    extras: &[CaretRange],
    primary: CaretRange,
    mut text_for: F,
) -> Vec<PendingEdit>
where
    F: FnMut(usize) -> String,
{
    let mut edits: Vec<PendingEdit> = Vec::with_capacity(extras.len() + 1);
    edits.push(PendingEdit {
        lo: primary.lo,
        hi: primary.hi,
        text: text_for(0),
        owner: EditOwner::Primary,
    });
    for (idx, r) in extras.iter().enumerate() {
        edits.push(PendingEdit {
            lo: r.lo,
            hi: r.hi,
            text: text_for(idx + 1),
            owner: EditOwner::Extra(idx),
        });
    }
    edits
}

fn build_edits_paste(
    extras: &[CaretRange],
    primary: CaretRange,
    chunks: &[String],
) -> Vec<PendingEdit> {
    // Pair each range with its source-order chunk. Build a sorted list of
    // (range, owner) so chunks land top-to-bottom of the document, then map
    // the chunk index by sorted position.
    let mut all: Vec<(CaretRange, EditOwner)> = Vec::with_capacity(extras.len() + 1);
    all.push((primary, EditOwner::Primary));
    for (idx, r) in extras.iter().enumerate() {
        all.push((*r, EditOwner::Extra(idx)));
    }
    all.sort_by_key(|(r, _)| r.lo);
    let fallback = chunks.last().cloned().unwrap_or_default();
    all.into_iter()
        .enumerate()
        .map(|(i, (r, owner))| PendingEdit {
            lo: r.lo,
            hi: r.hi,
            text: chunks.get(i).cloned().unwrap_or_else(|| fallback.clone()),
            owner,
        })
        .collect()
}

fn split_for_paste(text: &str, expected: usize) -> Vec<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() == expected {
        lines.into_iter().map(|s| s.to_string()).collect()
    } else {
        // Mismatched line count — paste the full text at every cursor (VS
        // Code's fallback behaviour).
        std::iter::repeat_n(text.to_string(), expected).collect()
    }
}

fn build_edits_backspace(extras: &[CaretRange], primary: CaretRange) -> Vec<PendingEdit> {
    let mut edits: Vec<PendingEdit> = Vec::with_capacity(extras.len() + 1);
    let mut push_one = |r: CaretRange, owner: EditOwner| {
        if r.is_caret() {
            if r.lo == 0 {
                return;
            }
            edits.push(PendingEdit {
                lo: r.lo - 1,
                hi: r.lo,
                text: String::new(),
                owner,
            });
        } else {
            edits.push(PendingEdit {
                lo: r.lo,
                hi: r.hi,
                text: String::new(),
                owner,
            });
        }
    };
    push_one(primary, EditOwner::Primary);
    for (idx, r) in extras.iter().enumerate() {
        push_one(*r, EditOwner::Extra(idx));
    }
    edits
}

fn build_edits_delete(
    source: &str,
    extras: &[CaretRange],
    primary: CaretRange,
) -> Vec<PendingEdit> {
    let mut edits: Vec<PendingEdit> = Vec::with_capacity(extras.len() + 1);
    let total_chars = source.chars().count();
    let mut push_one = |r: CaretRange, owner: EditOwner| {
        if r.is_caret() {
            if r.hi >= total_chars {
                return;
            }
            edits.push(PendingEdit {
                lo: r.hi,
                hi: r.hi + 1,
                text: String::new(),
                owner,
            });
        } else {
            edits.push(PendingEdit {
                lo: r.lo,
                hi: r.hi,
                text: String::new(),
                owner,
            });
        }
    };
    push_one(primary, EditOwner::Primary);
    for (idx, r) in extras.iter().enumerate() {
        push_one(*r, EditOwner::Extra(idx));
    }
    edits
}

fn build_char_to_byte(s: &str) -> Vec<usize> {
    let mut v = Vec::with_capacity(s.len() + 1);
    for (i, _) in s.char_indices() {
        v.push(i);
    }
    v.push(s.len());
    v
}

fn char_to_byte_with(table: &[usize], s: &str, char_idx: usize) -> usize {
    if char_idx >= table.len() {
        s.len()
    } else {
        table[char_idx]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cr(lo: usize, hi: usize) -> CaretRange {
        CaretRange::new(lo, hi)
    }

    #[test]
    fn insert_fan_out_replaces_each_selection() {
        let src = "foo bar foo";
        let primary = cr(0, 3);
        let extras = vec![cr(8, 11)];
        let edits = build_edits_replace(&extras, primary, |_| "X".to_string());
        let (out, p, e) = apply_edits(src, edits);
        assert_eq!(out, "X bar X");
        // Selection collapses to caret AFTER inserted text.
        assert_eq!(p, cr(1, 1));
        assert_eq!(e[0], cr(7, 7));
    }

    #[test]
    fn insert_at_caret_advances_each() {
        let src = "ab\ncd\nef";
        let primary = cr(0, 0);
        let extras = vec![cr(3, 3), cr(6, 6)];
        let edits = build_edits_replace(&extras, primary, |_| "//".to_string());
        let (out, p, e) = apply_edits(src, edits);
        assert_eq!(out, "//ab\n//cd\n//ef");
        assert_eq!(p, cr(2, 2));
        assert_eq!(e[0], cr(7, 7));
        assert_eq!(e[1], cr(12, 12));
    }

    #[test]
    fn backspace_at_caret_eats_one_char() {
        let src = "abc def";
        let primary = cr(3, 3);
        let extras = vec![cr(7, 7)];
        let edits = build_edits_backspace(&extras, primary);
        let (out, p, e) = apply_edits(src, edits);
        assert_eq!(out, "ab de");
        assert_eq!(p, cr(2, 2));
        assert_eq!(e[0], cr(5, 5));
    }

    #[test]
    fn delete_at_eof_is_noop_for_that_caret() {
        let src = "ab";
        let primary = cr(2, 2);
        let extras = vec![cr(0, 0)];
        let edits = build_edits_delete(src, &extras, primary);
        let (out, _p, e) = apply_edits(src, edits);
        // Only the caret at 0 deletes 'a'; primary at EOF was a noop.
        assert_eq!(out, "b");
        assert_eq!(e[0], cr(0, 0));
    }

    #[test]
    fn copy_collects_selections_in_order() {
        let src = "alpha beta gamma";
        let primary = cr(11, 16); // gamma
        let extras = vec![cr(0, 5), cr(6, 10)]; // alpha, beta
        let text = collect_selections_text(src, &extras, primary);
        assert_eq!(text, "alpha\nbeta\ngamma");
    }

    #[test]
    fn paste_distributes_lines_when_count_matches() {
        let chunks = split_for_paste("one\ntwo\nthree", 3);
        assert_eq!(chunks, vec!["one", "two", "three"]);
    }

    #[test]
    fn paste_falls_back_to_full_text_when_count_differs() {
        let chunks = split_for_paste("one\ntwo", 3);
        assert_eq!(chunks, vec!["one\ntwo", "one\ntwo", "one\ntwo"]);
    }

    #[test]
    fn paste_lines_land_top_to_bottom() {
        let src = "AAA\nBBB\nCCC";
        let primary = cr(8, 11); // CCC
        let extras = vec![cr(0, 3), cr(4, 7)]; // AAA, BBB
        let chunks = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        let edits = build_edits_paste(&extras, primary, &chunks);
        let (out, _, _) = apply_edits(src, edits);
        assert_eq!(out, "one\ntwo\nthree");
    }
}
