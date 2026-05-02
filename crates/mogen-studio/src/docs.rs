//! In-app documentation for MoGen Studio.
//!
//! Bundles the four reference markdown files (`docs/dsl.md`, `docs/modules.md`,
//! `docs/cli.md`, `docs/studio.md`) directly into the binary at compile time
//! and exposes:
//!
//! - [`DocPage`] / [`DOC_PAGES`] — the embedded sources, in display order.
//! - [`DocAnchor`] — `(page, slug)` pair pointing at a single section.
//! - [`page_outline`] — section index for a page (extracted on demand from
//!   the markdown's `##` / `###` headings).
//! - [`lookup_topic`] — maps an editor token (`box`, `material`, `mirror`,
//!   `use`, a stdlib module name, …) to the docs section that documents it.
//!   Powers Ctrl+click on a keyword.
//! - [`render_markdown`] — minimal egui markdown renderer (headings, code
//!   fences, inline backticks, bullet lists, paragraphs). Deliberately tiny:
//!   we control the source so we don't need to handle every CommonMark
//!   construct.

use eframe::egui;

/// One bundled markdown reference file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocPage {
    /// Stable slug used in URLs and as the page id in the docs window's
    /// sidebar. Lower-case, no extension (`"dsl"`, `"modules"`, `"cli"`,
    /// `"studio"`).
    pub id: &'static str,
    /// Friendly title shown in the sidebar tab.
    pub title: &'static str,
    /// One-line subtitle shown under the title in the sidebar.
    pub subtitle: &'static str,
    /// The raw markdown source.
    pub source: &'static str,
}

/// Pointer to a particular heading within a page. Used for cross-links and
/// for the editor's Ctrl+click resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocAnchor {
    pub page: &'static str,
    /// `None` means "scroll to the top of the page". `Some(slug)` is a
    /// GitHub-style slug derived from a heading line via [`heading_slug`].
    pub slug: Option<String>,
}

impl DocAnchor {
    pub fn section(page: &'static str, slug: impl Into<String>) -> Self {
        Self {
            page,
            slug: Some(slug.into()),
        }
    }
}

/// One entry in a page's outline. Indentation in the sidebar is driven by
/// `level` (2 = `##`, 3 = `###`).
#[derive(Debug, Clone)]
pub struct OutlineEntry {
    pub level: u8,
    pub title: String,
    pub slug: String,
}

/// Order matches the sidebar listing — DSL first, since most Ctrl+click
/// resolutions land there.
pub const DOC_PAGES: &[DocPage] = &[
    DocPage {
        id: "dsl",
        title: "DSL reference",
        subtitle: "every node kind, attribute, and grammar form",
        source: include_str!("../../../docs/dsl.md"),
    },
    DocPage {
        id: "modules",
        title: "Module catalog",
        subtitle: "stdlib modules invoked via `use`",
        source: include_str!("../../../docs/modules.md"),
    },
    DocPage {
        id: "studio",
        title: "MoGen Studio",
        subtitle: "the desktop GUI you're using right now",
        source: include_str!("../../../docs/studio.md"),
    },
    DocPage {
        id: "cli",
        title: "CLI reference",
        subtitle: "`mogen` command-line subcommands",
        source: include_str!("../../../docs/cli.md"),
    },
];

pub fn page_by_id(id: &str) -> Option<&'static DocPage> {
    DOC_PAGES.iter().find(|p| p.id == id)
}

/// Build the section outline for `page`. Walks the source once and skips
/// headings inside fenced code blocks so a `### example` line in a snippet
/// doesn't sneak into the table of contents.
pub fn page_outline(page: &DocPage) -> Vec<OutlineEntry> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in page.source.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let trimmed = line.trim_start();
        let level = trimmed.bytes().take_while(|b| *b == b'#').count();
        if level == 0 || level > 6 {
            continue;
        }
        let after = &trimmed[level..];
        if !after.starts_with(' ') {
            continue;
        }
        // Top-level `#` is the page title — the sidebar tab already shows it.
        if level == 1 {
            continue;
        }
        let title = after.trim().to_string();
        let slug = heading_slug(&title);
        out.push(OutlineEntry {
            level: level as u8,
            title,
            slug,
        });
    }
    out
}

