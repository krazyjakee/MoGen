//! Pretty-printer for `.mog` source. Re-indents block bodies (`{`/`}`) and
//! multi-line attribute lists (`(`/`)`) at 2 spaces per level, strips trailing
//! whitespace, collapses runs of blank lines, and forces a newline after a
//! `{` / before a `}` so blocks lay out predictably — much like a JavaScript
//! beautifier in its default mode.
//!
//! Conservative on purpose: string literals and `//` line comments pass
//! through verbatim, and nothing inside an attribute list gets reflowed onto
//! new lines (the user might have packed `(a=1, b=2)` tightly on purpose).

/// Reformat `src` using brace + paren depth as the indentation guide. Always
/// produces output terminated by a single `\n` when non-empty. Idempotent:
/// `tidy(tidy(s)) == tidy(s)`.
pub fn tidy(src: &str) -> String {
    let logical = split_logical_lines(src);

    let mut out = String::with_capacity(src.len() + 32);
    let mut depth_brace: i32 = 0;
    let mut depth_paren: i32 = 0;
    let mut prev_blank = true;
    let mut emitted_any = false;

    for line in &logical {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if emitted_any && !prev_blank {
                out.push('\n');
            }
            prev_blank = true;
            continue;
        }

        let (lead_close_brace, lead_close_paren) = count_leading_closers(trimmed);
        let eff_brace = (depth_brace - lead_close_brace).max(0);
        let eff_paren = (depth_paren - lead_close_paren).max(0);
        let indent = (eff_brace + eff_paren) as usize * 2;

        for _ in 0..indent {
            out.push(' ');
        }
        out.push_str(trimmed);
        out.push('\n');

        let (db, dp) = compute_delta(trimmed);
        depth_brace = (depth_brace + db).max(0);
        depth_paren = (depth_paren + dp).max(0);

        prev_blank = false;
        emitted_any = true;
    }

    out
}

/// Slice the source into one entry per output line: original `\n` newlines
/// break a line, and a `{` / `}` outside strings + comments forces a break
/// too so blocks lay out one statement per line. String contents and `//`
/// comments pass through opaquely so a `{` inside a `"…"` literal does not
/// trigger a split.
fn split_logical_lines(src: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = src.chars().peekable();
    let mut in_str = false;
    let mut esc = false;
    let mut in_comment = false;

    while let Some(c) = chars.next() {
        if in_comment {
            if c == '\n' {
                out.push(std::mem::take(&mut cur));
                in_comment = false;
            } else {
                cur.push(c);
            }
            continue;
        }
        if in_str {
            cur.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                cur.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                cur.push(c);
                cur.push(chars.next().unwrap());
                in_comment = true;
            }
            '\n' => {
                out.push(std::mem::take(&mut cur));
            }
            '{' => {
                cur.push(c);
                out.push(std::mem::take(&mut cur));
                consume_line_tail(&mut chars);
            }
            '}' => {
                if !cur.trim().is_empty() {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
                cur.push(c);
                out.push(std::mem::take(&mut cur));
                consume_line_tail(&mut chars);
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// After a `{` / `}` forces a line break, the rest of that physical source
/// line is just trailing whitespace and the line's own `\n`. Swallow it so it
/// doesn't surface as a phantom blank logical line (which would render as a
/// spurious blank after `{` or an extra trailing newline). Stops at the first
/// non-whitespace char or after consuming a single newline, so deliberate
/// blank lines further down are still preserved.
fn consume_line_tail(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while let Some(&c) = chars.peek() {
        if c == '\n' {
            chars.next();
            break;
        }
        if c == ' ' || c == '\t' || c == '\r' {
            chars.next();
        } else {
            break;
        }
    }
}

/// Count `}` / `)` characters at the very start of a trimmed line. The line
/// is dedented by this much before its indent is applied, so a line that
/// only carries closers lands at the outer level rather than the inner one.
fn count_leading_closers(line: &str) -> (i32, i32) {
    let mut b = 0i32;
    let mut p = 0i32;
    for c in line.chars() {
        match c {
            '}' => b += 1,
            ')' => p += 1,
            c if c.is_whitespace() => {}
            _ => break,
        }
    }
    (b, p)
}

/// Net change to brace and paren depths contributed by `line`, ignoring
/// anything inside string literals or `//` comments.
fn compute_delta(line: &str) -> (i32, i32) {
    let mut b = 0i32;
    let mut p = 0i32;
    let mut chars = line.chars().peekable();
    let mut in_str = false;
    let mut esc = false;
    while let Some(c) = chars.next() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '/' if chars.peek() == Some(&'/') => break,
            '{' => b += 1,
            '}' => b -= 1,
            '(' => p += 1,
            ')' => p -= 1,
            _ => {}
        }
    }
    (b, p)
}

#[cfg(test)]
mod tests {
    use super::tidy;

    #[test]
    fn idempotent_on_clean_source() {
        let src = "scene {\n  box \"b\" (size=[1, 1, 1])\n}\n";
        assert_eq!(tidy(src), src);
        assert_eq!(tidy(&tidy(src)), tidy(src));
    }

    #[test]
    fn splits_inline_block() {
        let src = "scene { box \"b\" (size=1) }";
        let want = "scene {\n  box \"b\" (size=1)\n}\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn reindents_misaligned_block() {
        let src = "scene {\nbox \"b\" (size=1)\n      cylinder \"c\" (radius=1)\n}\n";
        let want = "scene {\n  box \"b\" (size=1)\n  cylinder \"c\" (radius=1)\n}\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn indents_multiline_attr_list() {
        let src = "meta (\nname = \"x\",\nseed = \"1\"\n)\n";
        let want = "meta (\n  name = \"x\",\n  seed = \"1\"\n)\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn collapses_blank_runs() {
        let src = "a\n\n\n\nb\n";
        let want = "a\n\nb\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn strips_trailing_whitespace() {
        let src = "scene {  \n  box \"b\"   \n}\n";
        let want = "scene {\n  box \"b\"\n}\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn preserves_strings_with_braces() {
        let src = "meta (note = \"{not a block}\")\n";
        assert_eq!(tidy(src), src);
    }

    #[test]
    fn preserves_line_comment() {
        let src = "scene {\n  // a leg\n  box \"b\"\n}\n";
        assert_eq!(tidy(src), src);
    }

    #[test]
    fn nested_blocks_indent_two_levels() {
        let src = "scene { group \"g\" { box \"b\" } }";
        let want =
            "scene {\n  group \"g\" {\n    box \"b\"\n  }\n}\n";
        assert_eq!(tidy(src), want);
    }
}
