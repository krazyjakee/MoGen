//! Syntax tokeniser + LayoutJob builder for `.mog` source shown in the editor.
//!
//! The tokeniser is deliberately loose and only knows the shape of the grammar
//! (comments, strings, numbers, identifiers, param refs, punctuation). It does
//! not share state with `mogen-dsl`'s pest parser — an unfinished line mid-edit
//! still needs to colour — so correctness here is "best effort that degrades
//! gracefully" rather than authoritative.

use eframe::egui::{self, text::LayoutJob, Color32, FontId, TextFormat};

/// Node kinds + parameter enum values from the `.mog` grammar. Anything in this
/// set is painted as a keyword regardless of position; everything else is left
/// with the default text colour. Keep this list roughly aligned with the match
/// arms in `mogen-dsl/src/{lower,module,anim_lower,skin_lower,attach}.rs`.
const KEYWORDS: &[&str] = &[
    // Top-level + structural
    "scene", "module", "use", "group",
    // Primitives
    "box", "sphere", "cylinder", "cone", "capsule", "torus", "prism", "pyramid", "disc",
    "icosphere", "rounded_box", "plane", "quad", "ellipsoid", "superellipsoid", "hemisphere",
    "frustum", "tube", "spline_tube", "torus_arc", "half_cylinder", "curved_plane", "lathe",
    "wedge", "slab", "panel", "wall", "roof",
    // CSG + repetition
    "union", "difference", "intersect", "array", "mirror",
    // Materials + attachment
    "material", "attach", "connector", "mask",
    // Animation + skinning
    "joint", "skeleton", "clip", "track", "skin", "spin", "open_close", "wave", "flap", "idle",
    "translation", "rotation", "scale", "hinge", "slider", "ball", "rotor",
    // Parameter enum values that commonly appear at the top level of an attr
    "opaque", "blend",
];

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
/// Wrapping is intentionally disabled so one source line is one visual row —
/// the gutter renders one number per source line, and a wrapped editor would
/// drift the numbers out of alignment. Long lines scroll horizontally in the
/// surrounding `ScrollArea` instead.
pub fn highlight(src: &str, font_id: FontId, palette: Palette) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;

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
            let color = if KEYWORDS.contains(&word) {
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

/// Build a right-aligned gutter `LayoutJob` with one row per source line,
/// padded to at least `min_rows`. Rows beyond the source's line count are
/// rendered as blank cells so the gutter column visually extends alongside a
/// TextEdit whose `desired_rows` exceeds the content length.
pub fn gutter_job_padded(
    src: &str,
    min_rows: usize,
    font_id: FontId,
    palette: Palette,
) -> (LayoutJob, usize) {
    let line_count = line_count_for(src);
    let total_rows = line_count.max(min_rows).max(1);
    let digits = line_count.max(1).to_string().len().max(3);
    let fmt = TextFormat {
        font_id,
        color: palette.gutter,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    for n in 1..=total_rows {
        if n > 1 {
            job.append("\n", 0.0, fmt.clone());
        }
        let cell = if n <= line_count {
            format!("{:>width$}", n, width = digits)
        } else {
            " ".repeat(digits)
        };
        job.append(&cell, 0.0, fmt.clone());
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
        let job = highlight("box foo", font(), palette());
        let secs = sections_text(&job);
        assert_eq!(secs[0].0, "box");
        assert_eq!(secs[0].1, palette().keyword);
        // "foo" is an identifier, not a keyword.
        let foo = secs.iter().find(|(t, _)| t == "foo").expect("foo section");
        assert_eq!(foo.1, palette().default);
    }

    #[test]
    fn highlights_comment_to_end_of_line() {
        let job = highlight("// hi\nbox", font(), palette());
        let secs = sections_text(&job);
        assert_eq!(secs[0].0, "// hi");
        assert_eq!(secs[0].1, palette().comment);
    }

    #[test]
    fn highlights_string_literal() {
        let job = highlight("box \"hello\"", font(), palette());
        let secs = sections_text(&job);
        let s = secs.iter().find(|(t, _)| t == "\"hello\"").expect("string section");
        assert_eq!(s.1, palette().string);
    }

    #[test]
    fn highlights_numbers() {
        let job = highlight("x=1.5", font(), palette());
        let secs = sections_text(&job);
        let n = secs.iter().find(|(t, _)| t == "1.5").expect("number section");
        assert_eq!(n.1, palette().number);
    }

    #[test]
    fn highlights_param_ref() {
        let job = highlight("$size", font(), palette());
        let secs = sections_text(&job);
        assert_eq!(secs[0].0, "$size");
        assert_eq!(secs[0].1, palette().param_ref);
    }

    #[test]
    fn tolerates_unclosed_string() {
        // Unterminated strings mid-edit must not consume the rest of the buffer.
        let job = highlight("\"oops\nbox", font(), palette());
        let secs = sections_text(&job);
        assert_eq!(secs[0].0, "\"oops");
        assert_eq!(secs[0].1, palette().string);
        assert!(secs.iter().any(|(t, c)| t == "box" && *c == palette().keyword));
    }

    #[test]
    fn gutter_line_count_matches_newlines() {
        let (_, _) = gutter_job_padded("a\nb\nc", 0, font(), palette());
        assert_eq!(line_count_for("a\nb\nc"), 3);
        assert_eq!(line_count_for("a\nb\n"), 3);
        assert_eq!(line_count_for(""), 1);
    }

    #[test]
    fn gutter_pads_to_min_rows() {
        // With 2 source lines but min_rows=5, the gutter should render 5
        // rows: "1".."2" for the content and blank cells for rows 3..=5.
        let (job, digits) = gutter_job_padded("a\nb", 5, font(), palette());
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
        let (job, _) = gutter_job_padded("a\nb\nc\nd", 2, font(), palette());
        let rows: Vec<&str> = job.text.split('\n').collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[3].trim(), "4");
    }

    #[test]
    fn handles_utf8_punctuation() {
        // A smart quote used to panic the byte-at-a-time punctuation branch.
        let job = highlight("box \u{201C}x\u{201D}", font(), palette());
        // Should produce sections without panicking and recover `box` as a keyword.
        let secs = sections_text(&job);
        assert_eq!(secs[0].0, "box");
        assert_eq!(secs[0].1, palette().keyword);
    }
}