/// GitHub-style heading slug. Lower-cases, drops backticks and most
/// punctuation, replaces each space with a hyphen, and preserves underscores
/// + literal hyphens in the source. Consecutive separators are kept as-is —
/// `"`union` / `difference`"` becomes `"union--difference"` because the
/// removed `/` leaves two surrounding spaces, each turning into its own
/// hyphen. Matches the hand-rolled tables of contents at the top of the
/// docs files (which were written against the same GitHub algorithm).
pub fn heading_slug(heading: &str) -> String {
    let mut out = String::with_capacity(heading.len());
    for ch in heading.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == ' ' {
            out.push('-');
        } else if ch == '-' || ch == '_' {
            out.push(ch);
        }
        // Everything else (backticks, slashes, colons, commas) is dropped
        // *without* leaving a separator in its place — the spaces around it
        // already supply the separators.
    }
    out
}

/// Map a token from the editor to the documentation section that explains
/// it. Returns `None` when nothing matches — the caller can fall back to
/// the table of contents or a brief "no docs for X" status message.
pub fn lookup_topic(token: &str) -> Option<DocAnchor> {
    let t = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
    if t.is_empty() {
        return None;
    }

    // Stdlib module names — open the catalog page on the relevant heading.
    if STDLIB_MODULE_NAMES.contains(&t) {
        return Some(DocAnchor::section("modules", heading_slug(t)));
    }

    match t {
        // Top-level structure
        "scene" | "group" => Some(DocAnchor::section("dsl", "scene-structure-scene-group")),
        "solid" => Some(DocAnchor::section("dsl", "solid-groups-solid")),
        "lod_scale" => Some(DocAnchor::section("dsl", "global-settings")),

        // Modules
        "module" | "use" => Some(DocAnchor::section("dsl", "modules-module-and-use")),

        // Materials
        "material" => Some(DocAnchor::section("dsl", "materials")),
        | "color" | "alpha" | "metallic" | "roughness"
        | "emissive" | "emissive_strength" | "transmission"
        | "alpha_mode" | "alpha_cutoff" | "double_sided"
        | "uv_mode" | "uv_scale"
        | "base_color_texture" | "metallic_roughness_texture"
        | "normal_texture" | "occlusion_texture" | "emissive_texture"
        | "normal_strength" | "occlusion_strength"
            => Some(DocAnchor::section("dsl", "materials")),

        // Connectors / attach
        "connector" | "attach" => Some(DocAnchor::section("dsl", "connectors")),

        // Replicators
        "mirror" => Some(DocAnchor::section("dsl", "mirror")),
        "array"  => Some(DocAnchor::section("dsl", "array")),
        "stack"  => Some(DocAnchor::section("dsl", "stack")),
        "grid"   => Some(DocAnchor::section("dsl", "grid")),

        // CSG
        "union" | "difference" | "intersect"
            => Some(DocAnchor::section("dsl", "csg-union--difference--intersect")),
        "cleanup" => Some(DocAnchor::section("dsl", "cleanupcoplanar")),

        // Animation
        "joint" | "hinge" | "slider" | "ball" | "rotor"
            => Some(DocAnchor::section("dsl", "joints")),
        "clip" | "track"
            => Some(DocAnchor::section("dsl", "authored-clips")),
        "spin" | "open_close" | "wave" | "flap" | "idle"
            => Some(DocAnchor::section("dsl", "procedural-templates")),
        "skeleton" | "bone" | "skin"
            => Some(DocAnchor::section("dsl", "animation-joint-clip-templates")),

        // Placement shortcuts
        "x" | "y" | "z" | "rx" | "ry" | "rz" | "w" | "h" | "d" | "size"
            => Some(DocAnchor::section("dsl", "per-component-shortcuts")),
        "from" | "to"
            => Some(DocAnchor::section("dsl", "from--to--axis-aligned-box-by-corners")),
        "anchor"
            => Some(DocAnchor::section("dsl", "anchor--place-by-face-not-centre")),
        "above" | "below" | "left_of" | "right_of" | "in_front_of" | "behind" | "gap"
            => Some(DocAnchor::section(
                "dsl",
                "relative-placement-above-below-left_of-right_of-in_front_of-behind",
            )),
        "pos" | "rot" | "scale" | "mat" | "role" | "tags"
            => Some(DocAnchor::section("dsl", "common-attributes")),

        // Primitives — every primitive node kind shares the same heading.
        | "box" | "sphere" | "cylinder" | "cone" | "capsule" | "torus"
        | "prism" | "pyramid" | "disc" | "icosphere" | "rounded_box"
        | "plane" | "quad" | "ellipsoid" | "superellipsoid" | "hemisphere"
        | "frustum" | "tube" | "spline_tube" | "torus_arc" | "half_cylinder"
        | "curved_plane" | "lathe" | "wedge" | "slab" | "post" | "panel"
        | "wall" | "roof"
            => Some(DocAnchor::section("dsl", "primitives")),

        _ => None,
    }
}

