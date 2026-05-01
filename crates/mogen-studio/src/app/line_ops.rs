//! VS Code–style line / selection operations for the code editor.
//!
//! Mirrors `indent.rs`: handlers run before the `TextEdit` paints so they can
//! consume the relevant key combos and rewrite source + cursor state. Each op
//! returns whether the buffer was mutated so the caller can flag dirty +
//! schedule a recompile.
//!
//! Bindings (logical Cmd = Cmd on macOS, Ctrl elsewhere):
//!   - Cmd+/             toggle line comment
//!   - Cmd+L             extend selection to whole line(s)
//!   - Cmd+Shift+K       delete line(s)
//!   - Cmd+D             select word under cursor / next occurrence
//!   - Alt+Up / Alt+Down move line(s) up / down
//!
//! Cmd+D is stateless: empty selection picks the word under the caret;
//! non-empty selection jumps to the next occurrence of the same text.

use eframe::egui;
use egui::text::{CCursor, CCursorRange};

use super::MogenStudioApp;

#[derive(Clone, Copy)]
pub(super) enum LineOp {
    ToggleComment,
    SelectLine,
    DeleteLine,
    MoveUp,
    MoveDown,
    SelectNext,
}

impl MogenStudioApp {
    /// Returns `true` when the source was mutated.
    pub(super) fn handle_line_op_keys(&mut self, ui: &egui::Ui, editor_id: egui::Id) -> bool {
        if self.autocomplete.open {
            return false;
        }
        if !ui.ctx().memory(|m| m.has_focus(editor_id)) {
            return false;
        }

        use egui::{Key, Modifiers};
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let alt = Modifiers::ALT;

        // Order matters: more-specific modifier sets are checked first because
        // egui's `consume_shortcut` uses `matches_logically` and would
        // otherwise let Cmd+Shift+K fire as plain Cmd+K.
        let op = ui.input_mut(|i| {
            if i.consume_key(cmd_shift, Key::K) {
                Some(LineOp::DeleteLine)
            } else if i.consume_key(cmd, Key::Slash) {
                Some(LineOp::ToggleComment)
            } else if i.consume_key(cmd, Key::L) {
                Some(LineOp::SelectLine)
            } else if i.consume_key(cmd, Key::D) {
                Some(LineOp::SelectNext)
            } else if i.consume_key(alt, Key::ArrowUp) {
                Some(LineOp::MoveUp)
            } else if i.consume_key(alt, Key::ArrowDown) {
                Some(LineOp::MoveDown)
            } else {
                None
            }
        });

        let Some(op) = op else { return false };
        self.apply_line_op(ui.ctx(), editor_id, op)
    }

    pub(super) fn apply_line_op(
        &mut self,
        ctx: &egui::Context,
        editor_id: egui::Id,
        op: LineOp,
    ) -> bool {
        let Some(state) = egui::TextEdit::load_state(ctx, editor_id) else {
            return false;
        };
        let Some(range) = state.cursor.char_range() else {
            return false;
        };

        let i = self.active;
        let source = self.files[i].source.clone();
        let [lo_c, hi_c] = range.sorted();
        let lo = char_to_byte(&source, lo_c.index);
        let hi = char_to_byte(&source, hi_c.index);
        let primary_was_first = range.primary.index == lo_c.index;

        let result = match op {
            LineOp::ToggleComment => toggle_comment(&source, lo, hi),
            LineOp::SelectLine => select_line(&source, lo, hi),
            LineOp::DeleteLine => delete_line(&source, lo, hi),
            LineOp::MoveUp => move_line(&source, lo, hi, /* up */ true),
            LineOp::MoveDown => move_line(&source, lo, hi, /* up */ false),
            LineOp::SelectNext => select_next(&source, lo, hi),
        };

        let Some(out) = result else {
            return false;
        };

        if let Some(new_text) = out.new_text {
            self.files[i].source = new_text;
        }

        let src_now = &self.files[i].source;
        let new_lo_char = byte_to_char(src_now, out.sel_lo);
        let new_hi_char = byte_to_char(src_now, out.sel_hi);
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
        st.store(ctx, editor_id);

        out.mutated
    }
}

struct OpResult {
    new_text: Option<String>,
    sel_lo: usize,
    sel_hi: usize,
    mutated: bool,
}

fn line_start(s: &str, byte: usize) -> usize {
    s[..byte].rfind('\n').map(|p| p + 1).unwrap_or(0)
}

