//! Lossless syntax tree for `.mog` source — the editing model behind the
//! Python API (`mogen-py`) and any other tool that rewrites source files.
//!
//! [`crate::parse`] builds the compiler AST, which drops comments, normalises
//! unit literals (`18in` → `0.4572`) and folds constant expressions. That is
//! right for lowering but wrong for editing: a script that loads a file,
//! tweaks one attribute and saves it must not rewrite the rest of the file.
//!
//! [`parse`] keeps every byte instead. Attribute values are held as their
//! source text, and every node/attribute remembers the whitespace and
//! comments around it plus a snapshot of how it was written. [`print`]
//! re-emits untouched pieces verbatim and only reformats what changed, so
//! `print(parse(src)) == src` for every valid file, and an edit produces a
//! minimal diff.
//!
//! Nodes and attributes created in code carry no snapshot (`orig: None`);
//! they are printed in the house style (`kind "name" (k=v, …) { … }`) with
//! indentation copied from their siblings.

use anyhow::{anyhow, bail, Context, Result};
use pest::iterators::Pair;
use pest::Parser;

use crate::ast::Value;
use crate::parser::{build_attr, DslParser, Rule};

/// A whole `.mog` file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyntaxDoc {
    pub nodes: Vec<SyntaxNode>,
    /// Trivia (whitespace and comments) after the last node.
    pub tail: String,
    /// Indent unit detected from the source, used for nodes created in code.
    pub indent: String,
}

/// One `kind ["name"] [(attrs)] [{ children }]` statement.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxNode {
    pub kind: String,
    /// Unquoted name.
    pub name: Option<String>,
    pub attrs: Vec<SyntaxAttr>,
    /// `None` = no `{ … }` block; `Some(vec![])` = an empty block.
    pub children: Option<Vec<SyntaxNode>>,
    /// Trivia before the node. `None` = choose from the surrounding style.
    pub leading: Option<String>,
    /// How the node was written. `None` for nodes created in code.
    pub orig: Option<NodeOrig>,
}

/// One `key=value` attribute. `value` is source text, e.g. `18in`,
/// `[0.1, $h * 2, 0.1]`, `"oak"`, `linear(from=[1,0,0], to=[0,0,1])`.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxAttr {
    pub key: String,
    pub value: String,
    /// Trivia after the `(` or `,` preceding this attribute.
    pub leading: Option<String>,
    /// Trivia between the value and the following `,`.
    pub trailing: Option<String>,
    pub orig: Option<AttrOrig>,
}

/// Snapshot of a parsed node's header and the trivia inside its brackets.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeOrig {
    kind: String,
    name: Option<String>,
    /// Source text from the kind keyword through the name.
    head: String,
    attr_list: Option<ListTrivia>,
    block: Option<ListTrivia>,
}

#[derive(Debug, Clone, PartialEq)]
struct ListTrivia {
    /// Trivia between the preceding token and the opening bracket.
    pre: String,
    /// Trivia (and an optional trailing comma) before the closing bracket.
    tail: String,
    /// Number of entries the list was parsed with.
    count: usize,
}

/// Snapshot of a parsed attribute.
#[derive(Debug, Clone, PartialEq)]
pub struct AttrOrig {
    key: String,
    value: String,
    /// Source text of the whole attribute, e.g. `name = "chair"`.
    text: String,
    /// Whether it was the first attribute, so its leading trivia is the
    /// padding after `(` rather than after a `,`.
    first: bool,
}

impl SyntaxNode {
    pub fn new(kind: impl Into<String>, name: Option<String>) -> Self {
        Self {
            kind: kind.into(),
            name,
            attrs: Vec::new(),
            children: None,
            leading: None,
            orig: None,
        }
    }

    pub fn attr(&self, key: &str) -> Option<&SyntaxAttr> {
        self.attrs.iter().find(|a| a.key == key)
    }

