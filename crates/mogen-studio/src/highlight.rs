//! Syntax tokeniser + LayoutJob builder for `.mog` source shown in the editor.
//!
//! The tokeniser is deliberately loose and only knows the shape of the grammar
//! (comments, strings, numbers, identifiers, param refs, punctuation). It does
//! not share state with `mogen-dsl`'s pest parser — an unfinished line mid-edit
//! still needs to colour — so correctness here is "best effort that degrades
//! gracefully" rather than authoritative.

use eframe::egui::{self, text::LayoutJob, Color32, FontId, TextFormat};

use mogen_validate::KNOWN_KINDS;

/// Parameter enum values that commonly appear bare on the RHS of an attribute
/// (e.g. `axis=y`, `uv_mode=tile`, `kind=directional`). These aren't node
/// kinds — the highlighter paints them as keywords for visual consistency with
/// the enum names dispatched on in `mogen-dsl/src/{lower,anim_lower,…}`.
/// Node kinds themselves come from [`KNOWN_KINDS`].
const ENUM_VALUE_KEYWORDS: &[&str] = &[
    // Joint type names
    "translation", "rotation", "scale", "hinge", "slider", "ball", "rotor",
    // Material/light enum values
    "opaque", "mask", "blend", "tile", "fit", "directional", "point", "spot",
];

fn is_keyword(word: &str) -> bool {
    KNOWN_KINDS.contains(&word) || ENUM_VALUE_KEYWORDS.contains(&word)
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub default: Color32,
    pub keyword: Color32,
    pub string: Color32,
    pub number: Color32,
    pub comment: Color32,
    pub param_ref: Color32,
    pub punct: Color32,
    pub gutter: Color32,
}

impl Palette {
    pub fn for_visuals(visuals: &egui::Visuals) -> Self {
        if visuals.dark_mode {
            Self::dark(visuals)
        } else {
            Self::light(visuals)
        }
    }

    fn dark(visuals: &egui::Visuals) -> Self {
        Self {
            default: visuals.text_color(),
            keyword: Color32::from_rgb(86, 156, 214),
            string: Color32::from_rgb(206, 145, 120),
            number: Color32::from_rgb(181, 206, 168),
            comment: Color32::from_rgb(106, 153, 85),
            param_ref: Color32::from_rgb(197, 134, 192),
            punct: Color32::from_rgb(200, 200, 200),
            gutter: Color32::from_rgb(120, 120, 130),
        }
    }

    fn light(visuals: &egui::Visuals) -> Self {
        Self {
            default: visuals.text_color(),
            keyword: Color32::from_rgb(0, 0, 200),
            string: Color32::from_rgb(163, 21, 21),
            number: Color32::from_rgb(9, 134, 88),
            comment: Color32::from_rgb(0, 128, 0),
            param_ref: Color32::from_rgb(128, 0, 128),
            punct: Color32::from_rgb(80, 80, 80),
            gutter: Color32::from_rgb(130, 130, 140),
        }
    }
}

/// Build a styled `LayoutJob` for `src` using the monospace font at `font_id`.
/// Pass `f32::INFINITY` for `wrap_width` to disable wrapping (long lines scroll
/// horizontally in the surrounding `ScrollArea`) or a finite value to enable
/// soft-wrap at word boundaries — the gutter must then be built with
/// `visual_rows_per_line` so continuation rows render as blanks instead of
/// pulling line numbers out of alignment.
pub fn highlight(src: &str, font_id: FontId, palette: Palette, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;

    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];

        // Line comment: `// … \n` — consume up to (not including) newline.
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            append(&mut job, &src[start..i], palette.comment, &font_id);
            continue;
        }

        // String literal: `"…"` — consume up to the matching quote or EOL so
        // an unclosed string doesn't dye the rest of the file.
        if c == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                i += 1;
            }
            append(&mut job, &src[start..i], palette.string, &font_id);
            continue;
        }

        // `$ident` — param reference inside a module body.
        if c == b'$' {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            append(&mut job, &src[start..i], palette.param_ref, &font_id);
            continue;
        }

        // Number: ASCII digit, optionally followed by `.digits`. A leading `-`
        // is always emitted as punctuation so we don't misread `a-1` as a
        // negative literal.
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            append(&mut job, &src[start..i], palette.number, &font_id);
            continue;
        }

        // Identifier — keyword lookup decides colour.
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            let word = &src[start..i];
            let color = if is_keyword(word) {
                palette.keyword
            } else {
                palette.default
            };
            append(&mut job, word, color, &font_id);
            continue;
        }

        // Whitespace (including newlines) stays with the default colour so
        // selection rendering doesn't look patchy.
        if c.is_ascii_whitespace() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            append(&mut job, &src[start..i], palette.default, &font_id);
            continue;
        }

        // Everything else: punctuation / operators. Handle one byte at a time
        // so multi-byte UTF-8 chars (e.g. a user pasting a smart quote) don't
        // panic on a mid-codepoint slice.
        let ch_end = next_char_boundary(src, i);
        append(&mut job, &src[i..ch_end], palette.punct, &font_id);
        i = ch_end;
    }

    job
}