/// Byte index of the newline that ends the line containing `byte`, or
/// `s.len()` if the line has no trailing newline.
fn line_end(s: &str, byte: usize) -> usize {
    s[byte..].find('\n').map(|o| byte + o).unwrap_or(s.len())
}

/// Range `[start..end]` covering every line touched by the selection
/// `[lo..hi]`. `end` points at the newline (or EOF) of the last line.
fn line_block(s: &str, lo: usize, hi: usize) -> (usize, usize) {
    let start = line_start(s, lo);
    // If the selection ends exactly on a line start (e.g. user dragged down
    // through a final newline), don't pull the next line into the block.
    let probe = if hi > lo && s[..hi].ends_with('\n') {
        hi.saturating_sub(1)
    } else {
        hi
    };
    let end = line_end(s, probe);
    (start, end)
}

fn select_line(s: &str, lo: usize, hi: usize) -> Option<OpResult> {
    let (start, end) = line_block(s, lo, hi);
    // Include the trailing newline so a follow-up Cmd+L extends to the next
    // line, matching VS Code.
    let new_hi = if end < s.len() { end + 1 } else { end };
    Some(OpResult {
        new_text: None,
        sel_lo: start,
        sel_hi: new_hi,
        mutated: false,
    })
}

fn delete_line(s: &str, lo: usize, hi: usize) -> Option<OpResult> {
    let (start, end) = line_block(s, lo, hi);
    if start == end && end == s.len() {
        // Empty buffer / nothing to delete.
        return None;
    }
    let mut new_text = s.to_string();
    let (cut_lo, cut_hi) = if end < s.len() {
        // Take the trailing newline with the line.
        (start, end + 1)
    } else if start > 0 {
        // Last line, no trailing newline — eat the preceding newline so the
        // file doesn't end with an extra blank.
        (start - 1, end)
    } else {
        (start, end)
    };
    new_text.replace_range(cut_lo..cut_hi, "");
    let caret = cut_lo.min(new_text.len());
    Some(OpResult {
        new_text: Some(new_text),
        sel_lo: caret,
        sel_hi: caret,
        mutated: true,
    })
}

/// Toggle `// ` line comments across the selected line range. If every
/// non-blank line in the range is already commented, strip the comment;
/// otherwise insert a comment at a uniform column equal to the minimum
/// indent across non-blank lines.
fn toggle_comment(s: &str, lo: usize, hi: usize) -> Option<OpResult> {
    let (block_start, block_end) = line_block(s, lo, hi);
    if block_start == block_end {
        // Single empty line — toggle by inserting an empty comment.
        let mut new_text = String::with_capacity(s.len() + 3);
        new_text.push_str(&s[..block_start]);
        new_text.push_str("// ");
        new_text.push_str(&s[block_start..]);
        let new_lo = block_start + 3;
        let new_hi = new_lo;
        return Some(OpResult {
            new_text: Some(new_text),
            sel_lo: new_lo,
            sel_hi: new_hi,
            mutated: true,
        });
    }

    let block = &s[block_start..block_end];
    let line_offsets: Vec<usize> = std::iter::once(0)
        .chain(
            block
                .match_indices('\n')
                .map(|(p, _)| p + 1)
                .filter(|&p| p < block.len()),
        )
        .collect();
    let lines: Vec<&str> = line_offsets
        .iter()
        .map(|&off| {
            let end = block[off..]
                .find('\n')
                .map(|p| off + p)
                .unwrap_or(block.len());
            &block[off..end]
        })
        .collect();

    let non_blank: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| (!line.trim().is_empty()).then_some(idx))
        .collect();
    if non_blank.is_empty() {
        return None;
    }

    let all_commented = non_blank
        .iter()
        .all(|&idx| lines[idx].trim_start().starts_with("//"));

    let mut new_block = String::with_capacity(block.len() + non_blank.len() * 3);
    if all_commented {
        // Uncomment: strip the first `//` (and a single following space if
        // present) from each non-blank line. Blank lines pass through.
        for (idx, line) in lines.iter().enumerate() {
            if idx > 0 {
                new_block.push('\n');
            }
            if non_blank.contains(&idx) {
                let leading_ws: usize = line
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .map(char::len_utf8)
                    .sum();
                let (indent, rest) = line.split_at(leading_ws);
                debug_assert!(rest.starts_with("//"));
                let after = &rest[2..];
                let after = after.strip_prefix(' ').unwrap_or(after);
                new_block.push_str(indent);
                new_block.push_str(after);
            } else {
                new_block.push_str(line);
            }
        }
    } else {
        // Comment: insert `// ` at the uniform min-indent column.
        let min_indent: usize = non_blank
            .iter()
            .map(|&idx| {
                lines[idx]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .map(char::len_utf8)
                    .sum::<usize>()
            })
            .min()
            .unwrap_or(0);
        for (idx, line) in lines.iter().enumerate() {
            if idx > 0 {
                new_block.push('\n');
            }
            if non_blank.contains(&idx) {
                let split = min_indent.min(line.len());
                new_block.push_str(&line[..split]);
                new_block.push_str("// ");
                new_block.push_str(&line[split..]);
            } else {
                new_block.push_str(line);
            }
        }
    }

    let mut new_text = String::with_capacity(s.len() + 32);
    new_text.push_str(&s[..block_start]);
    new_text.push_str(&new_block);
    new_text.push_str(&s[block_end..]);

    // Re-clamp the original selection into the rewritten block.
    let new_block_end = block_start + new_block.len();
    let new_lo = lo.min(new_block_end);
    let new_hi = hi.min(new_block_end);
    Some(OpResult {
        new_text: Some(new_text),
        sel_lo: new_lo,
        sel_hi: new_hi,
        mutated: true,
    })
}

