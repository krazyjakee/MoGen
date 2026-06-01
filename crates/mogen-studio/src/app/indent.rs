//! Block-indent / block-dedent for the code editor's selection.
//!
//! Runs before the `TextEdit` paints so it can swallow Tab / Shift+Tab and
//! rewrite the source + cursor itself. Tab inserts two spaces (one indent
//! unit) rather than a literal `\t`: a multi-line selection indents every
//! covered line, while a caret or in-line selection inserts a single indent.

use eframe::egui;
use egui::text::{CCursor, CCursorRange};

use super::MogenStudioApp;

/// Width of one indent level, in spaces. Drives both the Tab insert and the
/// Shift+Tab dedent so the two stay symmetric.
const SPACES_PER_TAB: usize = 2;

/// One indent level as literal text. Inserted on Tab in place of egui's
/// default `\t` so the code surface stays space-indented.
const INDENT: &str = "  ";

// Compile-time guard: if someone changes one constant they must update both.
const _: () = assert!(INDENT.len() == SPACES_PER_TAB);

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

        // Tab is always intercepted so it inserts spaces instead of egui's
        // default literal `\t`: a multi-line selection block-indents, a caret
        // or in-line selection inserts a single indent unit. Shift+Tab is
        // likewise always handled (dedents the current line at minimum).
        //
        // Check Shift+Tab FIRST: egui's `consume_key` uses
        // `Modifiers::matches_logically`, which ignores extra modifiers — so
        // `consume_key(NONE, Tab)` happily matches a Shift+Tab event too. If
        // we matched the broader Tab pattern first, Shift+Tab would silently
        // re-indent instead of dedenting.
        let (tab, shift_tab) = ui.input_mut(|i| {
            let shift_tab = i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
            let tab = if !shift_tab {
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

        let source = &mut self.files[i].source;
        let (new_lo, new_hi, mutated) = if tab {
            apply_tab(source, lo_byte, hi_byte, &line_starts, multi_line)
        } else {
            apply_shift_tab(source, lo_byte, hi_byte, &line_starts)
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

/// Apply a Tab key press to `source` in-place, returning `(new_lo, new_hi, mutated)`.
///
/// For multi-line selections every covered line is block-indented by one
/// `INDENT` unit. For a caret or same-line selection the selection is replaced
/// with one `INDENT` and the cursor placed after it.
fn apply_tab(
    source: &mut String,
    lo_byte: usize,
    hi_byte: usize,
    line_starts: &[usize],
    multi_line: bool,
) -> (usize, usize, bool) {
    if multi_line {
        // Block-indent every covered line by one indent unit. Insert in
        // reverse so earlier byte offsets stay valid for later inserts.
        for &ls in line_starts.iter().rev() {
            source.insert_str(ls, INDENT);
        }
        // Adjust the selection endpoints. Each insertion at a line start
        // that falls BEFORE the original endpoint shifts it right by
        // INDENT.len(). We compare against the ORIGINAL positions (not the
        // accumulating new_lo/new_hi) to avoid counting a line start that
        // only appears to fall before an endpoint because earlier increments
        // pushed it there — this would over-count and produce a stale cursor
        // when INDENT is more than one byte.
        let count_before = |orig: usize| {
            line_starts.iter().filter(|&&ls| ls < orig).count()
        };
        let new_lo = lo_byte + count_before(lo_byte) * INDENT.len();
        let new_hi = hi_byte + count_before(hi_byte) * INDENT.len();
        (new_lo, new_hi, true)
    } else {
        // Caret or in-line selection: replace the selection (empty for a
        // bare caret) with one indent unit and drop the cursor after it.
        source.replace_range(lo_byte..hi_byte, INDENT);
        let new_pos = lo_byte + INDENT.len();
        (new_pos, new_pos, true)
    }
}

/// Apply a Shift+Tab key press to `source` in-place, returning `(new_lo, new_hi, mutated)`.
///
/// Every covered line loses up to `SPACES_PER_TAB` leading spaces (or one
/// leading tab). Returns `mutated = false` when no line had removable indent.
fn apply_shift_tab(
    source: &mut String,
    lo_byte: usize,
    hi_byte: usize,
    line_starts: &[usize],
) -> (usize, usize, bool) {
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
    let mut new_lo = lo_byte;
    let mut new_hi = hi_byte;
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
    (new_lo, new_hi, any)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(src: &str, lo: usize, hi: usize) -> (String, usize, usize) {
        let first_line_start = src[..lo].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let mut line_starts = vec![first_line_start];
        let mut p = first_line_start;
        while let Some(off) = src[p..].find('\n') {
            let next = p + off + 1;
            if next >= hi {
                break;
            }
            line_starts.push(next);
            p = next;
        }
        let multi_line = src[lo..hi].contains('\n');
        let mut source = src.to_string();
        let (new_lo, new_hi, _) = apply_tab(&mut source, lo, hi, &line_starts, multi_line);
        (source, new_lo, new_hi)
    }

    fn shift_tab(src: &str, lo: usize, hi: usize) -> (String, usize, usize, bool) {
        let first_line_start = src[..lo].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let mut line_starts = vec![first_line_start];
        let mut p = first_line_start;
        while let Some(off) = src[p..].find('\n') {
            let next = p + off + 1;
            if next >= hi {
                break;
            }
            line_starts.push(next);
            p = next;
        }
        let mut source = src.to_string();
        let (new_lo, new_hi, mutated) =
            apply_shift_tab(&mut source, lo, hi, &line_starts);
        (source, new_lo, new_hi, mutated)
    }

    // --- Tab: caret / inline selection ---

    #[test]
    fn tab_caret_inserts_two_spaces() {
        let src = "foo\nbar\n";
        let (text, lo, hi) = tab(src, 4, 4); // caret at start of "bar"
        assert_eq!(text, "foo\n  bar\n");
        assert_eq!(lo, 6); // cursor after the two inserted spaces
        assert_eq!(hi, 6);
    }

    #[test]
    fn tab_inline_selection_replaced_by_indent() {
        // Selecting "ba" inside "bar" (same line) — should replace with two spaces.
        let src = "foo\nbar\n";
        let (text, lo, hi) = tab(src, 4, 6); // selects "ba"
        assert_eq!(text, "foo\n  r\n");
        assert_eq!(lo, 6);
        assert_eq!(hi, 6);
    }

    #[test]
    fn tab_middle_of_line_caret() {
        let src = "hello world";
        let (text, lo, hi) = tab(src, 5, 5); // caret between 'o' and ' '
        assert_eq!(text, "hello   world");
        assert_eq!(lo, 7);
        assert_eq!(hi, 7);
    }

    // --- Tab: multi-line block indent ---

    #[test]
    fn tab_multiline_indents_each_line() {
        let src = "foo\nbar\nbaz\n";
        // Select all three lines (lo=0, hi=12 which is len)
        let (text, lo, hi) = tab(src, 0, 12);
        assert_eq!(text, "  foo\n  bar\n  baz\n");
        assert_eq!(lo, 0); // selection starts at line start — stays there
        assert_eq!(hi, 18); // 12 + 3 lines * 2 spaces
    }

    #[test]
    fn tab_multiline_cursor_in_middle_of_first_line() {
        // lo=1 (inside "foo"), hi=7 (inside "bar") — two lines.
        let src = "foo\nbar\n";
        let (text, lo, hi) = tab(src, 1, 7);
        assert_eq!(text, "  foo\n  bar\n");
        // Original lo=1 is after line-start=0, so it shifts by 2 → 3.
        assert_eq!(lo, 3);
        // Original hi=7 is after line-start=4, so it shifts by 2 (from ls=0) + 2 (from ls=4) → 11.
        assert_eq!(hi, 11);
    }

    #[test]
    fn tab_multiline_cursor_not_overcounted_when_close_line_starts() {
        // Regression: when INDENT.len()>1 and a line start falls between
        // original_lo and the first incremented new_lo, the old algorithm
        // would count it twice and produce a stale cursor position.
        //
        // Source: "a\nbc\ndef\n"  (line starts at 0, 2, 5)
        // Selection lo=1 (after 'a'), hi=9 (full "def\n").
        // Inserts happen at 0, 2, 5.  Only ls=0 is < orig lo=1 → new_lo = 3.
        let src = "a\nbc\ndef\n";
        let (text, lo, hi) = tab(src, 1, 9);
        assert_eq!(text, "  a\n  bc\n  def\n");
        // lo=1, only insert at ls=0 (< 1) shifts it: 1 + 1*2 = 3.
        assert_eq!(lo, 3);
        // hi=9, all three inserts (0<9, 2<9, 5<9) shift it: 9 + 3*2 = 15.
        assert_eq!(hi, 15);
    }

    // --- Shift+Tab: dedent ---

    #[test]
    fn shift_tab_removes_two_spaces() {
        let src = "  foo\n  bar\n";
        let (text, lo, hi, mutated) = shift_tab(src, 0, 12);
        assert_eq!(text, "foo\nbar\n");
        assert_eq!(mutated, true);
        assert_eq!(lo, 0);
        assert_eq!(hi, 8);
    }

    #[test]
    fn shift_tab_partial_indent_removes_available() {
        // Only one leading space — should remove just that one.
        let src = " foo\n";
        let (text, _, _, mutated) = shift_tab(src, 0, 5);
        assert_eq!(text, "foo\n");
        assert_eq!(mutated, true);
    }

    #[test]
    fn shift_tab_no_indent_is_noop() {
        let src = "foo\n";
        let (text, _, _, mutated) = shift_tab(src, 0, 4);
        assert_eq!(text, "foo\n");
        assert_eq!(mutated, false);
    }

    #[test]
    fn shift_tab_removes_tab_character() {
        let src = "\tfoo\n";
        let (text, _, _, mutated) = shift_tab(src, 0, 5);
        assert_eq!(text, "foo\n");
        assert_eq!(mutated, true);
    }

    // --- Round-trip: Tab then Shift+Tab ---

    #[test]
    fn tab_shift_tab_round_trip_single_line() {
        let src = "foo\n";
        let (indented, lo, hi) = tab(src, 0, 0);
        assert_eq!(indented, "  foo\n");
        let (back, _, _, _) = shift_tab(&indented, lo, hi);
        assert_eq!(back, "foo\n");
    }

    #[test]
    fn tab_shift_tab_round_trip_multiline() {
        let src = "foo\nbar\n";
        let (indented, lo, hi) = tab(src, 0, 8);
        let (back, _, _, _) = shift_tab(&indented, lo, hi);
        assert_eq!(back, "foo\nbar\n");
    }

    // --- Helper function coverage ---

    #[test]
    fn char_to_byte_ascii() {
        let s = "hello";
        assert_eq!(char_to_byte(s, 0), 0);
        assert_eq!(char_to_byte(s, 3), 3);
        assert_eq!(char_to_byte(s, 5), 5);
    }

    #[test]
    fn char_to_byte_multibyte() {
        let s = "hé"; // 'é' is 2 bytes
        assert_eq!(char_to_byte(s, 1), 1);
        assert_eq!(char_to_byte(s, 2), 3);
    }

    #[test]
    fn byte_to_char_basic() {
        let s = "hello";
        assert_eq!(byte_to_char(s, 0), 0);
        assert_eq!(byte_to_char(s, 3), 3);
    }

    #[test]
    fn count_indent_removable_full() {
        let s = "  foo";
        assert_eq!(count_indent_removable(s.as_bytes(), 0), 2);
    }

    #[test]
    fn count_indent_removable_partial() {
        let s = " foo";
        assert_eq!(count_indent_removable(s.as_bytes(), 0), 1);
    }

    #[test]
    fn count_indent_removable_none() {
        let s = "foo";
        assert_eq!(count_indent_removable(s.as_bytes(), 0), 0);
    }
}
