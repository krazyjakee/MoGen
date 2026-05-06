//! System-instruction assembly for Gemini.
//!
//! The system instruction is the static context we want cached: DSL grammar
//! rules, known node kinds/attributes, and a summary of shipped stdlib modules
//! (if any). Keeping this assembly pure lets callers hash it and pin a
//! `cachedContents` resource on the Gemini side.
//!
//! The long reference text (preamble, grammar reference, conventions, kinds
//! table, fewshots, output contract) lives in [`content`] so this file stays
//! focused on assembly, helpers, and the test suite that guards the budgets
//! and substring contracts.

mod content;

use mogen_dsl::ModuleRegistry;
use mogen_validate::{attrs_for_kind, KNOWN_KINDS};

use content::{
    ALLOWLIST_INTRO, CONVENTIONS, FEWSHOT, GRAMMAR_REFERENCE, KINDS_REFERENCE, OUTPUT_CONTRACT,
    PREAMBLE,
};

/// A light summary of modules discovered in stdlib / user module paths.
/// Used purely to populate the system instruction — the real registry is
/// resolved at lowering time.
#[derive(Debug, Default, Clone)]
pub struct StdlibIndex {
    pub modules: Vec<ModuleSummary>,
}

#[derive(Debug, Clone)]
pub struct ModuleSummary {
    pub name: String,
    pub params: Vec<(String, Option<String>)>,
    /// Optional one-line description (e.g. from a leading `//` comment in the
    /// module's source file).
    pub doc: Option<String>,
}

