//! Context-aware completion provider for the `.mog` editor.
//!
//! Given a source buffer and a caret byte offset, `compute_completions`
//! figures out whether the user is typing a node kind, an attribute name,
//! or an attribute value, and returns a ranked list of candidates plus the
//! byte range the suggestion should replace on accept. It is intentionally
//! loose — an unfinished buffer mid-edit doesn't parse, so context detection
//! is a brace/quote walk, not a pest pass.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    NodeKind,
    Attribute,
    EnumValue,
    Material,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub label: String,
    pub kind: CandidateKind,
    /// What gets spliced into the source in place of the prefix.
    pub insert: String,
    /// Optional one-line hint shown to the right of `label`.
    pub detail: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub struct Completions {
    /// Byte range of the word the popup is completing — replaced on accept.
    pub range: Range<usize>,
    pub candidates: Vec<Candidate>,
}

/// Context we inferred at the caret position.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Context {
    /// Typing a new node at top-level or inside `{ … }`.
    NodeKind,
    /// Inside `(…)` attr list, on the key side.
    AttrKey { parent_kind: Option<String> },
    /// Inside `(…)` attr list, on the value side of `key=…`.
    AttrValue { attr: String, parent_kind: Option<String> },
    /// Don't offer completions (string, comment, inside a list literal, …).
    Skip,
}

pub fn compute_completions(
    src: &str,
    caret: usize,
    materials: &[String],
) -> Option<Completions> {
    let caret = caret.min(src.len());
    if !src.is_char_boundary(caret) {
        return None;
    }
    let (word_start, word_end) = word_bounds(src, caret);
    let prefix = &src[word_start..caret];
    if prefix.is_empty() {
        return None;
    }
    // Must begin with an identifier-starting char — otherwise it's punctuation
    // or a number and completion has nothing useful to offer.
    if !prefix
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false)
    {
        return None;
    }

    let ctx = infer_context(src, word_start);
    let pool: Vec<Candidate> = match ctx {
        Context::NodeKind => node_kind_pool(),
        Context::AttrKey { parent_kind } => attr_key_pool(parent_kind.as_deref()),
        Context::AttrValue { attr, parent_kind } => {
            attr_value_pool(&attr, parent_kind.as_deref(), materials)
        }
        Context::Skip => return None,
    };

    let cands = rank_and_filter(pool, prefix);
    if cands.is_empty() {
        return None;
    }
    Some(Completions {
        range: word_start..word_end,
        candidates: cands,
    })
}

/// Extend the current identifier/`$param` word around `caret` (byte offset).
fn word_bounds(src: &str, caret: usize) -> (usize, usize) {
    let bytes = src.as_bytes();
    let mut start = caret;
    while start > 0 {
        let b = bytes[start - 1];
        if is_ident_byte(b) || b == b'$' {
            start -= 1;
            // `$` only attaches at the very front of the word.
            if b == b'$' {
                break;
            }
        } else {
            break;
        }
    }
    let mut end = caret;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    (start, end)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Walk backward from `word_start` tracking brackets + quotes to decide
/// whether we're in a node-kind position, an attr key, or an attr value.
/// String and comment interiors are marked Skip.
fn infer_context(src: &str, word_start: usize) -> Context {
    if in_string_or_comment(src, word_start) {
        return Context::Skip;
    }

    // Scan the nearest enclosing bracket by walking backward, tracking only
    // matched pairs we encounter along the way.
    let bytes = src.as_bytes();
    let mut i = word_start;
    let mut paren_depth = 0i32;
    let mut bracket_depth = 0i32;
    let mut brace_depth = 0i32;

    // Walk backward, collapsing strings/comments and matched pairs, until we
    // hit an unmatched opener.
    while i > 0 {
        i -= 1;
        // Skip comment rest-of-line when we detect a `//` start on this line
        // by scanning the line from its beginning — cheap since `.mog` lines
        // are short.
        let b = bytes[i];
        match b {
            b')' => paren_depth += 1,
            b']' => bracket_depth += 1,
            b'}' => brace_depth += 1,
            b'(' => {
                if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 {
                    // Unmatched open paren — we're inside an attr list.
                    let parent = find_parent_kind_before(src, i);
                    return classify_inside_parens(src, i, word_start, parent);
                }
                paren_depth -= 1;
            }
            b'[' => {
                if bracket_depth == 0 && paren_depth == 0 && brace_depth == 0 {
                    // Inside a list/vec literal — no meaningful keyword
                    // completion (just numbers / nested lists).
                    return Context::Skip;
                }
                bracket_depth -= 1;
            }
            b'{' => {
                if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                    // Inside a block body — expect a node kind.
                    return Context::NodeKind;
                }
                brace_depth -= 1;
            }
            b'"' => {
                // Back up over the string to its opening quote. If this is
                // actually a closing quote for a string that started earlier,
                // that means the span we just crossed is a string literal and
                // has no effect on bracket depths.
                let mut j = i;
                while j > 0 {
                    j -= 1;
                    if bytes[j] == b'"' {
                        // Respect backslash escapes.
                        let mut bs = 0;
                        let mut k = j;
                        while k > 0 && bytes[k - 1] == b'\\' {
                            bs += 1;
                            k -= 1;
                        }
                        if bs % 2 == 0 {
                            break;
                        }
                    } else if bytes[j] == b'\n' {
                        // Unterminated string — stop here and let the outer
                        // loop continue from before it.
                        break;
                    }
                }
                i = j;
            }
            _ => {}
        }
    }

    // Reached the start of file with no unmatched opener → node-kind context.
    Context::NodeKind
}

