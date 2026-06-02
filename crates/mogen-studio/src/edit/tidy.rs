//! Pretty-printer for `.mog` source. Re-indents block bodies (`{`/`}`) and
//! multi-line attribute lists (`(`/`)`) at 2 spaces per level, normalizes the
//! spacing of attribute assignments (`a=1` → `a = 1`) and comma separators
//! (`[1,2]` → `[1, 2]`), strips trailing whitespace, collapses runs of blank
//! lines, and forces a newline after a `{` / before a `}` so blocks lay out
//! predictably — much like a JavaScript beautifier in its default mode.
//!
//! Long inline node argument lists are expanded one-attribute-per-line once
//! they pass a width / count threshold; short lists stay inline. Expansion is
//! one-directional — already-multiline lists are never collapsed back, so the
//! per-attribute inline comments authors rely on survive untouched.
//!
//! Conservative on purpose: string literals and `//` line comments pass
//! through verbatim, the comparison operators `<= >= == !=` are left atomic
//! (never spaced), and only a node's own attribute list is reflowed — the
//! nested parens of an arithmetic group or gradient value are left alone.

/// Width past which an inline node argument list is exploded onto its own
/// lines. Counts the normalized characters of the logical line (indent
/// excluded).
const MAX_INLINE_WIDTH: usize = 80;
/// Attribute count past which an inline node argument list is exploded, even
/// when it would otherwise fit within [`MAX_INLINE_WIDTH`].
const MAX_INLINE_ATTRS: usize = 6;

/// Reformat `src` using brace + paren depth as the indentation guide. Always
/// produces output terminated by a single `\n` when non-empty. Idempotent:
/// `tidy(tidy(s)) == tidy(s)`.
pub fn tidy(src: &str) -> String {
    let logical = split_logical_lines(src);

    // Normalize each line's spacing, then optionally explode an oversized
    // inline attribute list into several logical lines. The brace/paren depth
    // pass below indents whatever falls out.
    let mut prepared: Vec<String> = Vec::with_capacity(logical.len());
    for line in &logical {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            prepared.push(String::new());
            continue;
        }
        let normalized = normalize_spacing(trimmed);
        if let Some(lines) = expand_attr_list_if_long(&normalized) {
            prepared.extend(lines);
        } else if let Some(lines) = split_fragment_attrs(&normalized) {
            prepared.extend(lines);
        } else {
            prepared.push(normalized);
        }
    }

    let mut out = String::with_capacity(src.len() + 32);
    let mut depth_brace: i32 = 0;
    let mut depth_paren: i32 = 0;
    let mut prev_blank = true;
    let mut emitted_any = false;

    for line in &prepared {
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

/// Normalize the inner spacing of a single logical line: put one space on each
/// side of an assignment `=`, one space after a `,` separator (none before),
/// and collapse internal whitespace runs to a single space. String literals
/// and `//` comments pass through verbatim, and the comparison operators
/// `<= >= == !=` are emitted untouched so they are never split or spaced.
fn normalize_spacing(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len() + 8);
    let mut i = 0;
    let mut in_str = false;

    while i < chars.len() {
        let c = chars[i];

        if in_str {
            out.push(c);
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }

        // `//` comment: ensure one leading space, then copy the rest verbatim.
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.extend(&chars[i..]);
            break;
        }

        match c {
            '"' => {
                in_str = true;
                out.push(c);
                i += 1;
            }
            ',' => {
                trim_trailing_spaces(&mut out);
                out.push(',');
                let mut j = i + 1;
                while matches!(chars.get(j), Some(' ' | '\t')) {
                    j += 1;
                }
                // No space before a trailing comma's closer.
                if !matches!(chars.get(j), None | Some(')') | Some(']')) {
                    out.push(' ');
                }
                i = j;
            }
            '=' => {
                let next_eq = chars.get(i + 1) == Some(&'=');
                let prev = last_non_space(&out);
                let part_of_cmp = next_eq || matches!(prev, Some('<' | '>' | '!' | '='));
                if part_of_cmp {
                    out.push('=');
                    i += 1;
                } else {
                    trim_trailing_spaces(&mut out);
                    out.push_str(" = ");
                    let mut j = i + 1;
                    while matches!(chars.get(j), Some(' ' | '\t')) {
                        j += 1;
                    }
                    i = j;
                }
            }
            ' ' | '\t' => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }

    out
}

/// If `line` carries a node's own inline attribute list that exceeds the
/// width / count threshold, explode it into `header (`, one `attr,` per line,
/// and a closing `)` (carrying any trailing ` {` or comment). Returns `None`
/// when the list is absent, short, single-attribute, or unbalanced (a fragment
/// of an already-multiline list). The returned lines are un-indented; the depth
/// pass in [`tidy`] indents them.
fn expand_attr_list_if_long(line: &str) -> Option<Vec<String>> {
    let (open, close) = find_node_attr_parens(line)?;
    let attrs = split_top_level_commas(&line[open + 1..close]);
    if attrs.len() < 2 {
        return None;
    }
    if line.chars().count() <= MAX_INLINE_WIDTH && attrs.len() <= MAX_INLINE_ATTRS {
        return None;
    }

    let mut out = Vec::with_capacity(attrs.len() + 2);
    let mut header = line[..open].to_string();
    header.push('(');
    out.push(header);

    let last = attrs.len() - 1;
    for (idx, attr) in attrs.iter().enumerate() {
        if idx == last {
            out.push(attr.clone());
        } else {
            out.push(format!("{attr},"));
        }
    }

    let mut closer = String::from(")");
    closer.push_str(&line[close + 1..]);
    out.push(closer.trim_end().to_string());
    Some(out)
}