fn move_line(s: &str, lo: usize, hi: usize, up: bool) -> Option<OpResult> {
    let (block_start, block_end) = line_block(s, lo, hi);
    if up {
        if block_start == 0 {
            return None;
        }
        // Previous line: from the byte after the prior newline (or 0) up to
        // the newline at block_start - 1.
        let prev_end = block_start - 1; // position of the '\n' separating prev from block
        let prev_start = s[..prev_end].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prev = &s[prev_start..prev_end];
        let block = &s[block_start..block_end];

        let mut new_text = String::with_capacity(s.len());
        new_text.push_str(&s[..prev_start]);
        new_text.push_str(block);
        new_text.push('\n');
        new_text.push_str(prev);
        new_text.push_str(&s[block_end..]);

        let shift = (prev_end + 1) - prev_start; // length of prev line + its newline
        let new_lo = lo.saturating_sub(shift);
        let new_hi = hi.saturating_sub(shift);
        Some(OpResult {
            new_text: Some(new_text),
            sel_lo: new_lo,
            sel_hi: new_hi,
            mutated: true,
        })
    } else {
        if block_end == s.len() {
            return None;
        }
        // Next line spans `[block_end + 1 .. line_end(block_end + 1)]`.
        let next_start = block_end + 1;
        let next_end = line_end(s, next_start);
        let next = &s[next_start..next_end];
        let block = &s[block_start..block_end];

        let mut new_text = String::with_capacity(s.len());
        new_text.push_str(&s[..block_start]);
        new_text.push_str(next);
        new_text.push('\n');
        new_text.push_str(block);
        new_text.push_str(&s[next_end..]);

        let shift = next.len() + 1;
        let new_lo = lo + shift;
        let new_hi = hi + shift;
        Some(OpResult {
            new_text: Some(new_text),
            sel_lo: new_lo,
            sel_hi: new_hi,
            mutated: true,
        })
    }
}