    /// Set `key` to the value source text `raw`, replacing an existing
    /// attribute in place or appending a new one. `raw` is checked against
    /// the grammar first.
    pub fn set_attr(&mut self, key: &str, raw: &str) -> Result<()> {
        let value = check_value(key, raw)?;
        match self.attrs.iter_mut().find(|a| a.key == key) {
            Some(a) => a.value = value,
            None => self.attrs.push(SyntaxAttr::new(key, value)),
        }
        Ok(())
    }

    pub fn remove_attr(&mut self, key: &str) -> bool {
        let before = self.attrs.len();
        self.attrs.retain(|a| a.key != key);
        self.attrs.len() != before
    }
}

impl SyntaxAttr {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            leading: None,
            trailing: None,
            orig: None,
        }
    }
}

impl SyntaxDoc {
    /// An empty document using the repository's two-space indent.
    pub fn new() -> Self {
        Self { nodes: Vec::new(), tail: "\n".into(), indent: "  ".into() }
    }

    pub fn to_source(&self) -> String {
        print(self).text
    }
}

// --- parsing ----------------------------------------------------------------

/// Parse `src` into a lossless tree. Accepts exactly what [`crate::parse`]
/// accepts.
pub fn parse(src: &str) -> Result<SyntaxDoc> {
    // Run the compiler's parser first so the extra checks it makes beyond
    // the grammar (unit dimensions, list shapes) reject the same files.
    crate::parser::parse(src)?;
    let mut pairs = DslParser::parse(Rule::file, src).context("parse error")?;
    let file = pairs.next().ok_or_else(|| anyhow!("empty parse"))?;
    let mut cursor = 0;
    let mut nodes = Vec::new();
    for p in file.into_inner() {
        if p.as_rule() == Rule::node {
            let leading = src[cursor..p.as_span().start()].to_string();
            let (node, end) = build_node(src, p, leading)?;
            cursor = end;
            nodes.push(node);
        }
    }
    Ok(SyntaxDoc { nodes, tail: src[cursor..].to_string(), indent: detect_indent(src) })
}

/// Returns the node and the byte offset just past its last token. The pest
/// span of a node can run on over trailing trivia when its optional trailing
/// pieces are absent, so the end is computed from the pieces instead.
fn build_node(src: &str, pair: Pair<Rule>, leading: String) -> Result<(SyntaxNode, usize)> {
    let start = pair.as_span().start();
    let mut kind = String::new();
    let mut name = None;
    let mut end = start;
    let mut attrs: Vec<SyntaxAttr> = Vec::new();
    let mut attr_list = None;
    let mut children = None;
    let mut block = None;
    let mut head_end = start;

    for inner in pair.into_inner() {
        let (s, e) = (inner.as_span().start(), inner.as_span().end());
        match inner.as_rule() {
            Rule::ident => {
                kind = inner.as_str().to_string();
                head_end = e;
                end = e;
            }
            Rule::name => {
                let lit = inner.into_inner().next().ok_or_else(|| anyhow!("empty name"))?;
                name = Some(unquote(lit.as_str()));
                head_end = e;
                end = e;
            }
            Rule::attr_list => {
                let pre = src[end..s].to_string();
                let mut cur = s + 1;
                for a in inner.into_inner().filter(|a| a.as_rule() == Rule::attr) {
                    let a_start = a.as_span().start();
                    let gap = &src[cur..a_start];
                    let leading = match attrs.last_mut() {
                        None => gap.to_string(),
                        Some(prev) => {
                            let c = find_comma(gap)
                                .ok_or_else(|| anyhow!("missing `,` between attributes"))?;
                            prev.trailing = Some(gap[..c].to_string());
                            gap[c + 1..].to_string()
                        }
                    };
                    let mut it = a.clone().into_inner();
                    let key = it.next().ok_or_else(|| anyhow!("attr without key"))?;
                    let vp = it.next().ok_or_else(|| anyhow!("attr without value"))?;
                    let v_start = vp.as_span().start();
                    let v_end = sig_end(src, v_start, vp.as_span().end());
                    let value = src[v_start..v_end].to_string();
                    attrs.push(SyntaxAttr {
                        key: key.as_str().to_string(),
                        value: value.clone(),
                        leading: Some(leading),
                        trailing: Some(String::new()),
                        orig: Some(AttrOrig {
                            key: key.as_str().to_string(),
                            value,
                            text: src[a_start..v_end].to_string(),
                            first: attrs.is_empty(),
                        }),
                    });
                    cur = v_end;
                }
                attr_list = Some(ListTrivia {
                    pre,
                    tail: src[cur..e - 1].to_string(),
                    count: attrs.len(),
                });
                end = e;
            }
            Rule::block => {
                let pre = src[end..s].to_string();
                let mut cur = s + 1;
                let mut kids = Vec::new();
                for c in inner.into_inner().filter(|c| c.as_rule() == Rule::node) {
                    let leading = src[cur..c.as_span().start()].to_string();
                    let (child, c_end) = build_node(src, c, leading)?;
                    cur = c_end;
                    kids.push(child);
                }
                block = Some(ListTrivia {
                    pre,
                    tail: src[cur..e - 1].to_string(),
                    count: kids.len(),
                });
                children = Some(kids);
                end = e;
            }
            _ => {}
        }
    }

    let orig = NodeOrig {
        kind: kind.clone(),
        name: name.clone(),
        head: src[start..head_end].to_string(),
        attr_list,
        block,
    };
    let node = SyntaxNode { kind, name, attrs, children, leading: Some(leading), orig: Some(orig) };
    Ok((node, end))
}