/// Given that `open_paren` is the byte index of an unmatched `(`, figure out
/// whether `word_start` sits on the key or value side of a `key = value`.
fn classify_inside_parens(
    src: &str,
    open_paren: usize,
    word_start: usize,
    parent_kind: Option<String>,
) -> Context {
    // Scan forward from `open_paren + 1` to `word_start`, finding the last
    // top-level delimiter — `,` or `(` puts us on a key; `=` puts us on a
    // value. Nested `[ … ]` (vec/list) and `"…"` strings are skipped.
    let bytes = src.as_bytes();
    let mut last_marker: u8 = b'(';
    let mut last_key: Option<String> = None;
    let mut cur_key_start: Option<usize> = Some(open_paren + 1);
    let mut i = open_paren + 1;
    let mut bracket_depth = 0i32;

    while i < word_start {
        let b = bytes[i];
        match b {
            b'[' => bracket_depth += 1,
            b']' => {
                if bracket_depth > 0 {
                    bracket_depth -= 1;
                }
            }
            b'"' => {
                i += 1;
                while i < word_start && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < word_start {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'\n' {
                        break;
                    }
                    i += 1;
                }
            }
            b'=' if bracket_depth == 0 => {
                // Everything between cur_key_start and here is the key ident.
                if let Some(start) = cur_key_start {
                    let key = src[start..i].trim();
                    if !key.is_empty() {
                        last_key = Some(key.trim_end_matches('=').trim().to_string());
                    }
                }
                last_marker = b'=';
                cur_key_start = None;
            }
            b',' if bracket_depth == 0 => {
                last_marker = b',';
                cur_key_start = Some(i + 1);
                last_key = None;
            }
            _ => {}
        }
        i += 1;
    }

    match last_marker {
        b'(' | b',' => Context::AttrKey { parent_kind },
        b'=' => {
            if let Some(k) = last_key {
                Context::AttrValue {
                    attr: k,
                    parent_kind,
                }
            } else {
                Context::AttrKey { parent_kind }
            }
        }
        _ => Context::AttrKey { parent_kind },
    }
}