/// Break a fragment of an already-multiline attribute list that packs several
/// attributes onto one physical line (`a = 1, b = 2, c = 3`) into one
/// attribute per line. A depth-0 comma (outside `[ ]`, `( )` and strings) can
/// only separate sibling attributes — node argument lists are the sole source
/// of commas and their own parens live on an earlier line — so its presence is
/// a reliable fragment signal. A trailing `,` separator and a closing line
/// comment are reattached to the last emitted attribute. Returns `None` when
/// the line holds at most one attribute (nothing to break).
fn split_fragment_attrs(line: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = line.chars().collect();
    let mut fields: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut comment: Option<String> = None;
    let mut in_str = false;
    let mut bracket = 0i32;
    let mut paren = 0i32;
    let mut trailing_comma = false;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if in_str {
            cur.push(c);
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            comment = Some(chars[i..].iter().collect());
            break;
        }
        match c {
            '"' => {
                in_str = true;
                cur.push(c);
            }
            '[' => {
                bracket += 1;
                cur.push(c);
            }
            ']' => {
                bracket -= 1;
                cur.push(c);
            }
            '(' => {
                paren += 1;
                cur.push(c);
            }
            ')' => {
                paren -= 1;
                cur.push(c);
            }
            ',' if bracket == 0 && paren == 0 => {
                let t = cur.trim();
                if !t.is_empty() {
                    fields.push(t.to_string());
                    trailing_comma = true;
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    let t = cur.trim();
    if !t.is_empty() {
        fields.push(t.to_string());
        trailing_comma = false;
    }

    if fields.len() < 2 {
        return None;
    }

    let last = fields.len() - 1;
    let mut out = Vec::with_capacity(fields.len());
    for (idx, field) in fields.iter().enumerate() {
        let mut s = field.clone();
        if idx < last || trailing_comma {
            s.push(',');
        }
        if idx == last {
            if let Some(cmt) = &comment {
                s.push(' ');
                s.push_str(cmt);
            }
        }
        out.push(s);
    }
    Some(out)
}

/// Locate a node's own attribute-list parentheses: the first top-level `(` that
/// is preceded only by a header (no `=` or `[` ahead of it, outside strings),
/// paired with its matching `)`. The header guard keeps us off the nested
/// parens of a gradient value or an arithmetic group, which always sit after a
/// `=`. Returns byte offsets of the `(` and `)`.
fn find_node_attr_parens(line: &str) -> Option<(usize, usize)> {
    let mut in_str = false;
    let mut open = None;
    for (idx, c) in line.char_indices() {
        if in_str {
            if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '=' | '[' => return None,
            '(' => {
                open = Some(idx);
                break;
            }
            _ => {}
        }
    }
    let open = open?;

    let mut depth = 0i32;
    let mut in_str = false;
    for (rel, c) in line[open..].char_indices() {
        if in_str {
            if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, open + rel));
                }
            }
            _ => {}
        }
    }
    None
}