/// Offset of the first `,` in `gap` that isn't inside a `//` comment.
fn find_comma(gap: &str) -> Option<usize> {
    let mut i = 0;
    while i < gap.len() {
        let rest = &gap[i..];
        if rest.starts_with("//") {
            i += rest.find('\n').unwrap_or(rest.len());
            continue;
        }
        let ch = rest.chars().next()?;
        if ch == ',' {
            return Some(i);
        }
        i += ch.len_utf8();
    }
    None
}

/// End of the last non-trivia character in `src[start..end]`. Strings are
/// skipped as a unit so `"a // b"` isn't mistaken for a comment.
fn sig_end(src: &str, start: usize, end: usize) -> usize {
    let text = &src[start..end];
    let bytes = text.as_bytes();
    let mut last = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
                last = i;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += text[i..].find('\n').unwrap_or(text.len() - i);
            }
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            _ => {
                // Step a whole UTF-8 character so `last` stays on a boundary.
                let w = text[i..].chars().next().map_or(1, char::len_utf8);
                i += w;
                last = i;
            }
        }
    }
    start + last
}

fn detect_indent(src: &str) -> String {
    for line in src.lines() {
        let trimmed = line.trim_start_matches([' ', '\t']);
        let ws = &line[..line.len() - trimmed.len()];
        if !ws.is_empty() && !trimmed.is_empty() && !trimmed.starts_with("//") {
            return ws.to_string();
        }
    }
    "  ".to_string()
}

// --- printing ---------------------------------------------------------------

/// Printed source plus the byte span of every node, in pre-order (a node
/// before its children, children in order). Lets callers map compiler
/// diagnostics back to the node that produced them.
#[derive(Debug, Clone)]
pub struct Printed {
    pub text: String,
    pub spans: Vec<(usize, usize)>,
}

pub fn print(doc: &SyntaxDoc) -> Printed {
    let indent = if doc.indent.is_empty() { "  " } else { doc.indent.as_str() };
    let mut p = Printer { out: String::new(), spans: Vec::new(), indent };
    p.list(&doc.nodes, 0, "");
    p.out.push_str(&doc.tail);
    Printed { text: p.out, spans: p.spans }
}

struct Printer<'a> {
    out: String,
    spans: Vec<(usize, usize)>,
    indent: &'a str,
}