/// Locate the node kind ident immediately preceding `paren_at`, skipping
/// over an optional `"name"` literal. Returns `None` when we can't find one
/// (file start, malformed source, etc.).
fn find_parent_kind_before(src: &str, paren_at: usize) -> Option<String> {
    let bytes = src.as_bytes();
    let mut i = paren_at;
    // Skip whitespace.
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    // Optional quoted name.
    if i > 0 && bytes[i - 1] == b'"' {
        // Back up to the opening quote.
        i -= 1;
        while i > 0 {
            i -= 1;
            if bytes[i] == b'"' {
                break;
            }
        }
        // Skip whitespace between name and kind.
        while i > 0 && bytes[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
    }
    // Now the ident should end at `i`.
    let end = i;
    while i > 0 && is_ident_byte(bytes[i - 1]) {
        i -= 1;
    }
    if end == i {
        return None;
    }
    Some(src[i..end].to_string())
}

/// Byte offset inside a `//` comment or a `"…"` string literal?
fn in_string_or_comment(src: &str, at: usize) -> bool {
    // Scan from the start of the current line to `at`. Cheap — lines are short.
    let line_start = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let mut i = line_start;
    let bytes = src.as_bytes();
    let mut in_string = false;
    while i < at {
        let b = bytes[i];
        if in_string {
            if b == b'\\' && i + 1 < at {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < at && bytes[i + 1] == b'/' {
            return true;
        }
        if b == b'"' {
            in_string = true;
        }
        i += 1;
    }
    in_string
}

/// Rank candidates against the typed prefix: exact prefix matches first,
/// case-insensitive prefix next, then substring hits. Within each tier, keep
/// the pool's original order so curated priorities (e.g. `box` before
/// `box_alias`) survive.
fn rank_and_filter(pool: Vec<Candidate>, prefix: &str) -> Vec<Candidate> {
    let lower = prefix.to_ascii_lowercase();
    let mut tiers: [Vec<Candidate>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for c in pool {
        let label_lower = c.label.to_ascii_lowercase();
        if c.label.starts_with(prefix) {
            tiers[0].push(c);
        } else if label_lower.starts_with(&lower) {
            tiers[1].push(c);
        } else if label_lower.contains(&lower) {
            tiers[2].push(c);
        }
    }
    let mut out = Vec::with_capacity(tiers.iter().map(|t| t.len()).sum::<usize>());
    for mut t in tiers {
        out.append(&mut t);
    }
    // Cap the popup length so the panel doesn't swallow the viewport on a
    // single-character prefix.
    const MAX: usize = 12;
    out.truncate(MAX);
    out
}

// -----------------------------------------------------------------------------
// Candidate pools
// -----------------------------------------------------------------------------

fn node_kind_pool() -> Vec<Candidate> {
    const KINDS: &[(&str, &str)] = &[
        ("scene", "root container"),
        ("group", "transform-only container"),
        ("solid", "group whose leaves CSG-merge at export"),
        ("module", "declare a parametric sub-graph"),
        ("use", "invoke a declared module"),
        ("material", "declare a PBR material"),
        ("connector", "oriented attach frame"),
        ("attach", "connect two connector tags"),
        // Primitives
        ("box", "axis-aligned box"),
        ("plane", "XZ-aligned flat quad"),
        ("quad", "XY-aligned flat quad"),
        ("cylinder", "cylinder along +Y"),
        ("cone", "cone along +Y"),
        ("sphere", "uv-sphere"),
        ("capsule", "capped cylinder"),
        ("torus", "donut"),
        ("prism", "triangular prism"),
        ("pyramid", "N-sided pyramid"),
        ("disc", "flat disc"),
        ("icosphere", "geodesic sphere"),
        ("rounded_box", "box with rounded corners"),
        ("ellipsoid", "per-axis sphere"),
        ("superellipsoid", "soft/sharp box family"),
        ("hemisphere", "half sphere"),
        ("frustum", "truncated cone"),
        ("tube", "hollow cylinder"),
        ("spline_tube", "tube along a Catmull-Rom curve"),
        ("torus_arc", "partial torus"),
        ("half_cylinder", "half cylinder"),
        ("curved_plane", "bent plane"),
        ("lathe", "revolve a 2D profile"),
        ("wedge", "triangular wedge"),
        ("slab", "box alias, anchor=bottom"),
        ("post", "box alias, anchor=bottom"),
        ("panel", "box alias, anchor=back"),
        ("wall", "wall with cutouts"),
        ("roof", "roof shell"),
        ("decal", "transparent image overlay (logo, label, handwriting)"),
        ("cave", "traversable multi-floor cave"),
        ("feature", "tune a cave decoration kind (inside cave)"),
        // CSG / replicators
        ("union", "CSG union of children"),
        ("difference", "CSG first minus rest"),
        ("intersect", "CSG intersection"),
        ("array", "rotational clone"),
        ("mirror", "axis-mirrored clone"),
        ("stack", "lay children along an axis"),
        ("grid", "N-D grid clone"),
        // Animation / skinning
        ("joint", "articulation (hinge/slider/ball)"),
        ("skeleton", "bone hierarchy"),
        ("bone", "bone inside a skeleton"),
        ("clip", "animation clip"),
        ("track", "single-channel track"),
        ("skin", "bind mesh to skeleton"),
        ("spin", "continuous rotation template"),
        ("open_close", "0° → angle → 0° swing"),
        ("wave", "sinusoidal wobble"),
        ("flap", "fast large wobble"),
        ("idle", "tiny breathing translate/scale"),
    ];
    KINDS
        .iter()
        .map(|(k, d)| Candidate {
            label: (*k).to_string(),
            kind: CandidateKind::NodeKind,
            insert: (*k).to_string(),
            detail: Some(*d),
        })
        .collect()
}

/// Common attributes for any geometry/group node. Always included.
const COMMON_ATTRS: &[(&str, &str)] = &[
    ("pos", "vec3 translation"),
    ("rot", "vec3 Euler XYZ (deg)"),
    ("scale", "scalar or vec3"),
    ("size", "vec3 dimensions"),
    ("x", "pos.x shortcut"),
    ("y", "pos.y shortcut"),
    ("z", "pos.z shortcut"),
    ("rx", "rot.x shortcut"),
    ("ry", "rot.y shortcut"),
    ("rz", "rot.z shortcut"),
    ("w", "size.x shortcut"),
    ("h", "size.y shortcut"),
    ("d", "size.z shortcut"),
    ("mat", "material reference"),
    ("role", "semantic label"),
    ("tags", "free-form labels"),
    ("anchor", "center|top|bottom|left|right|front|back"),
    ("from", "vec3 — corner of axis-aligned box"),
    ("to", "vec3 — opposite corner"),
    ("above", "sibling name — stack above"),
    ("below", "sibling name — stack below"),
    ("left_of", "sibling name — place left of"),
    ("right_of", "sibling name — place right of"),
    ("in_front_of", "sibling name — place in front of"),
    ("behind", "sibling name — place behind"),
    ("gap", "spacing for relative placement"),
    ("skin", "bind mesh to skeleton name"),
];

fn attr_key_pool(parent_kind: Option<&str>) -> Vec<Candidate> {
    // Kind-specific attrs (not including the common set, which is appended).
    let kind_specific: &[(&str, &str)] = match parent_kind.unwrap_or("") {
        "material" => &[
            ("color", "[r, g, b]"),
            ("alpha", "0..1"),
            ("metallic", "0..1"),
            ("roughness", "0..1"),
            ("emissive", "[r, g, b]"),
            ("emissive_strength", "HDR multiplier"),
            ("transmission", "0..1"),
            ("alpha_mode", "opaque|blend|mask"),
            ("alpha_cutoff", "0..1"),
            ("double_sided", "0|1"),
            ("uv_mode", "tile|fit"),
            ("uv_scale", "scalar or vec2"),
            ("base_color_texture", "path to sRGB PNG/JPG"),
            ("metallic_roughness_texture", "linear"),
            ("normal_texture", "tangent-space"),
            ("occlusion_texture", "red channel"),
            ("emissive_texture", "sRGB"),
        ],
        "connector" => &[
            ("at", "vec3 position"),
            ("dir", "vec3 direction"),
            ("tag", "string or ident"),
            ("radius", "optional hint radius"),
        ],
        "cylinder" | "cone" | "capsule" | "tube" | "half_cylinder" | "hemisphere" | "frustum" => &[
            ("radius", "bottom radius"),
            ("height", "axial height"),
            ("segments", "radial segments (24)"),
        ],
        "sphere" | "icosphere" | "disc" => &[
            ("radius", "sphere/disc radius"),
            ("rings", "latitude rings (16)"),
            ("segments", "longitude segments (24)"),
            ("subdivisions", "icosphere subdiv (2)"),
        ],
        "torus" | "torus_arc" => &[
            ("major", "ring radius"),
            ("minor", "tube radius"),
            ("major_segments", "24"),
            ("minor_segments", "12"),
            ("arc", "partial torus angle°"),
        ],
        "pyramid" => &[
            ("radius", "base radius"),
            ("height", "tip height"),
            ("sides", "N-gon base"),
        ],
        "rounded_box" => &[
            ("radius", "corner radius"),
            ("segments", "subdivisions per corner (4)"),
        ],
        "ellipsoid" | "superellipsoid" => &[
            ("rings", "16"),
            ("segments", "24"),
            ("ew", "east-west exponent"),
            ("ns", "north-south exponent"),
        ],
        "curved_plane" => &[
            ("bend_u", "arc along X°"),
            ("bend_v", "arc along Z°"),
            ("segments_u", "12"),
            ("segments_v", "12"),
        ],
        "lathe" => &[
            ("profile", "[[r, y], …]"),
            ("segments", "24"),
            ("cap_ends", "0|1"),
        ],
        "spline_tube" => &[
            ("points", "[[x, y, z], …]"),
            ("radius", "scalar"),
            ("radii", "per-point radii list"),
            ("segments", "radial (12)"),
            ("samples", "along-curve (8)"),
            ("cap_ends", "0|1"),
        ],
        "wall" => &[
            ("holes", "[[cx, cy, w, h], …]"),
        ],
        "cave" => &[
            ("seed", "deterministic generation seed"),
            ("chambers", "number of chambers (rooms)"),
            ("levels", "stacked vertical floors (1)"),
            ("level_gap", "solid rock between floors"),
            ("level_links", "vertical ramps joining floors"),
            ("chamber_min", "smallest chamber radius"),
            ("chamber_max", "largest chamber radius"),
            ("spacing", "min gap between chambers"),
            ("overlap", "0..1 fraction that merge into caverns"),
            ("chamber_flatten", "vertical squash of chambers"),
            ("passage_radius", "tunnel radius"),
            ("loops", "extra non-tree passages"),
            ("max_slope", "steepest walkable passage°"),
            ("roughness", "wall noise amount"),
            ("blend", "SDF smoothing radius"),
            ("margin", "rock padding around the block"),
            ("resolution", "voxel grid resolution"),
            ("lod_scale", "0.1..1.0 mesh-quality / triangle budget"),
            ("entrances", "surface mouth count, or per-band array [b0,b1,…]"),
            ("stalagmites", "floor spike count"),
            ("stalactites", "ceiling spike count"),
            ("columns", "floor-to-ceiling stone pillars"),
            ("rock_piles", "rubble heap count"),
            ("pools", "small water pool count"),
            ("lakes", "large water body count"),
            ("mushrooms", "floor POI marker count"),
            ("mat_style", "rock material name"),
            ("water_mat", "water material name"),
            ("debug_hide_shell", "0|1 — strip outer hull to see interior"),
            ("debug_show_poi", "0|1 — show POI markers as debug spheres"),
        ],
        "feature" => &[
            ("kind", "stalagmite|stalactite|column|rock_pile|pool|lake"),
            ("count", "override the top-level count"),
            ("min_size", "smallest instance size"),
            ("max_size", "largest instance size"),
            ("mat", "material reference"),
        ],
        "array" => &[
            ("count", "copies"),
            ("around", "rotation axis x|y|z"),
            ("start_angle", "first-copy offset°"),
        ],
        "mirror" => &[("axis", "x|y|z")],
        "stack" => &[
            ("axis", "x|y|z (default y)"),
            ("gap", "inter-child spacing"),
            ("align", "center|start|end"),
            ("pack", "start|center|end"),
        ],
        "grid" => &[
            ("count", "scalar, [nx, nz], or [nx, ny, nz]"),
            ("step", "matching vec/scalar"),
            ("center", "0|1"),
        ],
        "solid" => &[("cleanup", "coplanar|none")],
        "joint" => &[
            ("type", "hinge|slider|ball|rotor"),
            ("pivot", "node name"),
            ("axis", "vec3"),
            ("limits", "[lo, hi]"),
        ],
        "clip" => &[("seconds", "duration")],
        "track" => &[
            ("from", "start value"),
            ("to", "end value"),
            ("prop", "translation|rotation|scale"),
        ],
        "spin" => &[
            ("target", "joint or node name"),
            ("axis", "vec3"),
            ("rpm", "revolutions per minute (60)"),
        ],
        "open_close" => &[
            ("target", "joint or node name"),
            ("axis", "vec3"),
            ("angle", "peak angle° (90)"),
            ("seconds", "duration (1.0)"),
        ],
        "wave" | "flap" => &[
            ("target", "joint or node name"),
            ("axis", "vec3"),
            ("amplitude", "degrees"),
            ("hz", "cycles per second"),
        ],
        "idle" => &[
            ("target", "joint or node name"),
            ("amplitude", "meters (0.02)"),
            ("hz", "cycles per second (0.5)"),
        ],
        "attach" => &[
            ("from", "connector tag"),
            ("to", "connector tag"),
        ],
        "bone" => &[
            ("pos", "position relative to parent bone"),
            ("envelope", "weight-radius in world units"),
        ],
        "decal" => &[
            ("size", "[w, h] (default [0.5, 0.5])"),
            ("prompt", "image description for the texture generator"),
            ("image", "path to RGBA PNG (overrides prompt)"),
            ("tint", "[r, g, b] base-color tint"),
            ("roughness", "0..1 (default 0.6)"),
            ("offset", "+Z gap from surface (default 0.001)"),
            ("on", "target node — bends the decal onto its curved surface"),
            ("at", "connector on `on=` target (required when on= is set)"),
            ("up", "x|y|z — local axis aligned with surface normal (default z)"),
            ("lift", "extra outward offset along surface normal (default 0)"),
        ],
        _ => &[],
    };

    let mut out: Vec<Candidate> = kind_specific
        .iter()
        .map(|(k, d)| Candidate {
            label: (*k).to_string(),
            kind: CandidateKind::Attribute,
            insert: format!("{k}="),
            detail: Some(*d),
        })
        .collect();
    // Include the common set for any geometry-like kind. Materials and the
    // animation templates don't take the common transform attrs, so skip
    // those.
    let include_common = !matches!(
        parent_kind.unwrap_or(""),
        "material" | "connector" | "joint" | "clip" | "track" | "attach" | "bone"
    );
    if include_common {
        for (k, d) in COMMON_ATTRS {
            // Skip duplicates already in kind_specific.
            if out.iter().any(|c| c.label == *k) {
                continue;
            }
            out.push(Candidate {
                label: (*k).to_string(),
                kind: CandidateKind::Attribute,
                insert: format!("{k}="),
                detail: Some(*d),
            });
        }
    }
    out
}

fn attr_value_pool(attr: &str, parent_kind: Option<&str>, materials: &[String]) -> Vec<Candidate> {
    // Attribute-driven enum/ident values. String-quoting is preserved in the
    // insert text because the grammar accepts bare idents too.
    let enums: &[&str] = match (parent_kind.unwrap_or(""), attr) {
        (_, "axis") | (_, "around") => &["x", "y", "z"],
        ("material", "alpha_mode") => &["\"opaque\"", "\"blend\"", "\"mask\""],
        ("material", "uv_mode") => &["\"tile\"", "\"fit\""],
        ("solid", "cleanup") => &["\"coplanar\"", "\"none\""],
        ("joint", "type") => &["hinge", "slider", "ball", "rotor"],
        ("feature", "kind") => &[
            "stalagmite", "stalactite", "column", "rock_pile", "pool", "lake",
        ],
        ("stack", "align") => &["center", "start", "end"],
        ("stack", "pack") => &["start", "center", "end"],
        ("track", "prop") => &["\"translation\"", "\"rotation\"", "\"scale\""],
        (_, "anchor") => &[
            "center", "top", "bottom", "left", "right", "front", "back",
            "top_left", "top_right", "bottom_left", "bottom_right",
            "bottom_left_front", "bottom_right_front",
            "bottom_left_back", "bottom_right_back",
        ],
        (_, "double_sided") | (_, "cap_ends") | (_, "center") | (_, "debug_hide_shell")
        | (_, "debug_show_poi") => &["0", "1"],
        _ => &[],
    };

    let mut out: Vec<Candidate> = enums
        .iter()
        .map(|v| Candidate {
            label: (*v).to_string(),
            kind: CandidateKind::EnumValue,
            insert: (*v).to_string(),
            detail: None,
        })
        .collect();

    // `mat="…"` / `target="…"` / `pivot="…"` take a name reference.
    let is_mat_attr = matches!(attr, "mat" | "skin");
    if is_mat_attr {
        for m in materials {
            out.push(Candidate {
                label: m.clone(),
                kind: CandidateKind::Material,
                insert: format!("\"{m}\""),
                detail: None,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(cs: &Completions) -> Vec<&str> {
        cs.candidates.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn node_kind_context_at_top_level() {
        let src = "bo";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert_eq!(cs.range, 0..2);
        assert!(labels(&cs).contains(&"box"));
    }

    #[test]
    fn node_kind_context_inside_block() {
        let src = "scene {\n  cyl";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert!(labels(&cs).iter().any(|l| l.starts_with("cyl")));
    }

    #[test]
    fn attr_key_context_after_open_paren() {
        let src = "box (si";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert!(labels(&cs).contains(&"size"));
        // Common attrs should come along for the ride.
        let all_labels = labels(&cs);
        assert!(
            all_labels.iter().any(|l| *l == "size"),
            "expected size in {all_labels:?}"
        );
    }

    #[test]
    fn attr_key_context_after_comma() {
        let src = "box (size=[1,1,1], po";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert!(labels(&cs).contains(&"pos"));
    }

    #[test]
    fn attr_value_enum_for_axis() {
        let src = "array (around=";
        // Nothing after `=` — no prefix, so no completions expected.
        assert!(compute_completions(src, src.len(), &[]).is_none());
        let src = "array (around=y";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert!(labels(&cs).contains(&"y"));
    }

    #[test]
    fn cave_attr_keys_include_new_features() {
        let src = "cave \"c\" (col";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert!(labels(&cs).contains(&"columns"), "{:?}", labels(&cs));
        let src = "cave \"c\" (chambers=8, lod";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        let ls = labels(&cs);
        assert!(ls.contains(&"lod_scale"), "{ls:?}");
        let src = "cave \"c\" (chambers=8, mush";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert!(labels(&cs).contains(&"mushrooms"), "{:?}", labels(&cs));
    }

    #[test]
    fn feature_kind_enum_values() {
        let src = "feature \"f\" (kind=col";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert!(labels(&cs).contains(&"column"), "{:?}", labels(&cs));
    }

    #[test]
    fn attr_value_alpha_mode_strings() {
        let src = "material \"m\" (alpha_mode=bl";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        // The enum values are quoted; prefix "bl" still matches substring.
        assert!(cs
            .candidates
            .iter()
            .any(|c| c.label.contains("blend")));
    }

    #[test]
    fn mat_attr_suggests_known_materials() {
        let src = "box (mat=w";
        let mats = vec!["wood".to_string(), "metal".to_string()];
        let cs = compute_completions(src, src.len(), &mats).unwrap();
        assert!(labels(&cs).contains(&"wood"));
    }

    #[test]
    fn inside_string_is_skipped() {
        let src = "material \"wo";
        // The prefix `wo` is inside a string — no keyword completion.
        let cs = compute_completions(src, src.len(), &[]);
        assert!(cs.is_none(), "should not complete inside a string");
    }

    #[test]
    fn inside_comment_is_skipped() {
        let src = "// sce";
        assert!(compute_completions(src, src.len(), &[]).is_none());
    }

    #[test]
    fn prefix_ranking_exact_beats_substring() {
        let src = "sc";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        // `scene` is a prefix match; `icosphere` contains "sc" as a
        // substring match through case folding — prefix should win.
        let first = cs.candidates.first().unwrap();
        assert_eq!(first.label, "scene");
    }

    #[test]
    fn material_attrs_include_color() {
        let src = "material \"x\" (co";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert!(labels(&cs).contains(&"color"));
    }

    #[test]
    fn nested_brackets_survive_backward_walk() {
        // vec3 literals inside an attr list must not confuse the scanner.
        let src = "box (size=[1, 2, 3], po";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert!(labels(&cs).contains(&"pos"));
    }

    #[test]
    fn attr_value_continues_across_vec() {
        // Vec literals earlier in the attr list shouldn't confuse context
        // inference further along — `anchor=bo` should still resolve to
        // enum-value suggestions.
        let src = "box (pos=[0, 0, 0], anchor=bo";
        let cs = compute_completions(src, src.len(), &[]).unwrap();
        assert!(cs.candidates.iter().any(|c| c.label == "bottom"));
    }

    #[test]
    fn digit_prefix_is_rejected() {
        // A numeric prefix (e.g. mid-value after `=`) isn't an identifier
        // — no completion should be offered.
        let src = "box (pos=[0, 0, 0], x=1";
        assert!(compute_completions(src, src.len(), &[]).is_none());
    }
}
