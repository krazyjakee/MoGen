//! Block-indent / block-dedent for the code editor's selection.
//!
//! Runs before the `TextEdit` paints so it can swallow Tab / Shift+Tab and
//! rewrite the source + cursor itself. Defaults are preserved when the action
//! doesn't apply: a single-line Tab still inserts a literal `\t`.

use eframe::egui;
use egui::text::{CCursor, CCursorRange};

use super::MogenStudioApp;

const SPACES_PER_TAB: usize = 4;

impl MogenStudioApp {
    /// Returns `true` when the source was mutated and the caller should mark
    /// the file dirty / schedule a recompile.
    pub(super) fn handle_indent_keys(
        &mut self,
        ui: &egui::Ui,
        editor_id: egui::Id,
    ) -> bool {
        // Autocomplete popup has first claim on Tab/Enter.
        if self.autocomplete.open {
            return false;
        }
        if !ui.ctx().memory(|m| m.has_focus(editor_id)) {
            return false;
        }

        let Some(state) = egui::TextEdit::load_state(ui.ctx(), editor_id) else {
            return false;
        };
        let Some(range) = state.cursor.char_range() else {
            return false;
        };

        let i = self.active;
        let source_ref = &self.files[i].source;
        let [lo, hi] = range.sorted();
        let lo_byte = char_to_byte(source_ref, lo.index);
        let hi_byte = char_to_byte(source_ref, hi.index);
        let multi_line = source_ref[lo_byte..hi_byte].contains('\n');
        let has_selection = hi_byte > lo_byte;

        // Tab is only intercepted for multi-line selections — otherwise the
        // default code-editor behaviour (insert / replace with `\t`) wins.
        // Shift+Tab is always handled (dedents the current line at minimum).
        //
        // Check Shift+Tab FIRST: egui's `consume_key` uses
        // `Modifiers::matches_logically`, which ignores extra modifiers — so
        // `consume_key(NONE, Tab)` happily matches a Shift+Tab event too. If
        // we matched the broader Tab pattern first on a multi-line selection,
        // Shift+Tab would silently re-indent instead of dedenting.
        let (tab, shift_tab) = ui.input_mut(|i| {
            let shift_tab = i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
            let tab = if multi_line && !shift_tab {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
            } else {
                false
            };
            (tab, shift_tab)
        });
        if !tab && !shift_tab {
            return false;
        }
        // Shift+Tab on a line with no leading whitespace is a no-op but we
        // still consume the key so focus doesn't escape the editor.
        let _ = has_selection;

        let first_line_start = source_ref[..lo_byte]
            .rfind('\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        let mut line_starts: Vec<usize> = vec![first_line_start];
        {
            let mut p = first_line_start;
            while let Some(off) = source_ref[p..].find('\n') {
                let next = p + off + 1;
                if next >= hi_byte {
                    break;
                }
                line_starts.push(next);
                p = next;
            }
        }

        let mut new_lo = lo_byte;
        let mut new_hi = hi_byte;
        let source = &mut self.files[i].source;

        let mutated = if tab {
            // Insert in reverse so earlier byte offsets stay valid.
            for &ls in line_starts.iter().rev() {
                source.insert(ls, '\t');
            }
            for &ls in &line_starts {
                if ls < new_lo {
                    new_lo += 1;
                }
                if ls < new_hi {
                    new_hi += 1;
                }
            }
            true
        } else {
            let removals: Vec<usize> = line_starts
                .iter()
                .map(|&ls| count_indent_removable(source.as_bytes(), ls))
                .collect();
            let any = removals.iter().any(|&n| n > 0);
            for (&ls, &n) in line_starts.iter().rev().zip(removals.iter().rev()) {
                if n > 0 {
                    source.replace_range(ls..ls + n, "");
                }
            }
            for (&ls, &n) in line_starts.iter().zip(removals.iter()) {
                if n == 0 {
                    continue;
                }
                if ls + n <= new_lo {
                    new_lo -= n;
                } else if ls < new_lo {
                    new_lo = ls;
                }
                if ls + n <= new_hi {
                    new_hi -= n;
                } else if ls < new_hi {
                    new_hi = ls;
                }
            }
            any
        };

        // Persist the new selection so the TextEdit (rendered immediately
        // after) picks it up. This runs even on a no-op shift+tab so the
        // cursor at least keeps focus.
        let src_now = &self.files[i].source;
        let new_lo_char = byte_to_char(src_now, new_lo);
        let new_hi_char = byte_to_char(src_now, new_hi);
        let primary_was_first = range.primary.index == lo.index;
        let new_range = if new_lo_char == new_hi_char {
            CCursorRange::one(CCursor::new(new_lo_char))
        } else if primary_was_first {
            CCursorRange {
                primary: CCursor::new(new_lo_char),
                secondary: CCursor::new(new_hi_char),
            }
        } else {
            CCursorRange {
                primary: CCursor::new(new_hi_char),
                secondary: CCursor::new(new_lo_char),
            }
        };
        let mut st = state;
        st.cursor.set_char_range(Some(new_range));
        st.store(ui.ctx(), editor_id);

        mutated
    }
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

fn byte_to_char(s: &str, byte_idx: usize) -> usize {
    s[..byte_idx.min(s.len())].chars().count()
}

fn count_indent_removable(bytes: &[u8], ls: usize) -> usize {
    if ls >= bytes.len() {
        return 0;
    }
    if bytes[ls] == b'\t' {
        return 1;
    }
    let mut n = 0;
    while n < SPACES_PER_TAB && ls + n < bytes.len() && bytes[ls + n] == b' ' {
        n += 1;
    }
    n
}