impl Printer<'_> {
    /// `parent_indent` is the indentation of the line the enclosing node
    /// starts on; new children go one indent unit deeper.
    fn list(&mut self, nodes: &[SyntaxNode], depth: usize, parent_indent: &str) {
        for (i, n) in nodes.iter().enumerate() {
            match &n.leading {
                Some(l) => self.out.push_str(l),
                None => {
                    let l = self.default_leading(nodes, i, depth, parent_indent);
                    self.out.push_str(&l);
                }
            }
            self.node(n, depth);
        }
    }

    /// Whitespace between the last newline and the end of the output, i.e.
    /// the indentation of a node about to be printed.
    fn current_indent(&self) -> String {
        let line = after_last_newline(&self.out);
        if line.chars().all(|c| c == ' ' || c == '\t') {
            line.to_string()
        } else {
            line.chars().take_while(|c| *c == ' ' || *c == '\t').collect()
        }
    }

    fn default_leading(
        &self,
        nodes: &[SyntaxNode],
        i: usize,
        depth: usize,
        parent_indent: &str,
    ) -> String {
        if depth == 0 {
            return if self.out.is_empty() { String::new() } else { "\n\n".into() };
        }
        // Copy the indentation of the nearest sibling that sits on its own line.
        let sibling = nodes[..i]
            .iter()
            .rev()
            .chain(nodes[i + 1..].iter())
            .filter_map(|s| s.leading.as_deref())
            .find(|l| l.contains('\n'));
        match sibling {
            Some(l) => format!("\n{}", after_last_newline(l)),
            None => format!("\n{parent_indent}{}", self.indent),
        }
    }

    fn node(&mut self, n: &SyntaxNode, depth: usize) {
        let idx = self.spans.len();
        let my_indent = self.current_indent();
        self.spans.push((self.out.len(), 0));
        let orig = n.orig.as_ref();

        match orig {
            Some(o) if o.kind == n.kind && o.name == n.name => self.out.push_str(&o.head),
            _ => {
                self.out.push_str(&n.kind);
                if let Some(name) = &n.name {
                    self.out.push(' ');
                    self.out.push_str(&quote(name));
                }
            }
        }

        let list_orig = orig.and_then(|o| o.attr_list.as_ref());
        if !n.attrs.is_empty() {
            self.out.push_str(list_orig.map_or(" ", |l| l.pre.as_str()));
            self.out.push('(');
            let eq = attr_eq_style(&n.attrs);
            for (i, a) in n.attrs.iter().enumerate() {
                if i > 0 {
                    self.out.push_str(n.attrs[i - 1].trailing.as_deref().unwrap_or(""));
                    self.out.push(',');
                }
                // Same-line padding means different things before the first
                // attribute (`( a=1`) and after a comma (`, b=2`); when an
                // attribute has moved between those positions, restyle it.
                let was_first = a.orig.as_ref().is_some_and(|o| o.first);
                let leading = match a.leading.as_deref() {
                    Some(l) if l.contains('\n') || was_first == (i == 0) => l.to_string(),
                    Some(_) | None => default_attr_leading(&n.attrs, i),
                };
                self.out.push_str(&leading);
                match &a.orig {
                    Some(o) if o.key == a.key && o.value == a.value => self.out.push_str(&o.text),
                    _ => {
                        self.out.push_str(&a.key);
                        self.out.push_str(eq);
                        self.out.push_str(&a.value);
                    }
                }
            }
            self.out.push_str(list_orig.map_or("", |l| l.tail.as_str()));
            self.out.push(')');
        } else if let Some(l) = list_orig.filter(|l| l.count == 0) {
            // An empty `()` the author wrote (e.g. `use "x" ()`).
            self.out.push_str(&l.pre);
            self.out.push('(');
            self.out.push_str(&l.tail);
            self.out.push(')');
        }

        if let Some(children) = &n.children {
            let block_orig = orig.and_then(|o| o.block.as_ref());
            self.out.push_str(block_orig.map_or(" ", |b| b.pre.as_str()));
            self.out.push('{');
            self.list(children, depth + 1, &my_indent);
            let closing_line = format!("\n{my_indent}");
            match block_orig {
                Some(b) if b.count == 0 && !children.is_empty() && !b.tail.contains('\n') => {
                    self.out.push_str(&closing_line)
                }
                Some(b) => self.out.push_str(&b.tail),
                None if children.is_empty() => {}
                None => self.out.push_str(&closing_line),
            }
            self.out.push('}');
        }

        self.spans[idx].1 = self.out.len();
    }
}

