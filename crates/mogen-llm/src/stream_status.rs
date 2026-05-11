//! Turn a partially-streamed DSL response into a short status string.
//!
//! The streaming clients call [`StreamStatus::observe`] with the cumulative
//! response text on every SSE frame. We deliberately do **not** parse the
//! source — mid-edit it would parse-fail most frames — so this module uses
//! cheap substring scans for the keyword that opens each top-level
//! declaration kind and reports the latest stage the model has reached.
//!
//! The progression mirrors how a Coder model is instructed to write a
//! `mogen` DSL file: `meta(` first, then `material(`, then optional
//! `module` declarations, then `scene { … }` for the geometry, then any
//! rigging / animation declarations at the bottom. Stages are walked in
//! order so the **last hit wins** — once `clip(` appears we settle on
//! "authoring animation" even though `meta(` and `scene` are still in
//! the buffer.
//!
//! [`observe`] returns `Some(headline)` only when the label or the
//! rough geometry-node count has changed since the previous call,
//! keeping the studio's status channel quiet between transitions.
//!
//! ## Known limitations
//!
//! Substring matching is content-blind: an anchor like `clip(` will
//! also fire if the model emits the literal text `clip(` inside a
//! string attribute (most plausibly `meta(prompt="…clip(spin)…")`).
//! Because anchors are walked in order and **last-anchor-wins**, a
//! false positive on an early stage gets overridden as soon as a real
//! later anchor lands, so the worst observable outcome is a brief
//! flicker in the status line — not a stuck or wrong final state.

/// Per-call state for one streaming run. Tracks the last status the
/// caller emitted so [`observe`] can suppress unchanged updates.
#[derive(Default, Debug, Clone)]
pub struct StreamStatus {
    last_label: Option<&'static str>,
    last_nodes: usize,
}

/// Stage anchors in the order a `mogen` Coder typically emits them. The
/// last entry whose substring appears in the cumulative buffer wins, so
/// the user sees the progression `thinking → header → materials →
/// geometry → animation` instead of getting stuck on the first stage
/// that ever appeared.
///
/// Substrings are intentionally tight enough that they don't trip
/// inside an identifier or attribute value (`material(` vs the literal
/// word "material" in a comment, `scene ` with the trailing space vs
/// `scene_thing`). Each anchor is the byte-exact form the grammar
/// emits at the start of a declaration.
const STAGES: &[(&str, &str)] = &[
    // Modify-with-edits responses (SEARCH/REPLACE blocks) come first
    // so they win on Modify calls — the model emits these top-down
    // and rarely also contains a top-level `meta(` or `scene {` in
    // the search/replace bodies. Anchored on the marker text the
    // repair loop's `parse_edit_blocks` expects byte-for-byte.
    ("<<<<<<< SEARCH", "writing edits"),
    (">>>>>>> REPLACE", "applying edits"),
    ("meta(", "writing header"),
    ("meta (", "writing header"),
    ("material(", "choosing materials"),
    ("material \"", "choosing materials"),
    ("module ", "defining modules"),
    ("scene {", "writing geometry"),
    ("scene{", "writing geometry"),
    // Inside-scene structure tells us the model is mid-geometry rather
    // than just opening the block.
    ("connector(", "placing connectors"),
    ("connector \"", "placing connectors"),
    ("array(", "arraying parts"),
    ("mirror(", "mirroring parts"),
    ("difference(", "carving with CSG"),
    ("union(", "combining shapes"),
    ("intersect(", "combining shapes"),
    ("attach(", "linking parts"),
    ("attach (", "linking parts"),
    ("skeleton ", "rigging skeleton"),
    ("bone ", "rigging skeleton"),
    ("skin=", "binding skin"),
    // Animation declarations are top-level after `scene { … }`, so any
    // of these landing in the buffer means we're past geometry.
    ("joint ", "authoring animation"),
    ("clip ", "authoring animation"),
    ("clip(", "authoring animation"),
    ("spin ", "authoring animation"),
    ("open_close ", "authoring animation"),
    ("wave ", "authoring animation"),
    ("flap ", "authoring animation"),
    ("idle ", "authoring animation"),
];

impl StreamStatus {
    /// Re-evaluate the latest streamed text. Returns `Some(headline)`
    /// when the displayable status has changed since the previous call,
    /// `None` otherwise. Pre-anchor frames (the model is still thinking
    /// or has only emitted whitespace / a markdown fence) report
    /// `"thinking…"`.
    pub fn observe(&mut self, cumulative: &str) -> Option<String> {
        let nodes = count_geometry_nodes(cumulative);

        let label = pick_label(cumulative).unwrap_or("thinking…");

        let label_changed = self.last_label != Some(label);
        let nodes_changed = nodes != self.last_nodes;
        if !label_changed && !nodes_changed {
            return None;
        }
        self.last_label = Some(label);
        self.last_nodes = nodes;

        Some(if nodes > 0 {
            format!("{label} · {nodes} nodes")
        } else {
            label.to_string()
        })
    }
}