/// Stdlib module names recognised by the resolver. Keep aligned with
/// `crates/mogen-dsl/stdlib/*.mog`. Each name corresponds to a `### name`
/// heading in `docs/modules.md`, but we resolve generously to the catalog
/// page even when a heading isn't present yet.
pub const STDLIB_MODULE_NAMES: &[&str] = &[
    "humanoid_head",
    "humanoid_torso",
    "humanoid_arm",
    "humanoid_leg",
    "humanoid_hand_5fingers",
    "quadruped_torso",
    "quadruped_leg",
    "tail",
    "ear",
    "eye",
    "leaf",
    "branch",
    "leg",
    "slab",
    "arm_with_rotor",
];

// -----------------------------------------------------------------------------
// Editor Ctrl+click resolver
// -----------------------------------------------------------------------------

/// What a Ctrl+click on a token should do. Resolved purely from the source
/// + caret offset; the caller dispatches the action against the live UI
/// (open browser, open file, jump caret, open docs window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// External URL (`http://…` / `https://…`). Open in the system browser.
    Url(String),
    /// Path string (relative or absolute). Resolved against the active
    /// file's directory by the dispatcher; `.mog` paths open as a new tab,
    /// everything else hands off to the OS.
    File(String),
    /// `use "name"` referring to a module. The dispatcher checks first if
    /// there's a matching `module "name"` declaration in the same buffer
    /// (jump caret) before falling back to a stdlib docs lookup.
    Module(String),
    /// Generic keyword / attribute / node-kind that resolves to a docs
    /// section.
    Docs(DocAnchor),
}

/// Result of resolving a click position to something actionable.
#[derive(Debug, Clone)]
pub struct LinkHit {
    /// Byte range of the token that was hit. The caller can use it to draw
    /// an underline on Ctrl+hover or to scope a context-menu label. Not
    /// read by the click dispatcher itself, but exposed so the eventual
    /// "Go to definition" right-click menu can reuse the same resolution.
    #[allow(dead_code)]
    pub range: std::ops::Range<usize>,
    pub target: LinkTarget,
}