/// `" = "` if the node's existing attributes are written that way (as `meta`
/// blocks often are), otherwise `"="`.
fn attr_eq_style(attrs: &[SyntaxAttr]) -> &'static str {
    let spaced = attrs
        .iter()
        .filter_map(|a| a.orig.as_ref())
        .find_map(|o| o.text.get(o.key.len()..).map(|r| r.trim_start().len() != r.len()));
    if spaced == Some(true) {
        " = "
    } else {
        "="
    }
}

fn default_attr_leading(attrs: &[SyntaxAttr], i: usize) -> String {
    if i == 0 {
        return String::new();
    }
    // One attribute per line → keep going one per line.
    let multiline = attrs[..i]
        .iter()
        .rev()
        .filter_map(|a| a.leading.as_deref())
        .find(|l| l.contains('\n'));
    match multiline {
        Some(l) => format!("\n{}", after_last_newline(l)),
        None => " ".into(),
    }
}

fn after_last_newline(s: &str) -> &str {
    s.rfind('\n').map_or(s, |i| &s[i + 1..])
}

// --- values -----------------------------------------------------------------

/// True if `s` is a valid identifier (node kind, attribute key, bare value).
pub fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Quote `s` as a `.mog` string literal (inverse of the parser's unquote).
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Format a number as a `.mog` literal. The compiler stores numbers as
/// `f32`, so this prints the shortest text that round-trips through `f32`
/// (`0.14 * cos(120°)` → `-0.07`, not `-0.07000000000000003`) — the
/// compiled output is identical either way. The grammar has no exponent
/// form; Rust's float `Display` never emits one.
pub fn format_number(x: f64) -> Result<String> {
    let f = x as f32;
    if !f.is_finite() {
        bail!("{x} is not a finite number that fits in a .mog literal");
    }
    let f = if f == 0.0 { 0.0 } else { f }; // no `-0`
    Ok(format!("{f}"))
}

/// Check that `raw` is a complete, valid value for attribute `key` and return
/// it trimmed. Rejects anything that would change the surrounding structure
/// (extra attributes, closing brackets, trailing comments).
pub fn check_value(key: &str, raw: &str) -> Result<String> {
    with_probe(key, raw, |attr| build_attr(attr).map(drop))?;
    Ok(raw.trim().to_string())
}

/// Parse `raw` as the value of attribute `key`, exactly as the compiler would.
pub fn parse_value(key: &str, raw: &str) -> Result<Value> {
    with_probe(key, raw, |attr| Ok(build_attr(attr)?.1))
}

fn with_probe<T>(key: &str, raw: &str, f: impl FnOnce(Pair<Rule>) -> Result<T>) -> Result<T> {
    if !is_ident(key) {
        bail!("`{key}` is not a valid attribute name");
    }
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("empty value for `{key}`");
    }
    // The newline before `)` stops a trailing `//` in `raw` from swallowing
    // the bracket; the span check below then rejects the comment itself.
    let probe = format!("__probe ({key}={raw}\n)");
    let value_at = "__probe (".len() + key.len() + 1;
    let invalid = || anyhow!("invalid value for `{key}`: `{raw}`");
    let mut pairs = DslParser::parse(Rule::file, &probe).map_err(|_| invalid())?;
    let file = pairs.next().ok_or_else(invalid)?;
    let nodes: Vec<_> = file.into_inner().filter(|p| p.as_rule() == Rule::node).collect();
    let [node] = nodes.as_slice() else { return Err(invalid()) };
    let parts: Vec<_> = node.clone().into_inner().collect();
    let [_, list] = parts.as_slice() else { return Err(invalid()) };
    if list.as_rule() != Rule::attr_list {
        return Err(invalid());
    }
    let attrs: Vec<_> = list.clone().into_inner().filter(|a| a.as_rule() == Rule::attr).collect();
    let [attr] = attrs.as_slice() else { return Err(invalid()) };
    let vp = attr.clone().into_inner().nth(1).ok_or_else(invalid)?;
    let v_start = vp.as_span().start();
    if v_start != value_at || sig_end(&probe, v_start, vp.as_span().end()) != value_at + raw.len() {
        return Err(invalid());
    }
    f(attr.clone()).map_err(|e| anyhow!("invalid value for `{key}`: {e}"))
}