impl StdlibIndex {
    pub fn from_registry(reg: &ModuleRegistry) -> Self {
        let mut modules: Vec<ModuleSummary> = reg
            .names()
            .map(|name| {
                let def = reg.get(name).expect("registry name disappeared");
                ModuleSummary {
                    name: def.name.clone(),
                    params: def
                        .params
                        .iter()
                        .map(|p| {
                            let default = p.default.as_ref().and_then(|e| e.eval_const())
                                .map(|n| format_number(n));
                            (p.name.clone(), default)
                        })
                        .collect(),
                    doc: def.doc.clone(),
                }
            })
            .collect();
        modules.sort_by(|a, b| a.name.cmp(&b.name));
        Self { modules }
    }

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

/// Render the validator's `attrs_for_kind` table into the prompt as a closed
/// allowlist. Sorted alphabetically for deterministic cache-key output.
fn render_allowlist(s: &mut String) {
    let mut kinds: Vec<&str> = KNOWN_KINDS.iter().copied().collect();
    kinds.sort_unstable();
    for kind in kinds {
        let attrs = attrs_for_kind(kind);
        s.push_str(&format!("- `{kind}`: "));
        if attrs.is_empty() {
            s.push_str("(no kind-specific attrs)");
        } else {
            let joined = attrs
                .iter()
                .map(|a| format!("`{a}`"))
                .collect::<Vec<_>>()
                .join(", ");
            s.push_str(&joined);
        }
        s.push('\n');
    }
}

fn format_number(n: f32) -> String {
    if n == n.trunc() && n.abs() < 1e6 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Static reference content for Gemini `cachedContents`: DSL grammar, known
/// node kinds, and the validator-derived attribute allowlist. Independent of
/// `StdlibIndex` and stable across all builds at a fixed grammar+validator
/// version, which is what cache keys want — the bytes change only when the
/// language itself does. Pay once per cache lifetime, reuse across requests.
pub fn cacheable_block() -> String {
    let mut s = String::with_capacity(20 * 1024);
    append_grammar(&mut s);
    append_kinds(&mut s);
    append_allowlist(&mut s);
    s
}

/// Per-request system instruction: rules preamble, conventions, fewshots, the
/// stdlib-module summary, and the output contract. This is what the model
/// re-reads on every call, so it carries the budget-sensitive guard — see
/// [`inline_block_stays_under_request_budget`] in tests.
///
/// Pair with [`cacheable_block`] when a `cachedContents` resource is
/// configured; otherwise call [`system_instruction`] for the full prompt.
pub fn inline_block(index: &StdlibIndex) -> String {
    let mut s = String::with_capacity(28 * 1024);
    s.push_str(PREAMBLE);
    append_conventions(&mut s);
    append_fewshots(&mut s);
    append_modules(&mut s, index);
    s.push_str(OUTPUT_CONTRACT);
    s
}

/// Full system instruction for the no-cache path. Byte-stable given the same
/// index. Section order is unchanged from the pre-split assembly — callers
/// without `cachedContents` configured see exactly the prompt they always did.
pub fn system_instruction(index: &StdlibIndex) -> String {
    let mut s = String::with_capacity(40 * 1024);
    s.push_str(PREAMBLE);
    append_grammar(&mut s);
    append_conventions(&mut s);
    append_kinds(&mut s);
    append_allowlist(&mut s);
    append_fewshots(&mut s);
    append_modules(&mut s, index);
    s.push_str(OUTPUT_CONTRACT);
    s
}

fn append_grammar(s: &mut String) {
    s.push_str("\n\n## DSL grammar (pest-derived, informal)\n\n");
    s.push_str(GRAMMAR_REFERENCE);
}

fn append_conventions(s: &mut String) {
    s.push_str("\n\n## Conventions: units and orientation\n\n");
    s.push_str(CONVENTIONS);
}

fn append_kinds(s: &mut String) {
    s.push_str("\n\n## Known node kinds\n\n");
    s.push_str(KINDS_REFERENCE);
}

fn append_allowlist(s: &mut String) {
    s.push_str("\n\n## Attribute allowlist (authoritative)\n\n");
    s.push_str(ALLOWLIST_INTRO);
    render_allowlist(s);
}

fn append_fewshots(s: &mut String) {
    s.push_str("\n\n## Prompt → DSL demonstrations\n\n");
    s.push_str(FEWSHOT);
}

fn append_modules(s: &mut String, index: &StdlibIndex) {
    s.push_str("\n\n## Stdlib modules\n\n");
    if index.is_empty() {
        s.push_str("_No stdlib modules are currently registered. Define any you need inline with `module \"name\" (...) { ... }` and then instantiate with `use \"name\" (...)`._\n");
    } else {
        s.push_str("When one of these modules fits a part, prefer `use \"name\" (...)` over rebuilding it from primitives — they're pre-validated.\n\n");
        for m in &index.modules {
            s.push_str(&format!("- `{}(", m.name));
            let parts: Vec<String> = m
                .params
                .iter()
                .map(|(n, d)| match d {
                    Some(v) => format!("{n}={v}"),
                    None => n.clone(),
                })
                .collect();
            s.push_str(&parts.join(", "));
            s.push_str(")`");
            if let Some(doc) = &m.doc {
                s.push_str(" — ");
                s.push_str(doc);
            }
            s.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_index_renders_placeholder() {
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("No stdlib modules"));
        assert!(s.contains("## DSL grammar"));
        assert!(s.contains("## Output contract"));
        assert!(s.contains("## Conventions"));
        assert!(s.contains("## Prompt → DSL demonstrations"));
    }

    #[test]
    fn populated_index_lists_modules_with_defaults() {
        let mut idx = StdlibIndex::default();
        idx.modules.push(ModuleSummary {
            name: "leg".into(),
            params: vec![("height".into(), Some("0.5".into())), ("radius".into(), Some("0.05".into()))],
            doc: Some("a cylindrical leg".into()),
        });
        idx.modules.push(ModuleSummary {
            name: "slab".into(),
            params: vec![("width".into(), Some("1".into())), ("required".into(), None)],
            doc: None,
        });
        let s = system_instruction(&idx);
        assert!(s.contains("`leg(height=0.5, radius=0.05)`"));
        assert!(s.contains("a cylindrical leg"));
        assert!(s.contains("`slab(width=1, required)`"));
        // Populated index swaps the placeholder for the "prefer use" nudge.
        assert!(s.contains("prefer `use"));
        assert!(!s.contains("No stdlib modules"));
    }

    #[test]
    fn output_is_byte_stable_for_same_input() {
        // Cache-key property: repeated calls produce identical bytes.
        let idx = StdlibIndex::default();
        let a = system_instruction(&idx);
        let b = system_instruction(&idx);
        assert_eq!(a, b);
    }

    #[test]
    fn from_registry_sorts_module_names() {
        let ast = mogen_dsl::parse(
            r#"
            module "zed" (x=1) { box "b" (size=[1,1,1]) }
            module "alpha" (y=2) { box "b" (size=[1,1,1]) }
            "#,
        )
        .unwrap();
        let reg = mogen_dsl::collect_modules(&ast).unwrap();
        let idx = StdlibIndex::from_registry(&reg);
        assert_eq!(idx.modules.len(), 2);
        assert_eq!(idx.modules[0].name, "alpha");
        assert_eq!(idx.modules[1].name, "zed");
    }

    #[test]
    fn kinds_table_lists_every_primitive() {
        // If a primitive exists in mogen-geom but not here, the model won't use it.
        let s = system_instruction(&StdlibIndex::default());
        for kind in [
            "box", "rounded_box", "plane", "quad", "disc", "cylinder", "cone",
            "capsule", "sphere", "icosphere", "torus", "prism", "pyramid",
            "wedge", "frustum", "tube", "hemisphere", "half_cylinder",
            "torus_arc", "ellipsoid", "superellipsoid", "curved_plane",
            "lathe", "spline_tube", "spline_ribbon",
            // Foliage card + recursive procedural tree.
            "leaf_card", "branch",
            // Box aliases + hole-punched wall.
            "slab", "post", "panel", "wall",
        ] {
            assert!(s.contains(&format!("`{kind}`")), "kinds table missing {kind}");
        }
    }

    #[test]
    fn exposes_solid_stack_grid_and_placement_shortcuts() {
        // These features all existed in the DSL but weren't being pushed by
        // the system instruction — so the LLM never reached for them.
        let s = system_instruction(&StdlibIndex::default());
        // Containers:
        assert!(s.contains("`solid`"), "kinds table missing solid");
        assert!(s.contains("`stack`"), "kinds table missing stack");
        assert!(s.contains("`grid`"), "kinds table missing grid");
        assert!(
            s.contains("cleanup=\"coplanar\""),
            "grammar reference missing solid cleanup option"
        );
        // Placement shortcuts:
        assert!(s.contains("`anchor=bottom"), "grammar missing anchor shortcut");
        assert!(s.contains("above=\"sib\""), "grammar missing relative placement");
        assert!(s.contains("`from=[x,y,z]"), "grammar missing from/to corners");
        // Output-contract self-check picks them up:
        assert!(s.contains("Flush-joined siblings"), "output contract missing sibling-placement rule");
        assert!(s.contains("single solid shape"), "output contract missing solid-grouping rule");
        // An archway fewshot actually demonstrates solid+post+slab+above+cleanup.
        assert!(
            s.contains("Prompt: \"a stone archway\""),
            "stone archway fewshot missing"
        );
        assert!(
            s.contains("solid \"arch\" (mat=\"stone\", cleanup=\"coplanar\")"),
            "archway fewshot should use solid + cleanup"
        );
    }

    #[test]
    fn includes_numbered_rules_and_conventions() {
        let s = system_instruction(&StdlibIndex::default());
        // Load-bearing rules are numbered at the top.
        assert!(s.contains("1. Root the scene"));
        assert!(s.contains("2. Join parts with `attach`"));
        // Units and orientation are stated explicitly.
        assert!(s.contains("Units are meters"));
        assert!(s.contains("canonical orientation"));
    }

    #[test]
    fn output_contract_has_self_check() {
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("Before you emit, silently verify"));
    }

    #[test]
    fn exposes_rigging_and_multi_keyframe_tracks() {
        // The skinning path: the model won't emit rigs unless it sees them
        // in the grammar reference, the kinds table, and a fewshot.
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("`skeleton`"), "kinds table missing skeleton row");
        assert!(s.contains("`bone`"), "kinds table missing bone row");
        assert!(s.contains("skin=\"rig\""), "grammar reference missing skin= example");
        assert!(s.contains("parent-relative"), "conventions missing bone-pos-is-parent-relative rule");
        assert!(s.contains("envelope"), "conventions missing envelope guidance");
        // Multi-keyframe tracks:
        assert!(s.contains("keys=[[t, v]"), "grammar reference missing keys=[[t,v]] form");
        // The humanoid fewshot is the concrete demonstration.
        assert!(
            s.contains("Prompt: \"a person walking\""),
            "humanoid walk fewshot missing"
        );
        assert!(
            s.contains("clip \"walk\" (seconds=1.0)"),
            "humanoid walk clip missing"
        );
    }

    #[test]
    fn inline_block_stays_under_request_budget() {
        // Guard against regressions in the per-request portion of the system
        // instruction (preamble + conventions + fewshots + stdlib summary +
        // output contract). The cacheable block (grammar + kinds + allowlist)
        // is paid once per cache lifetime and has its own looser cap below.
        //
        // History on what's accumulated here, kept so future authors can
        // judge whether to push back on a new section: pre-consolidation
        // (separate EXAMPLES and ANTI_PATTERN sections) the full assembly
        // was ~15 KB with an empty stdlib index. Organic primitives
        // (superellipsoid, curved_plane, lathe, spline_tube) added ~1.4 KB
        // across the kinds table, connectors table, and conventions block.
        // A single-sidedness rule for curved_plane/plane/disc/quad plus a
        // potted-fern fewshot showing the group+mirror pattern added ~0.9 KB;
        // steering the model onto the `double_sided=1` material flag (and
        // away from mirroring bent planes, which produced divergent sheets
        // rather than double-sided leaves) added another ~0.1 KB; teaching
        // the model about CSG material inheritance, cut-through geometry,
        // and coplanar-face avoidance added ~0.4 KB; steering the model
        // away from using `attach` on concentric parts (pane-in-frame,
        // core-in-shell) and from stacking `transmission` with
        // `alpha_mode="blend"` on glass added ~0.75 KB. Exposing skinning
        // (skeleton/bone/skin=, multi-keyframe `keys=` tracks, and a
        // humanoid walk fewshot) added ~4 KB; teaching the model about
        // world-space `uv_mode="tile"` vs image-style `uv_mode="fit"` and
        // `uv_scale` density tuning added ~0.6 KB. Surfacing placement
        // shortcuts, the `solid`/`stack`/`grid` containers, the
        // `slab`/`post`/`panel`/`wall` aliases, plus a stone-archway
        // fewshot and two new output-contract rules added ~3.7 KB. Emitting
        // the validator's `attrs_for_kind` table as an authoritative closed
        // allowlist plus the intro paragraph re-stating which kinds accept
        // common attrs added ~3.5 KB. Three organic fewshots (tiger /
        // humanoid mid-stride / oak tree) and the organic-shapes preamble
        // rule added ~3.6 KB. Adding `branch` + `leaf_card` (kinds table,
        // conventions paragraphs, connectors row, preamble update) was
        // net-neutral: ~1.4 KB of new prose offset by rewriting the oak
        // fewshot to a single procedural `branch (...)` declaration. The
        // mirrored-cart fewshot demonstrating `mirror axis=x` added ~1.5 KB,
        // and switching the two humanoid fewshots to `humanoid_full`
        // (walking + armored knight) added another ~1.5 KB net.
        //
        // After the cache split, grammar/kinds/allowlist (~17 KB) move to
        // `cacheable_block()` and don't count against this budget. Today's
        // inline portion sits around 22 KB; 25_000 leaves ~3 KB headroom
        // for one more fewshot or a tightening pass.
        let s = inline_block(&StdlibIndex::default());
        assert!(
            s.len() < 25_000,
            "inline_block grew to {} bytes — cap is 25_000. Either tighten an \
             existing section, drop a fewshot, or move a stable section into \
             cacheable_block.",
            s.len()
        );
    }

    #[test]
    fn cacheable_block_stays_under_cache_budget() {
        // The cached portion (grammar + kinds + allowlist). Bytes here are
        // paid once per cache lifetime, so the cap is loose — but a guard
        // still catches an accidentally-uncached request-varying section
        // sneaking in. The deformation-modifier paragraph in
        // `ALLOWLIST_INTRO` added ~700 bytes vs the pre-modifier baseline;
        // the detailing-modules + per-node `lod=` recipes added another
        // ~1.0 KB.
        let s = cacheable_block();
        assert!(
            s.len() < 24_500,
            "cacheable_block grew to {} bytes — cap is 24_500. Reference \
             material that grows without bound should be fetched on demand, \
             not pinned in the cache.",
            s.len()
        );
    }

    #[test]
    fn split_blocks_cover_full_system_instruction() {
        // Sanity: every byte in `system_instruction` lives in either
        // `cacheable_block` or `inline_block`. Catches a section that gets
        // added to one path but not the others.
        let idx = StdlibIndex::default();
        let full = system_instruction(&idx);
        let cached = cacheable_block();
        let inline = inline_block(&idx);
        // Both halves contribute non-trivially.
        assert!(cached.len() > 10_000, "cacheable_block unexpectedly small");
        assert!(inline.len() > 15_000, "inline_block unexpectedly small");
        // Their combined byte count matches the full assembly. Section
        // headers are emitted by the same helpers in both paths, so the
        // sum is exact — not approximate.
        assert_eq!(
            cached.len() + inline.len(),
            full.len(),
            "split blocks ({} cached + {} inline) don't sum to full ({}) — \
             a section is duplicated or missing from one path",
            cached.len(),
            inline.len(),
            full.len(),
        );
    }

    #[test]
    fn exposes_closed_attribute_allowlist() {
        // The allowlist section exists, calls itself authoritative, and renders
        // the per-kind rows verbatim from the validator so prompt and validator
        // can't drift apart.
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("## Attribute allowlist"));
        assert!(s.contains("closed list of kind-specific attributes"));
        // A sampling of kinds whose exact attrs the model must respect.
        assert!(s.contains("- `open_close`: `target`, `axis`, `angle`, `seconds`"));
        assert!(s.contains("- `joint`: `type`, `axis`, `limits`, `pivot`"));
        assert!(s.contains("- `attach`: `parent`, `child`, `socket`, `plug`, `offset`, `twist`"));
        // Empty-allowlist kinds still appear so the model sees they take no
        // kind-specific attrs (only common ones).
        assert!(s.contains("- `scene`: (no kind-specific attrs)"));
    }