/// Build a right-aligned gutter `LayoutJob`, padded to at least `min_rows`.
///
/// When `visual_rows_per_line` is `None`, emits one row per source line
/// (the no-word-wrap default). When `Some`, each entry is the count of
/// visual rows that source line occupies in the wrapped editor — the line
/// number is rendered on the first visual row and continuation rows are
/// blank cells so the gutter and editor stay aligned. Rows beyond the
/// content's total visual rows pad with blanks so the column extends
/// alongside a TextEdit whose `desired_rows` exceeds the content length.
pub fn gutter_job_padded(
    src: &str,
    min_rows: usize,
    visual_rows_per_line: Option<&[usize]>,
    font_id: FontId,
    palette: Palette,
) -> (LayoutJob, usize) {
    let line_count = line_count_for(src);
    let content_visual_rows: usize = match visual_rows_per_line {
        Some(per) => per.iter().map(|n| (*n).max(1)).sum(),
        None => line_count,
    };
    let total_rows = content_visual_rows.max(min_rows).max(1);
    let digits = line_count.max(1).to_string().len().max(3);
    let fmt = TextFormat {
        font_id,
        color: palette.gutter,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    let blank_cell = " ".repeat(digits);
    let mut first = true;
    let push = |job: &mut LayoutJob, cell: &str, first: &mut bool| {
        if !*first {
            job.append("\n", 0.0, fmt.clone());
        }
        *first = false;
        job.append(cell, 0.0, fmt.clone());
    };

    let mut emitted = 0usize;
    match visual_rows_per_line {
        Some(per) => {
            for (idx, &vrows) in per.iter().enumerate() {
                let line_no = idx + 1;
                let label = format!("{:>width$}", line_no, width = digits);
                let vrows = vrows.max(1);
                for r in 0..vrows {
                    let cell: &str = if r == 0 { label.as_str() } else { blank_cell.as_str() };
                    push(&mut job, cell, &mut first);
                    emitted += 1;
                }
            }
        }
        None => {
            for n in 1..=line_count {
                let label = format!("{:>width$}", n, width = digits);
                push(&mut job, &label, &mut first);
                emitted += 1;
            }
        }
    }
    while emitted < total_rows {
        push(&mut job, &blank_cell, &mut first);
        emitted += 1;
    }
    (job, digits)
}

/// Match the row count TextEdit renders: one row per `\n`, plus one for the
/// final (possibly unterminated) line. Empty input still gets row 1 so the
/// gutter doesn't disappear on a blank buffer.
pub fn line_count_for(src: &str) -> usize {
    if src.is_empty() {
        return 1;
    }
    let nl = src.as_bytes().iter().filter(|b| **b == b'\n').count();
    nl + 1
}

fn append(job: &mut LayoutJob, slice: &str, color: Color32, font_id: &FontId) {
    if slice.is_empty() {
        return;
    }
    let fmt = TextFormat {
        font_id: font_id.clone(),
        color,
        ..Default::default()
    };
    job.append(slice, 0.0, fmt);
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn next_char_boundary(src: &str, i: usize) -> usize {
    let mut j = i + 1;
    while j < src.len() && !src.is_char_boundary(j) {
        j += 1;
    }
    j
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections_text(job: &LayoutJob) -> Vec<(String, Color32)> {
        job.sections
            .iter()
            .map(|s| (job.text[s.byte_range.clone()].to_string(), s.format.color))
            .collect()
    }

    fn palette() -> Palette {
        Palette::dark(&egui::Visuals::dark())
    }

    fn font() -> FontId {
        FontId::monospace(14.0)
    }

    #[test]
    fn highlights_keyword_over_identifier() {
        let job = highlight("box foo", font(), palette(), f32::INFINITY);
        let secs = sections_text(&job);
        assert_eq!(secs[0].0, "box");
        assert_eq!(secs[0].1, palette().keyword);
        // "foo" is an identifier, not a keyword.
        let foo = secs.iter().find(|(t, _)| t == "foo").expect("foo section");
        assert_eq!(foo.1, palette().default);
    }

    #[test]
    fn highlights_comment_to_end_of_line() {
        let job = highlight("// hi\nbox", font(), palette(), f32::INFINITY);
        let secs = sections_text(&job);
        assert_eq!(secs[0].0, "// hi");
        assert_eq!(secs[0].1, palette().comment);
    }

    #[test]
    fn highlights_string_literal() {
        let job = highlight("box \"hello\"", font(), palette(), f32::INFINITY);
        let secs = sections_text(&job);
        let s = secs.iter().find(|(t, _)| t == "\"hello\"").expect("string section");
        assert_eq!(s.1, palette().string);
    }

    #[test]
    fn highlights_numbers() {
        let job = highlight("x=1.5", font(), palette(), f32::INFINITY);
        let secs = sections_text(&job);
        let n = secs.iter().find(|(t, _)| t == "1.5").expect("number section");
        assert_eq!(n.1, palette().number);
    }

    #[test]
    fn highlights_param_ref() {
        let job = highlight("$size", font(), palette(), f32::INFINITY);
        let secs = sections_text(&job);
        assert_eq!(secs[0].0, "$size");
        assert_eq!(secs[0].1, palette().param_ref);
    }

    #[test]
    fn tolerates_unclosed_string() {
        // Unterminated strings mid-edit must not consume the rest of the buffer.
        let job = highlight("\"oops\nbox", font(), palette(), f32::INFINITY);
        let secs = sections_text(&job);
        assert_eq!(secs[0].0, "\"oops");
        assert_eq!(secs[0].1, palette().string);
        assert!(secs.iter().any(|(t, c)| t == "box" && *c == palette().keyword));
    }

    #[test]
    fn gutter_line_count_matches_newlines() {
        let (_, _) = gutter_job_padded("a\nb\nc", 0, None, font(), palette());
        assert_eq!(line_count_for("a\nb\nc"), 3);
        assert_eq!(line_count_for("a\nb\n"), 3);
        assert_eq!(line_count_for(""), 1);
    }

    #[test]
    fn gutter_pads_to_min_rows() {
        // With 2 source lines but min_rows=5, the gutter should render 5
        // rows: "1".."2" for the content and blank cells for rows 3..=5.
        let (job, digits) = gutter_job_padded("a\nb", 5, None, font(), palette());
        let newline_count = job.text.chars().filter(|c| *c == '\n').count();
        assert_eq!(newline_count, 4, "5 rows means 4 newlines between them");
        // Rows beyond the content should be all-whitespace.
        let rows: Vec<&str> = job.text.split('\n').collect();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].trim(), "1");
        assert_eq!(rows[1].trim(), "2");
        for row in &rows[2..] {
            assert!(row.chars().all(|c| c == ' '), "row must be blank: {row:?}");
            assert_eq!(row.len(), digits);
        }
    }

    #[test]
    fn gutter_padding_does_not_shrink_content() {
        // min_rows smaller than the content's line count must not truncate.
        let (job, _) = gutter_job_padded("a\nb\nc\nd", 2, None, font(), palette());
        let rows: Vec<&str> = job.text.split('\n').collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[3].trim(), "4");
    }

    #[test]
    fn gutter_wrapped_inserts_blank_continuation_rows() {
        // 3 source lines where line 2 occupies 3 visual rows in the wrapped
        // editor → gutter must read "1", "2", " ", " ", "3".
        let (job, digits) =
            gutter_job_padded("a\nlong\nc", 0, Some(&[1, 3, 1]), font(), palette());
        let rows: Vec<&str> = job.text.split('\n').collect();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].trim(), "1");
        assert_eq!(rows[1].trim(), "2");
        assert!(rows[2].chars().all(|c| c == ' '));
        assert_eq!(rows[2].len(), digits);
        assert!(rows[3].chars().all(|c| c == ' '));
        assert_eq!(rows[3].len(), digits);
        assert_eq!(rows[4].trim(), "3");
    }

    #[test]
    fn gutter_wrapped_pads_visual_rows_to_min() {
        // 2 source lines, line 1 wraps to 2 visual rows → 3 content rows.
        // min_rows=6 must add 3 trailing blank rows on top of the content.
        let (job, digits) =
            gutter_job_padded("ab\nc", 6, Some(&[2, 1]), font(), palette());
        let rows: Vec<&str> = job.text.split('\n').collect();
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0].trim(), "1");
        assert!(rows[1].chars().all(|c| c == ' '));
        assert_eq!(rows[1].len(), digits);
        assert_eq!(rows[2].trim(), "2");
        for row in &rows[3..] {
            assert!(row.chars().all(|c| c == ' '), "tail row must be blank: {row:?}");
            assert_eq!(row.len(), digits);
        }
    }

    #[test]
    fn handles_utf8_punctuation() {
        // A smart quote used to panic the byte-at-a-time punctuation branch.
        let job = highlight("box \u{201C}x\u{201D}", font(), palette(), f32::INFINITY);
        // Should produce sections without panicking and recover `box` as a keyword.
        let secs = sections_text(&job);
        assert_eq!(secs[0].0, "box");
        assert_eq!(secs[0].1, palette().keyword);
    }

    #[test]
    fn highlights_kinds_absent_from_old_keyword_list() {
        // chamfered_box, metaball, coil, extrude, heightfield are in KNOWN_KINDS
        // but were missing from the old hand-written KEYWORDS array. Verify
        // they now paint as keywords via the KNOWN_KINDS reference.
        let src = "chamfered_box metaball coil extrude heightfield";
        let job = highlight(src, font(), palette(), f32::INFINITY);
        let secs = sections_text(&job);
        for token in &["chamfered_box", "metaball", "coil", "extrude", "heightfield"] {
            let sec = secs
                .iter()
                .find(|(t, _)| t.as_str() == *token)
                .unwrap_or_else(|| panic!("{token} section not found"));
            assert_eq!(sec.1, palette().keyword, "{token} should paint as keyword");
        }
    }
}