fn unquote(s: &str) -> String {
    let body = s.strip_prefix('"').unwrap_or(s);
    let body = body.strip_suffix('"').unwrap_or(body);
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(e @ ('"' | '\\')) => out.push(e),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn mog_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.path());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                mog_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "mog") {
                out.push(p);
            }
        }
    }

    #[test]
    fn every_example_round_trips_byte_for_byte() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut files = Vec::new();
        mog_files(&root, &mut files);
        assert!(files.len() > 50, "expected the examples tree, found {}", files.len());
        let mut checked = 0;
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            let Ok(doc) = parse(&src) else { continue }; // deliberately-invalid fixtures
            assert_eq!(print(&doc).text, src, "{} did not round-trip", f.display());
            checked += 1;
        }
        assert!(checked > 50);
    }

    #[test]
    fn untouched_trivia_survives_edits() {
        let src = "// header\nmaterial \"oak\" (color=[0.5, 0.3, 0.1]) // trailing\n\nscene {\n  // legs\n  box \"a\" (size=18in, mat=\"oak\")\n  sphere (radius = 1)\n}\n";
        let mut doc = parse(src).unwrap();
        let scene = &mut doc.nodes[1];
        let a = &mut scene.children.as_mut().unwrap()[0];
        a.set_attr("size", "[1, 2, 3]").unwrap();
        a.set_attr("y", "0.5").unwrap();
        let out = doc.to_source();
        assert_eq!(
            out,
            "// header\nmaterial \"oak\" (color=[0.5, 0.3, 0.1]) // trailing\n\nscene {\n  // legs\n  box \"a\" (size=[1, 2, 3], mat=\"oak\", y=0.5)\n  sphere (radius = 1)\n}\n"
        );
        crate::parse(&out).unwrap();
    }

    #[test]
    fn new_nodes_follow_sibling_indentation() {
        let src = "scene {\n    box (size=1)\n}\n";
        let mut doc = parse(src).unwrap();
        let mut g = SyntaxNode::new("group", Some("legs".into()));
        let mut leg = SyntaxNode::new("cylinder", None);
        leg.set_attr("radius", "0.02").unwrap();
        g.children = Some(vec![leg]);
        doc.nodes[0].children.as_mut().unwrap().push(g);
        assert_eq!(
            doc.to_source(),
            "scene {\n    box (size=1)\n    group \"legs\" {\n        cylinder (radius=0.02)\n    }\n}\n"
        );
    }

    #[test]
    fn built_from_scratch() {
        let mut doc = SyntaxDoc::new();
        let mut mat = SyntaxNode::new("material", Some("oak".into()));
        mat.set_attr("color", "[0.5, 0.3, 0.1]").unwrap();
        let mut scene = SyntaxNode::new("scene", None);
        let mut b = SyntaxNode::new("box", Some("a \"quoted\"".into()));
        b.set_attr("size", "1").unwrap();
        b.set_attr("mat", "\"oak\"").unwrap();
        scene.children = Some(vec![b]);
        doc.nodes = vec![mat, scene];
        let out = doc.to_source();
        assert_eq!(
            out,
            "material \"oak\" (color=[0.5, 0.3, 0.1])\n\nscene {\n  box \"a \\\"quoted\\\"\" (size=1, mat=\"oak\")\n}\n"
        );
        let ast = crate::parse(&out).unwrap();
        assert_eq!(ast[1].children[0].name.as_deref(), Some("a \"quoted\""));
    }

    #[test]
    fn multiline_attr_lists_stay_multiline() {
        let src = "meta (\n  name = \"chair\",\n  version = \"1.0\"\n)\n";
        let mut doc = parse(src).unwrap();
        doc.nodes[0].set_attr("seed", "\"7\"").unwrap();
        doc.nodes[0].set_attr("version", "\"2.0\"").unwrap();
        assert_eq!(
            doc.to_source(),
            "meta (\n  name = \"chair\",\n  version = \"2.0\",\n  seed = \"7\"\n)\n"
        );
    }

    #[test]
    fn removing_attrs_and_children() {
        let src = "scene {\n  box (a=1, b=2)\n  // keep me with sphere\n  sphere\n}\n";
        let mut doc = parse(src).unwrap();
        let kids = doc.nodes[0].children.as_mut().unwrap();
        assert!(kids[0].remove_attr("a"));
        kids[0].kind = "rounded_box".into();
        assert_eq!(doc.to_source(), "scene {\n  rounded_box (b=2)\n  // keep me with sphere\n  sphere\n}\n");
        doc.nodes[0].children.as_mut().unwrap().remove(0);
        assert_eq!(doc.to_source(), "scene {\n  // keep me with sphere\n  sphere\n}\n");
    }

    #[test]
    fn values_keep_units_and_expressions() {
        let src = "module \"leg\" (h=18in) {\n  box (size=[0.1, $h * 2, 0.1], y = $h / 2) // half\n}\n";
        let doc = parse(src).unwrap();
        let b = &doc.nodes[0].children.as_ref().unwrap()[0];
        assert_eq!(b.attr("size").unwrap().value, "[0.1, $h * 2, 0.1]");
        assert_eq!(b.attr("y").unwrap().value, "$h / 2");
        assert_eq!(doc.nodes[0].attr("h").unwrap().value, "18in");
        assert!(matches!(parse_value("h", "18in").unwrap(), Value::Number(n) if (n - 0.4572).abs() < 1e-6));
        assert_eq!(print(&doc).text, src);
    }

    #[test]
    fn check_value_rejects_structure_injection() {
        for bad in ["1) box (", "1, b=2", "1 // c", "", "[1, 2", "5kg"] {
            assert!(check_value("size", bad).is_err(), "accepted {bad:?}");
        }
        assert_eq!(check_value("weight", " 5kg ").unwrap(), "5kg");
        assert_eq!(check_value("axis", "y").unwrap(), "y");
        assert!(check_value("not a key", "1").is_err());
    }

    #[test]
    fn spans_cover_nodes_in_preorder() {
        let src = "scene {\n  group \"g\" {\n    box\n  }\n  sphere\n}\n";
        let p = print(&parse(src).unwrap());
        let texts: Vec<_> = p.spans.iter().map(|&(s, e)| &p.text[s..e]).collect();
        assert_eq!(texts[1], "group \"g\" {\n    box\n  }");
        assert_eq!(texts[2], "box");
        assert_eq!(texts[3], "sphere");
    }

    #[test]
    fn number_formatting() {
        assert_eq!(format_number(1.0).unwrap(), "1");
        assert_eq!(format_number(-0.0).unwrap(), "0");
        assert_eq!(format_number(1e-7).unwrap(), "0.0000001");
        assert_eq!(format_number(0.14 * (120f64).to_radians().cos()).unwrap(), "-0.07");
        assert!(format_number(f64::NAN).is_err());
    }
}