    #[test]
    fn exposes_recursive_branch_and_leaf_card() {
        // The procedural-tree path: kinds table, conventions paragraph,
        // attribute allowlist, organic-shapes preamble, and the oak-tree
        // fewshot all need to push `branch` so the model reaches for it
        // instead of stacking `use \"branch\"` modules manually.
        let s = system_instruction(&StdlibIndex::default());
        // Kinds table rows.
        assert!(s.contains("`branch`"), "kinds table missing branch row");
        assert!(s.contains("`leaf_card`"), "kinds table missing leaf_card row");
        // Conventions paragraph names the recursive-tree feature.
        assert!(
            s.contains("recursive procedural tree"),
            "conventions missing recursive procedural tree blurb"
        );
        // Allowlist auto-renders; spot-check a few critical attrs.
        assert!(
            s.contains("`branch_angle`"),
            "allowlist missing branch_angle for branch"
        );
        assert!(
            s.contains("`leaf_mat`"),
            "allowlist missing leaf_mat for branch"
        );
        // Default connectors row for leaf_card so attach math knows where
        // a leaf mounts on a branch tip.
        assert!(
            s.contains("`stem`"),
            "connectors table missing leaf_card stem"
        );
        // Oak fewshot uses the procedural branch node now.
        assert!(
            s.contains("branch \"oak\""),
            "oak fewshot should use procedural branch"
        );
        // The leaf material in the fewshot demonstrates alpha-mask + double-sided.
        assert!(
            s.contains("alpha_mode=\"mask\""),
            "oak fewshot missing alpha_mode=mask on leaf material"
        );
    }