/// Resolve a click at byte offset `at` in `source` to a [`LinkHit`].
/// Returns `None` when the position lies on whitespace, a number, or
/// anything else that has no meaningful navigation target.
pub fn resolve_link_at(source: &str, at: usize) -> Option<LinkHit> {
    if at > source.len() || !source.is_char_boundary(at) {
        return None;
    }

    // Inside a string literal? Strings can carry URLs and file paths, both
    // of which resolve regardless of context.
    if let Some(string) = enclosing_string(source, at) {
        let inner = &source[string.start..string.end];
        let trimmed = inner.trim();
        if !trimmed.is_empty() {
            if is_url(trimmed) {
                return Some(LinkHit {
                    range: string,
                    target: LinkTarget::Url(trimmed.to_string()),
                });
            }
            if looks_like_path(trimmed) {
                return Some(LinkHit {
                    range: string,
                    target: LinkTarget::File(trimmed.to_string()),
                });
            }
            // Not a URL/path — but if the enclosing call is `use "name"`
            // the string holds a module reference.
            if preceded_by_use_kind(source, string.start) {
                return Some(LinkHit {
                    range: string,
                    target: LinkTarget::Module(trimmed.to_string()),
                });
            }
        }
    }

    // Inside a `// …` line comment? URLs in comments still navigate.
    if let Some(comment) = enclosing_comment(source, at) {
        if let Some(url_range) = find_url_in(source, comment.clone(), at) {
            let url = source[url_range.clone()].to_string();
            return Some(LinkHit {
                range: url_range,
                target: LinkTarget::Url(url),
            });
        }
        // Fall through: comments otherwise don't resolve.
        return None;
    }

    // Identifier hit — node kinds, attribute names, module declarations, etc.
    let word = enclosing_ident(source, at)?;
    let token = &source[word.clone()];
    if let Some(anchor) = lookup_topic(token) {
        return Some(LinkHit {
            range: word,
            target: LinkTarget::Docs(anchor),
        });
    }
    None
}

/// Byte range of the string literal containing `at`, exclusive of the
/// surrounding quotes. `None` when the position isn't inside a string.
/// Tolerates an unterminated string (user mid-edit) by treating end-of-line
/// or end-of-file as the closing boundary.
fn enclosing_string(source: &str, at: usize) -> Option<std::ops::Range<usize>> {
    let bytes = source.as_bytes();
    // Walk from the start of the current line to `at`, tracking whether we
    // are inside a string. If `at` is inside, the start of that string is
    // the most recent unmatched `"`. Then scan forward to find its close.
    let line_start = source[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let mut i = line_start;
    let mut open: Option<usize> = None;
    while i < at {
        let b = bytes[i];
        if b == b'\\' && i + 1 < at {
            i += 2;
            continue;
        }
        if b == b'"' {
            open = if open.is_some() { None } else { Some(i) };
        }
        i += 1;
    }
    let open = open?;
    // Find the closing quote (or EOL / EOF).
    let mut j = at;
    while j < bytes.len() {
        let b = bytes[j];
        if b == b'\\' && j + 1 < bytes.len() {
            j += 2;
            continue;
        }
        if b == b'"' || b == b'\n' {
            break;
        }
        j += 1;
    }
    Some(open + 1..j)
}

/// Byte range of the `// …` comment containing `at` (the `//` is included),
/// `None` when `at` is not inside a comment.
fn enclosing_comment(source: &str, at: usize) -> Option<std::ops::Range<usize>> {
    let line_start = source[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let line_end = source[at..]
        .find('\n')
        .map(|n| at + n)
        .unwrap_or(source.len());
    let line = &source[line_start..line_end];
    // Walk the line to find a `//` not inside a string. Strings aren't
    // common in comment-bearing positions so this is fine to be loose.
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'"' {
            in_str = !in_str;
        }
        if !in_str && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            let comment_start = line_start + i;
            if at >= comment_start {
                return Some(comment_start..line_end);
            }
            return None;
        }
        i += 1;
    }
    None
}

/// Find the URL that contains `at`, looking only inside `range`. Used to
/// pinpoint `https://…` substrings inside `// …` comments. Returns a byte
/// range over the URL itself.
fn find_url_in(
    source: &str,
    range: std::ops::Range<usize>,
    at: usize,
) -> Option<std::ops::Range<usize>> {
    // Split the comment text into whitespace-delimited words and check each
    // for URL-shape. Cheap because comment lines are short.
    let segment = &source[range.clone()];
    let mut idx = 0usize;
    for word in segment.split_whitespace() {
        // Recover the byte offset of `word` inside `segment`.
        let local = match segment[idx..].find(word) {
            Some(p) => idx + p,
            None => break,
        };
        idx = local + word.len();
        let abs = range.start + local..range.start + local + word.len();
        if !abs.contains(&at) && abs.end != at {
            continue;
        }
        // Strip trailing punctuation that often wraps URLs in prose.
        let cleaned = word.trim_end_matches([',', '.', ')', ']', ';', ':']);
        if is_url(cleaned) {
            let trimmed_len = cleaned.len();
            return Some(abs.start..abs.start + trimmed_len);
        }
    }
    None
}