fn pick_label(cumulative: &str) -> Option<&'static str> {
    // Walk the table in order; the last anchor present wins so we land
    // on the most recently entered stage rather than the first stage
    // the model ever emitted.
    let mut chosen: Option<&'static str> = None;
    for (anchor, label) in STAGES {
        if cumulative.contains(anchor) {
            chosen = Some(label);
        }
    }
    chosen
}

/// Rough proxy for "how many primitives has the model declared so far".
/// Every primitive (`box`, `cylinder`, `sphere`, …) emits exactly one
/// `(size=`, `(radius=`, or `(pos=` attribute opener. Counting those
/// substrings is much cheaper than tokenizing and good enough for a
/// status counter — over-counting by one or two doesn't matter.
fn count_geometry_nodes(s: &str) -> usize {
    let mut n = 0usize;
    for needle in &["(size=", "(radius=", "(height="] {
        n += s.matches(needle).count();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_thinking_before_any_anchor() {
        let mut s = StreamStatus::default();
        assert_eq!(s.observe(""), Some("thinking…".to_string()));
        // No change → no further emit.
        assert_eq!(s.observe("```"), None);
    }

    #[test]
    fn picks_meta_then_material_then_scene() {
        let mut s = StreamStatus::default();
        assert_eq!(s.observe(""), Some("thinking…".to_string()));
        assert_eq!(
            s.observe("meta(name=\"x\")"),
            Some("writing header".to_string()),
        );
        assert_eq!(
            s.observe("meta(name=\"x\")\nmaterial(\"wood\""),
            Some("choosing materials".to_string()),
        );
        let geom = "meta(name=\"x\")\nmaterial(\"wood\")\nscene {\n  box \"a\" (size=[1,1,1])";
        let out = s.observe(geom).expect("status should advance");
        assert!(out.starts_with("writing geometry"), "got {out:?}");
        assert!(out.contains("1 nodes"), "node count missing: {out:?}");
    }

    #[test]
    fn animation_anchor_overrides_earlier_stages() {
        let mut s = StreamStatus::default();
        let full = "meta(x)\nmaterial(\"w\")\nscene { box \"b\" (size=[1,1,1]) }\nclip \"c\" (seconds=1.0)";
        let out = s.observe(full).unwrap();
        assert!(out.starts_with("authoring animation"), "got {out:?}");
    }

    #[test]
    fn suppresses_unchanged_status() {
        let mut s = StreamStatus::default();
        let buf = "meta(name=\"x\")";
        assert_eq!(s.observe(buf), Some("writing header".to_string()));
        // Same anchor, same node count → no re-emit.
        assert_eq!(s.observe(buf), None);
        assert_eq!(s.observe("meta(name=\"x\", version=\"1\""), None);
    }

    #[test]
    fn node_count_drives_emit_even_when_label_stable() {
        let mut s = StreamStatus::default();
        let one = "scene { box \"a\" (size=[1,1,1])";
        let first = s.observe(one).unwrap();
        assert!(first.contains("1 nodes"));
        let two = "scene { box \"a\" (size=[1,1,1]) box \"b\" (size=[2,2,2])";
        let second = s.observe(two).unwrap();
        assert!(second.contains("2 nodes"), "got {second:?}");
    }

    #[test]
    fn skeleton_and_skin_distinguished() {
        let mut s = StreamStatus::default();
        let rig = "meta(x)\nscene {\n  skeleton \"arm\" { bone \"root\" (pos=[0,0,0]) }\n";
        let out = s.observe(rig).unwrap();
        assert!(out.starts_with("rigging skeleton"), "got {out:?}");

        let mut s2 = StreamStatus::default();
        let skin = "meta(x)\nscene {\n  skeleton \"arm\" {}\n  cylinder \"c\" (radius=0.1, skin=\"arm\"";
        let out = s2.observe(skin).unwrap();
        assert!(out.starts_with("binding skin"), "got {out:?}");
    }

    #[test]
    fn search_replace_block_surfaces_writing_edits_label() {
        // Modify-with-edit responses start with `<<<<<<< SEARCH` and
        // rarely embed top-level DSL kind keywords in their bodies, so
        // without an edit-block anchor `pick_label` falls through to
        // `None` and the status stays at "thinking…" for the whole
        // call — that was confusing users into thinking modify had
        // hung.
        let mut s = StreamStatus::default();
        let mid = "<<<<<<< SEARCH\nfoo\n=======\nbar\n";
        let out = s.observe(mid).unwrap();
        assert!(out.starts_with("writing edits"), "got {out:?}");
        let complete = "<<<<<<< SEARCH\nfoo\n=======\nbar\n>>>>>>> REPLACE\n";
        let out = s.observe(complete).unwrap();
        assert!(out.starts_with("applying edits"), "got {out:?}");
    }

    #[test]
    fn csg_overrides_plain_geometry() {
        let mut s = StreamStatus::default();
        let csg = "scene { difference(\n  box \"shell\" (size=[1,1,1])";
        let out = s.observe(csg).unwrap();
        assert!(out.starts_with("carving with CSG"), "got {out:?}");
    }
}