/// Cmd+D: when nothing is selected, expand the selection to the word under
/// the caret. Otherwise jump to the next occurrence of the current selection
/// (case-sensitive, wrapping). Single-cursor only.
fn select_next(s: &str, lo: usize, hi: usize) -> Option<OpResult> {
    if hi == lo {
        let bytes = s.as_bytes();
        let len = bytes.len();
        if len == 0 {
            return None;
        }
        let mut start = lo.min(len);
        // If caret sits between a non-word char on the left and a word char
        // on the right, prefer the word to the right (matches VS Code).
        let on_word = |b: usize| b < len && is_word_byte(bytes[b]);
        if !on_word(start) && start > 0 && on_word(start - 1) {
            start -= 1;
        }
        if !on_word(start) {
            return None;
        }
        let mut ws = start;
        while ws > 0 && is_word_byte(bytes[ws - 1]) {
            ws -= 1;
        }
        let mut we = start;
        while we < len && is_word_byte(bytes[we]) {
            we += 1;
        }
        if we == ws {
            return None;
        }
        return Some(OpResult {
            new_text: None,
            sel_lo: ws,
            sel_hi: we,
            mutated: false,
        });
    }

    let needle = &s[lo..hi];
    if needle.is_empty() || needle.contains('\n') {
        return None;
    }
    let after = &s[hi..];
    let next = if let Some(rel) = after.find(needle) {
        Some(hi + rel)
    } else {
        // Wrap: search from the start, but skip the current match itself.
        s[..lo].find(needle)
    };
    let Some(found) = next else {
        return None;
    };
    Some(OpResult {
        new_text: None,
        sel_lo: found,
        sel_hi: found + needle.len(),
        mutated: false,
    })
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(op: impl Fn(&str, usize, usize) -> Option<OpResult>, src: &str, lo: usize, hi: usize) -> (String, usize, usize) {
        let r = op(src, lo, hi).expect("op should produce a result");
        let text = r.new_text.unwrap_or_else(|| src.to_string());
        (text, r.sel_lo, r.sel_hi)
    }

    #[test]
    fn select_line_extends_to_full_line() {
        let src = "alpha\nbeta\ngamma\n";
        let (text, lo, hi) = run(select_line, src, 7, 7); // caret inside "beta"
        assert_eq!(text, src);
        assert_eq!(&src[lo..hi], "beta\n");
    }

    #[test]
    fn delete_line_removes_middle() {
        let src = "a\nb\nc\n";
        let (text, lo, hi) = run(delete_line, src, 2, 2);
        assert_eq!(text, "a\nc\n");
        assert_eq!(lo, 2);
        assert_eq!(hi, 2);
    }

    #[test]
    fn delete_line_removes_last_no_newline() {
        let src = "a\nb";
        let (text, _, _) = run(delete_line, src, 3, 3);
        assert_eq!(text, "a");
    }

    #[test]
    fn toggle_comment_adds_uniform_indent() {
        let src = "    foo\n      bar\n";
        let (text, _, _) = run(toggle_comment, src, 0, src.len());
        assert_eq!(text, "    // foo\n    //   bar\n");
    }

    #[test]
    fn toggle_comment_round_trip() {
        let src = "    foo\n    bar\n";
        let (commented, _, _) = run(toggle_comment, src, 0, src.len());
        let (back, _, _) = run(toggle_comment, &commented, 0, commented.len());
        assert_eq!(back, src);
    }

    #[test]
    fn toggle_comment_skips_blank_lines() {
        let src = "  foo\n\n  bar\n";
        let (text, _, _) = run(toggle_comment, src, 0, src.len());
        assert_eq!(text, "  // foo\n\n  // bar\n");
    }

    #[test]
    fn move_line_up_swaps() {
        let src = "alpha\nbeta\ngamma\n";
        let (text, _, _) = run(|s, lo, hi| move_line(s, lo, hi, true), src, 6, 6); // caret on "beta"
        assert_eq!(text, "beta\nalpha\ngamma\n");
    }

    #[test]
    fn move_line_down_swaps() {
        let src = "alpha\nbeta\ngamma\n";
        let (text, _, _) = run(|s, lo, hi| move_line(s, lo, hi, false), src, 0, 0);
        assert_eq!(text, "beta\nalpha\ngamma\n");
    }

    #[test]
    fn move_line_up_at_top_is_noop() {
        let src = "alpha\nbeta\n";
        assert!(move_line(src, 0, 0, true).is_none());
    }

    #[test]
    fn move_line_down_at_bottom_is_noop() {
        let src = "alpha\nbeta";
        assert!(move_line(src, 7, 7, false).is_none());
    }

    #[test]
    fn select_next_picks_word_under_caret() {
        let src = "foo bar foo";
        let (_, lo, hi) = run(select_next, src, 5, 5); // caret inside "bar"
        assert_eq!(&src[lo..hi], "bar");
    }

    #[test]
    fn select_next_jumps_to_next_occurrence() {
        let src = "foo bar foo bar";
        let (_, lo, hi) = run(select_next, src, 0, 3); // selecting first "foo"
        assert_eq!(&src[lo..hi], "foo");
        assert_eq!(lo, 8);
    }

    #[test]
    fn select_next_wraps() {
        let src = "foo bar foo bar";
        let (_, lo, hi) = run(select_next, src, 8, 11); // second "foo"
        assert_eq!(lo, 0);
        assert_eq!(hi, 3);
    }
}