/// Identifier (or `$param`) word containing `at`, returned as a byte range.
fn enclosing_ident(source: &str, at: usize) -> Option<std::ops::Range<usize>> {
    let bytes = source.as_bytes();
    if at >= bytes.len() {
        return None;
    }
    let here = bytes[at];
    // The cursor sits one byte to the right of the just-clicked glyph. If
    // it's not on an ident byte, look one back.
    let mut start = at;
    let mut end = at;
    if !is_ident_byte(here) {
        if at == 0 || !is_ident_byte(bytes[at - 1]) {
            return None;
        }
        start = at - 1;
        end = at;
    }
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    if end == start {
        return None;
    }
    Some(start..end)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Heuristic: does `s` look like an http/https URL? (No need to be pedantic
/// — we only call this on free-form text and on string literals, where a
/// false positive just opens the user's browser to nothing.)
fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Heuristic: does `s` look like a file path? Conservative — we only
/// trigger when the string contains a path separator or a recognised file
/// extension, so a literal like `"top"` (a connector tag) doesn't ask the
/// OS to open a non-existent file.
fn looks_like_path(s: &str) -> bool {
    if s.contains('/') || s.contains('\\') {
        return true;
    }
    // Bare filename with a recognised extension.
    let lower = s.to_ascii_lowercase();
    [
        ".mog", ".png", ".jpg", ".jpeg", ".webp", ".bmp", ".tga", ".glb",
        ".gltf",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

/// True when the string whose inner content begins at `inner_start` is the
/// name argument of a `use "…"` node — i.e. the kind ident immediately to
/// the left of the opening quote is `use`.
fn preceded_by_use_kind(source: &str, inner_start: usize) -> bool {
    if inner_start == 0 {
        return false;
    }
    let bytes = source.as_bytes();
    // The inner range excludes the opening `"`, so step one back to land on
    // it before scanning for the kind ident.
    let mut i = inner_start - 1;
    if bytes.get(i) != Some(&b'"') {
        return false;
    }
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    let end = i;
    while i > 0 && is_ident_byte(bytes[i - 1]) {
        i -= 1;
    }
    if end == i {
        return false;
    }
    &source[i..end] == "use"
}

/// Find a `module "name"` declaration in `source`. Used by the Ctrl+click
/// dispatcher to jump from a `use "name"` site to the matching declaration
/// in the same buffer. Returns the byte offset of the `m` in `module`.
pub fn find_module_decl(source: &str, name: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let needle = format!("module \"{name}\"");
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if source[i..].starts_with(&needle) {
            // Make sure we're at a token boundary so we don't match
            // `submodule` if someone names a kind like that someday.
            let prev_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            if prev_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

// -----------------------------------------------------------------------------
// Markdown renderer
// -----------------------------------------------------------------------------

/// Render `source` into `ui` as a sequence of egui widgets. Supports:
///
/// - `#`/`##`/`###` headings (each registers a scroll anchor named after its
///   slug so the sidebar can jump to it).
/// - Fenced code blocks (````) with a monospaced background block.
/// - Bullet lines starting with `-` or `*`.
/// - Inline `code` (single backticks).
/// - Inline links `[text](url)`.
/// - Paragraph breaks (blank line).
///
/// Tables, images, and HTML blocks are left as plain text — the docs files
/// use a tiny subset of markdown so this is enough.
pub fn render_markdown(ui: &mut egui::Ui, source: &str, scroll_to_slug: Option<&str>) {
    let mut lines = source.lines().peekable();
    let mut paragraph: Vec<String> = Vec::new();
    let mut in_code = false;
    let mut code_buf = String::new();
    let mut requested_scroll = false;

    while let Some(line) = lines.next() {
        // Fenced code block.
        if line.trim_start().starts_with("```") {
            if in_code {
                render_code_block(ui, &code_buf);
                code_buf.clear();
                in_code = false;
            } else {
                flush_paragraph(ui, &mut paragraph);
                in_code = true;
            }
            continue;
        }
        if in_code {
            if !code_buf.is_empty() {
                code_buf.push('\n');
            }
            code_buf.push_str(line);
            continue;
        }

        let trimmed = line.trim_start();

        // Headings.
        let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
        if hashes >= 1 && hashes <= 6 && trimmed.as_bytes().get(hashes) == Some(&b' ') {
            flush_paragraph(ui, &mut paragraph);
            let title = trimmed[hashes + 1..].trim().to_string();
            let slug = heading_slug(&title);
            let anchor_id = egui::Id::new(("doc_anchor", slug.clone()));
            let resp = match hashes {
                1 => ui.add(egui::Label::new(
                    egui::RichText::new(title).heading().strong(),
                )),
                2 => ui.add(egui::Label::new(
                    egui::RichText::new(title).size(20.0).strong(),
                )),
                3 => ui.add(egui::Label::new(
                    egui::RichText::new(title).size(16.0).strong(),
                )),
                _ => ui.add(egui::Label::new(
                    egui::RichText::new(title).size(14.0).strong(),
                )),
            };
            ui.scope(|ui| {
                ui.set_min_size(egui::Vec2::ZERO);
                ui.allocate_rect(resp.rect, egui::Sense::hover())
                    .on_hover_text(format!("#{slug}"));
            });
            // Register a scroll target keyed by the slug so the sidebar can
            // jump to it later.
            ui.ctx().memory_mut(|m| {
                m.data
                    .insert_temp::<egui::Pos2>(anchor_id, resp.rect.left_top());
            });
            if !requested_scroll {
                if let Some(target) = scroll_to_slug {
                    if target == slug {
                        resp.scroll_to_me(Some(egui::Align::TOP));
                        requested_scroll = true;
                    }
                }
            }
            ui.add_space(4.0);
            continue;
        }

        // Horizontal rule.
        if trimmed == "---" {
            flush_paragraph(ui, &mut paragraph);
            ui.separator();
            continue;
        }

        // Bullet line.
        if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            flush_paragraph(ui, &mut paragraph);
            ui.horizontal_wrapped(|ui| {
                ui.label("•");
                render_inline(ui, rest);
            });
            continue;
        }

        // Numbered list item like `1. text`.
        if let Some(after) = strip_numbered_marker(trimmed) {
            flush_paragraph(ui, &mut paragraph);
            ui.horizontal_wrapped(|ui| {
                ui.label("•");
                render_inline(ui, after);
            });
            continue;
        }

        // Blank line ends the current paragraph.
        if trimmed.is_empty() {
            flush_paragraph(ui, &mut paragraph);
            continue;
        }

        // Regular text — accumulate until the next blank.
        paragraph.push(line.to_string());
    }
    if in_code {
        // Unterminated fence — render whatever we have so the user still
        // sees the snippet rather than nothing.
        render_code_block(ui, &code_buf);
    }
    flush_paragraph(ui, &mut paragraph);
}

fn strip_numbered_marker(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i + 1 >= bytes.len() {
        return None;
    }
    if bytes[i] != b'.' && bytes[i] != b')' {
        return None;
    }
    if bytes[i + 1] != b' ' {
        return None;
    }
    Some(&s[i + 2..])
}

fn flush_paragraph(ui: &mut egui::Ui, lines: &mut Vec<String>) {
    if lines.is_empty() {
        return;
    }
    let joined = lines.join(" ");
    lines.clear();
    ui.horizontal_wrapped(|ui| {
        render_inline(ui, &joined);
    });
    ui.add_space(4.0);
}

fn render_code_block(ui: &mut egui::Ui, code: &str) {
    egui::Frame::none()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .rounding(4.0)
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(code)
                        .monospace()
                        .color(ui.visuals().strong_text_color()),
                )
                .selectable(true)
                .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
    ui.add_space(6.0);
}

/// Inline tokens: backticks → monospace chip; `[label](url)` → hyperlink;
/// everything else is plain text.
fn render_inline(ui: &mut egui::Ui, src: &str) {
    let mut i = 0;
    let bytes = src.as_bytes();
    let mut buf = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'`' {
            // Inline code span.
            if !buf.is_empty() {
                ui.label(std::mem::take(&mut buf));
            }
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'`' {
                end += 1;
            }
            let span = &src[start..end];
            ui.add(egui::Label::new(
                egui::RichText::new(span).monospace().background_color(
                    ui.visuals().extreme_bg_color,
                ),
            ));
            i = if end < bytes.len() { end + 1 } else { end };
            continue;
        }
        if b == b'[' {
            // Try to parse `[text](url)` — fall through to plain text on no
            // match so a stray `[` doesn't drop characters.
            if let Some((label, url, consumed)) = parse_link(&src[i..]) {
                if !buf.is_empty() {
                    ui.label(std::mem::take(&mut buf));
                }
                ui.hyperlink_to(label, url);
                i += consumed;
                continue;
            }
        }
        // Append this char to the running plain-text buffer. Walk UTF-8
        // boundaries so multi-byte chars don't panic.
        let end = next_char_boundary(src, i);
        buf.push_str(&src[i..end]);
        i = end;
    }
    if !buf.is_empty() {
        ui.label(buf);
    }
}

fn next_char_boundary(src: &str, i: usize) -> usize {
    let mut j = i + 1;
    while j < src.len() && !src.is_char_boundary(j) {
        j += 1;
    }
    j
}

fn parse_link(s: &str) -> Option<(String, String, usize)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    // Find matching `]`.
    let mut i = 1;
    while i < bytes.len() && bytes[i] != b']' {
        if bytes[i] == b'\n' {
            return None;
        }
        i += 1;
    }
    if i >= bytes.len() || i + 1 >= bytes.len() || bytes[i + 1] != b'(' {
        return None;
    }
    let label = s[1..i].to_string();
    let mut j = i + 2;
    while j < bytes.len() && bytes[j] != b')' {
        if bytes[j] == b'\n' {
            return None;
        }
        j += 1;
    }
    if j >= bytes.len() {
        return None;
    }
    let url = s[i + 2..j].to_string();
    Some((label, url, j + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_loads_and_has_outline() {
        for page in DOC_PAGES {
            assert!(!page.source.is_empty(), "page {} embedded empty", page.id);
            let outline = page_outline(page);
            assert!(
                !outline.is_empty(),
                "page {} produced no outline entries",
                page.id
            );
        }
    }

    #[test]
    fn heading_slug_matches_github_anchors() {
        assert_eq!(heading_slug("Grammar at a glance"), "grammar-at-a-glance");
        assert_eq!(
            heading_slug("CSG: `union` / `difference` / `intersect`"),
            "csg-union--difference--intersect"
        );
        assert_eq!(
            heading_slug("Modules: `module` and `use`"),
            "modules-module-and-use"
        );
        assert_eq!(
            heading_slug("Scene structure: `scene`, `group`"),
            "scene-structure-scene-group"
        );
    }

    #[test]
    fn lookup_topic_resolves_keywords() {
        assert_eq!(
            lookup_topic("box").unwrap(),
            DocAnchor::section("dsl", "primitives")
        );
        assert_eq!(
            lookup_topic("material").unwrap(),
            DocAnchor::section("dsl", "materials")
        );
        assert_eq!(
            lookup_topic("module").unwrap(),
            DocAnchor::section("dsl", "modules-module-and-use")
        );
        assert!(lookup_topic("nonexistent_keyword").is_none());
    }

    #[test]
    fn lookup_topic_resolves_stdlib_modules() {
        let leg = lookup_topic("leg").unwrap();
        assert_eq!(leg.page, "modules");
        let humanoid = lookup_topic("humanoid_head").unwrap();
        assert_eq!(humanoid.page, "modules");
    }

    #[test]
    fn lookup_topic_strips_punctuation() {
        // The Ctrl+click extractor passes the raw word; quotes / parens are
        // common on string-valued attrs.
        assert!(lookup_topic("\"box\"").is_some());
        assert!(lookup_topic("(material)").is_some());
    }

    #[test]
    fn page_outline_skips_code_block_headings() {
        let page = DocPage {
            id: "x",
            title: "x",
            subtitle: "",
            source: "## real\n```\n## fake\n```\n## also-real\n",
        };
        let outline = page_outline(&page);
        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0].title, "real");
        assert_eq!(outline[1].title, "also-real");
    }

    #[test]
    fn parse_link_extracts_label_and_url() {
        let (label, url, consumed) = parse_link("[click](https://x.test) extra").unwrap();
        assert_eq!(label, "click");
        assert_eq!(url, "https://x.test");
        assert_eq!(consumed, "[click](https://x.test)".len());
    }

    #[test]
    fn resolves_keyword_at_caret_to_docs() {
        let src = "scene { box \"b\" (size=[1,1,1]) }";
        // Place caret inside `box`.
        let at = src.find("box").unwrap() + 1;
        let hit = resolve_link_at(src, at).expect("hit");
        assert_eq!(&src[hit.range], "box");
        match hit.target {
            LinkTarget::Docs(a) => {
                assert_eq!(a.page, "dsl");
                assert_eq!(a.slug.as_deref(), Some("primitives"));
            }
            other => panic!("expected Docs, got {other:?}"),
        }
    }

    #[test]
    fn resolves_use_string_to_module() {
        let src = "scene { use \"leg\" (height=0.5) }";
        let at = src.find("leg").unwrap() + 1;
        let hit = resolve_link_at(src, at).expect("hit");
        match hit.target {
            LinkTarget::Module(name) => assert_eq!(name, "leg"),
            other => panic!("expected Module, got {other:?}"),
        }
    }

    #[test]
    fn resolves_url_in_string_literal() {
        let src = "// see https://example.test for details\nscene {}";
        let at = src.find("example").unwrap() + 2;
        let hit = resolve_link_at(src, at).expect("hit");
        match hit.target {
            LinkTarget::Url(url) => assert_eq!(url, "https://example.test"),
            other => panic!("expected Url, got {other:?}"),
        }
    }

    #[test]
    fn resolves_path_in_string_literal() {
        let src = "material \"wood\" (base_color_texture=\"./textures/wood.png\")";
        let at = src.find("wood.png").unwrap() + 2;
        let hit = resolve_link_at(src, at).expect("hit");
        match hit.target {
            LinkTarget::File(p) => assert_eq!(p, "./textures/wood.png"),
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn unknown_token_returns_none() {
        let src = "scene { box \"foobar\" () }";
        let at = src.find("foobar").unwrap() + 2;
        // "foobar" is just a node name string with no path/url shape, so no link.
        assert!(resolve_link_at(src, at).is_none());
    }

    #[test]
    fn finds_local_module_declaration() {
        let src = "module \"leg\" (h=0.5) { box \"b\" () }\nscene { use \"leg\" () }";
        let off = find_module_decl(src, "leg").expect("found");
        assert!(src[off..].starts_with("module \"leg\""));
    }

    #[test]
    fn looks_like_path_rejects_plain_strings() {
        assert!(!looks_like_path("top"));
        assert!(!looks_like_path("seat_back"));
        assert!(looks_like_path("./textures/wood.png"));
        assert!(looks_like_path("textures/wood.png"));
        assert!(looks_like_path("wood.png"));
        assert!(looks_like_path("/abs/path/file.glb"));
    }
}