    #[test]
    fn organic_fewshots_attach_limbs_via_module_connectors() {
        // The tiger fewshot still attaches legs to torso connectors via
        // quadruped_torso. Humanoid figures now go through `humanoid_full`,
        // so we check the knight fewshot demonstrates attaching armor to
        // the parts the preset already provides.
        let s = system_instruction(&StdlibIndex::default());
        assert!(
            s.contains("attach (parent=\"torso\", child=\"leg_fl\""),
            "tiger fewshot should attach legs to torso connectors"
        );
        assert!(
            s.contains("attach (parent=\"head\",   child=\"helmet\""),
            "knight fewshot should attach helmet to head's crown connector"
        );
        assert!(
            s.contains("attach (parent=\"hand_r\", child=\"sword_hilt\""),
            "knight fewshot should attach a sword to a hand from humanoid_full"
        );
    }

    #[test]
    fn humanoid_fewshots_use_humanoid_full_preset() {
        // Both walking and knight fewshots now lean on `humanoid_full` so the
        // detail floor (hands/feet/face/hair) is met without the LLM having
        // to author every part by hand.
        let s = system_instruction(&StdlibIndex::default());
        assert!(
            s.contains("use \"humanoid_full\" (height=1.7)"),
            "walking fewshot should use humanoid_full"
        );
        // Both fewshots use h=1.7 — known good height for humanoid_full's
        // current proportions; some other heights hit a CSG manifold edge
        // case in the smin-blended limbs. The preamble also references the
        // same call form once, so we expect at least 3 occurrences.
        assert!(
            s.matches("use \"humanoid_full\" (height=1.7)").count() >= 3,
            "expected humanoid_full(height=1.7) to appear in preamble + both humanoid fewshots"
        );
        // The standard material palette must appear so the LLM copies it.
        assert!(s.contains("material \"skin\""));
        assert!(s.contains("material \"cloth\""));
        assert!(s.contains("material \"hair\""));
        assert!(s.contains("material \"eye\""));
        assert!(s.contains("material \"mouth\""));
        assert!(s.contains("material \"boot\""));
    }