/// Split attribute-list inner text on the commas that sit at the top level —
/// ignoring commas nested inside `[ ]` vectors, `( )` groups, or string
/// literals. Each returned attribute is trimmed; an empty trailing field (from
/// a trailing comma) is dropped.
fn split_top_level_commas(inner: &str) -> Vec<String> {
    let mut res = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut bracket = 0i32;
    let mut paren = 0i32;

    for c in inner.chars() {
        if in_str {
            cur.push(c);
            if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                cur.push(c);
            }
            '[' => {
                bracket += 1;
                cur.push(c);
            }
            ']' => {
                bracket -= 1;
                cur.push(c);
            }
            '(' => {
                paren += 1;
                cur.push(c);
            }
            ')' => {
                paren -= 1;
                cur.push(c);
            }
            ',' if bracket == 0 && paren == 0 => {
                let t = cur.trim();
                if !t.is_empty() {
                    res.push(t.to_string());
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        res.push(t.to_string());
    }
    res
}

/// Last non-space character already emitted, used to tell an assignment `=`
/// from the tail of a `<= >= != ==` comparison operator.
fn last_non_space(s: &str) -> Option<char> {
    s.chars().rev().find(|c| *c != ' ' && *c != '\t')
}

/// Pop trailing spaces/tabs from `out` so the next token can supply its own.
fn trim_trailing_spaces(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
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
    use super::{normalize_spacing, tidy};

    #[test]
    fn idempotent_on_clean_source() {
        let src = "scene {\n  box \"b\" (size = [1, 1, 1])\n}\n";
        assert_eq!(tidy(src), src);
        assert_eq!(tidy(&tidy(src)), tidy(src));
    }

    #[test]
    fn splits_inline_block() {
        let src = "scene { box \"b\" (size=1) }";
        let want = "scene {\n  box \"b\" (size = 1)\n}\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn reindents_misaligned_block() {
        let src = "scene {\nbox \"b\" (size=1)\n      cylinder \"c\" (radius=1)\n}\n";
        let want = "scene {\n  box \"b\" (size = 1)\n  cylinder \"c\" (radius = 1)\n}\n";
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
        let want = "scene {\n  group \"g\" {\n    box \"b\"\n  }\n}\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn normalizes_assignment_and_comma_spacing() {
        assert_eq!(
            normalize_spacing("box \"b\" (size=[1,2,3],pos=[0,0,0])"),
            "box \"b\" (size = [1, 2, 3], pos = [0, 0, 0])"
        );
    }

    #[test]
    fn leaves_comparison_operators_atomic() {
        assert_eq!(normalize_spacing("x>=1"), "x>=1");
        assert_eq!(normalize_spacing("a == b"), "a == b");
        assert_eq!(normalize_spacing("a != b"), "a != b");
    }

    #[test]
    fn collapses_internal_runs_but_keeps_comment() {
        assert_eq!(
            normalize_spacing("seed=12,     // note"),
            "seed = 12, // note"
        );
    }

    #[test]
    fn expands_wide_inline_list() {
        let src = "material \"m\" (color=[0.4, 0.37, 0.32], roughness=0.95, metallic=0.1, base_color_texture=\"a/very/long/path/to/an/albedo/texture.png\")\n";
        let want = "material \"m\" (\n  color = [0.4, 0.37, 0.32],\n  roughness = 0.95,\n  metallic = 0.1,\n  base_color_texture = \"a/very/long/path/to/an/albedo/texture.png\"\n)\n";
        assert_eq!(tidy(src), want);
        assert_eq!(tidy(&tidy(src)), tidy(src));
    }

    #[test]
    fn expands_when_attr_count_exceeds_threshold() {
        let src = "n \"x\" (a=1, b=2, c=3, d=4, e=5, f=6, g=7)\n";
        let want =
            "n \"x\" (\n  a = 1,\n  b = 2,\n  c = 3,\n  d = 4,\n  e = 5,\n  f = 6,\n  g = 7\n)\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn short_list_stays_inline() {
        let src = "feature \"d\" (kind=stalactite, min_size=0.25, max_size=0.7)\n";
        let want = "feature \"d\" (kind = stalactite, min_size = 0.25, max_size = 0.7)\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn expands_inline_list_with_block_suffix() {
        let src = "cave \"h\" (seed=1, size=[64, 40, 64], chambers=20, levels=4, level_gap=2, level_links=2, loops=2) { feature \"d\" (kind=stalactite) }";
        let want = "cave \"h\" (\n  seed = 1,\n  size = [64, 40, 64],\n  chambers = 20,\n  levels = 4,\n  level_gap = 2,\n  level_links = 2,\n  loops = 2\n) {\n  feature \"d\" (kind = stalactite)\n}\n";
        assert_eq!(tidy(src), want);
        assert_eq!(tidy(&tidy(src)), tidy(src));
    }

    #[test]
    fn splits_packed_fragment_into_one_per_line() {
        let src = "cave \"h\" (\n  pools = 3,\n  mushrooms = 24, debug_show_poi = 1, colliders = \"all\" // markers\n)\n";
        let want = "cave \"h\" (\n  pools = 3,\n  mushrooms = 24,\n  debug_show_poi = 1,\n  colliders = \"all\" // markers\n)\n";
        assert_eq!(tidy(src), want);
        assert_eq!(tidy(&tidy(src)), tidy(src));
    }

    #[test]
    fn keeps_trailing_comma_when_fragment_ends_with_one() {
        let src = "cave \"h\" (\n  a = 1, b = 2,\n  c = 3\n)\n";
        let want = "cave \"h\" (\n  a = 1,\n  b = 2,\n  c = 3\n)\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn fragment_split_ignores_commas_in_vectors() {
        let src = "cave \"h\" (\n  size = [64, 40, 64], chambers = 20\n)\n";
        let want = "cave \"h\" (\n  size = [64, 40, 64],\n  chambers = 20\n)\n";
        assert_eq!(tidy(src), want);
    }

    #[test]
    fn does_not_collapse_existing_multiline_list() {
        // Expansion is one-directional: an already-multiline list keeps its
        // line breaks (and per-line comments) rather than being folded back
        // onto one line. Alignment padding before a comment is still collapsed.
        let src = "cave \"h\" (\n  seed = 1,            // pinned\n  size = [64, 40, 64]\n)\n";
        let want = "cave \"h\" (\n  seed = 1, // pinned\n  size = [64, 40, 64]\n)\n";
        assert_eq!(tidy(src), want);
        assert_eq!(tidy(&tidy(src)), tidy(src));
    }
}