    #[test]
    fn organic_preamble_has_detail_floor_rule() {
        // The detail-floor language is what makes the LLM stop emitting
        // hand/foot/face-less placeholder figures from short prompts.
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("Detail floor for any humanoid or creature"));
        assert!(s.contains("humanoid_full"));
    }

    #[test]
    fn mirror_has_a_concrete_fewshot() {
        // `mirror` was described in the grammar reference but never demonstrated,
        // so the model rarely reached for it. The cart fewshot pairs `mirror
        // axis=x` with a wheel pair to show the symmetric-replication pattern.
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("mirror (axis=x)"), "expected a mirror fewshot");
        assert!(
            s.contains("Prompt: \"a small wooden cart\""),
            "mirrored-cart fewshot missing"
        );
    }

    #[test]
    fn output_contract_requires_meta_name_description_tags() {
        // The LLM only fills `meta(name, description, tags)` if the contract
        // asks for it explicitly and points at the auto-stamped attrs as
        // off-limits. If this rule disappears, generated files lose their
        // human-readable identity and MoGHub publish forms come up blank.
        let s = system_instruction(&StdlibIndex::default());
        assert!(
            s.contains("Lead the file with a `meta(...)` block"),
            "output contract missing meta-block lead-in rule"
        );
        assert!(s.contains("`name = "), "meta rule missing name");
        assert!(s.contains("`description = "), "meta rule missing description");
        assert!(s.contains("`tags = "), "meta rule missing tags");
        // The full set of toolchain-stamped attrs must be called out so the
        // LLM doesn't invent a seed (would overwrite ours) or stamp a stale
        // mogen_version. All four belong to the toolchain.
        for attr in ["`seed`", "`thinking`", "`prompt`", "`mogen_version`"] {
            assert!(
                s.contains(attr),
                "meta rule must list {attr} as toolchain-stamped"
            );
        }
        assert!(
            s.contains("toolchain-stamped"),
            "meta rule should split author-written vs toolchain-stamped attrs"
        );
    }

    #[test]
    fn fewshots_demonstrate_meta_block() {
        // Fewshots are the strongest format driver — without a concrete
        // example of the meta block, the LLM tends to skip it even when the
        // contract requires it. Every fewshot now leads with one.
        let s = system_instruction(&StdlibIndex::default());
        let count = s.matches("meta (name = ").count();
        assert!(
            count >= 11,
            "expected every fewshot to lead with a meta(name=...) block, got {count}"
        );
        // Spot-check a couple of representative subjects so a missing one
        // surfaces with a useful diff.
        assert!(s.contains("meta (name = \"wooden_stool\""));
        assert!(s.contains("meta (name = \"young_oak_tree\""));
    }

    #[test]
    fn crate_fewshot_uses_hinge_group_not_mesh_center() {
        // Regression: the earlier fewshot attached `open_close` to the lid
        // mesh directly, which rotates about its centre. The canonical pattern
        // is joint + pivot on a group whose origin sits at the hinge edge.
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("group \"lid_hinge\""));
        assert!(s.contains("joint \"lid_pivot\" (type=hinge, axis=[1, 0, 0], pivot=\"lid_hinge\")"));
        assert!(s.contains("open_close \"lid_swing\" (target=\"lid_pivot\""));
    }
}
