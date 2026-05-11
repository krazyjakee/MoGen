//! Stdlib module loader.
//!
//! Each `crates/mogen-dsl/stdlib/<name>.mog` is embedded at compile time and
//! exposed as a single global `ModuleRegistry`. The leading `// summary:`
//! comment in each file becomes the module's `doc` so the LLM stdlib index
//! can render a one-liner per module.
//!
//! Loaded once per process via `OnceLock`; subsequent calls reuse the cached
//! registry. Adding a new module = drop a `.mog` file in the dir and append
//! one entry to `STDLIB_FILES`.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::ast::Node;
use crate::module::{collect_modules, ModuleRegistry};
use crate::parser::parse;

/// Each entry: `(filename for diagnostics, embedded source)`.
const STDLIB_FILES: &[(&str, &str)] = &[
    ("humanoid_head.mog",         include_str!("../stdlib/humanoid_head.mog")),
    ("humanoid_torso.mog",        include_str!("../stdlib/humanoid_torso.mog")),
    ("humanoid_arm.mog",          include_str!("../stdlib/humanoid_arm.mog")),
    ("humanoid_leg.mog",          include_str!("../stdlib/humanoid_leg.mog")),
    ("humanoid_hand_5fingers.mog",include_str!("../stdlib/humanoid_hand_5fingers.mog")),
    ("humanoid_foot.mog",         include_str!("../stdlib/humanoid_foot.mog")),
    ("humanoid_face.mog",         include_str!("../stdlib/humanoid_face.mog")),
    ("humanoid_full.mog",         include_str!("../stdlib/humanoid_full.mog")),
    ("humanoid_idle.mog",         include_str!("../stdlib/humanoid_idle.mog")),
    ("humanoid_walk.mog",         include_str!("../stdlib/humanoid_walk.mog")),
    ("humanoid_run.mog",          include_str!("../stdlib/humanoid_run.mog")),
    ("humanoid_jump.mog",         include_str!("../stdlib/humanoid_jump.mog")),
    ("quadruped_torso.mog",       include_str!("../stdlib/quadruped_torso.mog")),
    ("quadruped_leg.mog",         include_str!("../stdlib/quadruped_leg.mog")),
    ("tail.mog",                  include_str!("../stdlib/tail.mog")),
    ("ear.mog",                   include_str!("../stdlib/ear.mog")),
    ("eye.mog",                   include_str!("../stdlib/eye.mog")),
    ("leaf.mog",                  include_str!("../stdlib/leaf.mog")),
    ("branch.mog",                include_str!("../stdlib/branch.mog")),
    // Detailing modules — small parametric details that compose into hard-surface,
    // organic, or decorative parts. Each is documented in `docs/modules.md`.
    ("bolt_circle.mog",           include_str!("../stdlib/bolt_circle.mog")),
    ("vent_strip.mog",            include_str!("../stdlib/vent_strip.mog")),
    ("panel_seam.mog",            include_str!("../stdlib/panel_seam.mog")),
    ("rivet_line.mog",            include_str!("../stdlib/rivet_line.mog")),
    ("step_taper.mog",            include_str!("../stdlib/step_taper.mog")),
    ("cable.mog",                 include_str!("../stdlib/cable.mog")),
    ("chain.mog",                 include_str!("../stdlib/chain.mog")),
    ("feather_card.mog",          include_str!("../stdlib/feather_card.mog")),
    ("scale_band.mog",            include_str!("../stdlib/scale_band.mog")),
    ("gear.mog",                  include_str!("../stdlib/gear.mog")),
    // Organic shape wrappers — sensible defaults over Phase D primitives.
    ("spring.mog",                include_str!("../stdlib/spring.mog")),
    ("terrain_patch.mog",         include_str!("../stdlib/terrain_patch.mog")),
    ("blob.mog",                  include_str!("../stdlib/blob.mog")),
    ("water_patch.mog",           include_str!("../stdlib/water_patch.mog")),
    // Building modules — referenced by `building` for door, window, and
    // skylight stamping. Each takes `width=`/`height=` so the building
    // generator can size them to the opening.
    ("door_simple.mog",           include_str!("../stdlib/door_simple.mog")),
    ("window_simple.mog",         include_str!("../stdlib/window_simple.mog")),
    ("skylight_simple.mog",       include_str!("../stdlib/skylight_simple.mog")),
];

static STDLIB: OnceLock<ModuleRegistry> = OnceLock::new();

/// Return the (cached) stdlib module registry.
///
/// Panics if a stdlib `.mog` fails to parse — those files are static and
/// covered by `tests::all_stdlib_modules_parse`, so a panic here is a build
/// regression, not a user-facing error.
pub fn stdlib_registry() -> &'static ModuleRegistry {
    STDLIB.get_or_init(build_stdlib_registry)
}

fn build_stdlib_registry() -> ModuleRegistry {
    let mut combined = ModuleRegistry::default();
    for (filename, src) in STDLIB_FILES {
        let summary = parse_summary(src);
        let ast = parse(src).unwrap_or_else(|e| {
            panic!("stdlib parse failed in {filename}: {e}");
        });
        let mut reg = collect_modules(&ast).unwrap_or_else(|e| {
            panic!("stdlib collect_modules failed in {filename}: {e}");
        });
        // Synthetic origin so expanded stdlib nodes are tagged the same way
        // imported nodes are. Studio uses `origin.is_none()` as "this node
        // was authored in the active source"; without this tag, stdlib
        // bodies (which are NOT in the active source — their `source_span`
        // bytes are local to the embedded stdlib `.mog` string, not to
        // whatever file the user is editing) would get treated as
        // editable, and span-based set_attr writeback would splice into
        // the wrong offsets in the active source.
        let stdlib_origin = stdlib_origin_path(filename);
        // Each stdlib file declares one module; attach its summary doc and
        // fold it into the combined registry.
        let names: Vec<String> = reg.names().cloned().collect();
        for name in names {
            if let Some(mut def) = reg.remove(&name) {
                def.doc = summary.clone();
                for body_node in &mut def.body {
                    set_origin_recursive(body_node, &stdlib_origin);
                }
                combined.insert(def);
            }
        }
    }
    combined
}

/// Synthetic path identifying a stdlib `.mog` file. Not a real filesystem
/// path — the `<stdlib>/…` prefix exists so anything inspecting `origin`
/// (Studio's per-file sidebar, error rendering) can recognise stdlib
/// nodes without confusing them for an imported relative path the user
/// could open.
fn stdlib_origin_path(filename: &str) -> PathBuf {
    PathBuf::from(format!("<stdlib>/{filename}"))
}

/// Same shape as `module::imports::helpers::set_origin_recursive`,
/// duplicated here to avoid making that helper crate-public for a single
/// caller.
fn set_origin_recursive(node: &mut Node, origin: &std::path::Path) {
    if node.origin.is_none() {
        node.origin = Some(origin.to_path_buf());
    }
    for c in &mut node.children {
        set_origin_recursive(c, origin);
    }
}

/// Extract the first `// summary: …` comment line from a stdlib source.
fn parse_summary(src: &str) -> Option<String> {
    for line in src.lines().take(8) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("// summary:") {
            let s = rest.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::lower;

    #[test]
    fn all_stdlib_modules_parse_and_load() {
        let reg = stdlib_registry();
        for (filename, _src) in STDLIB_FILES {
            let name = filename.trim_end_matches(".mog");
            assert!(
                reg.contains(name),
                "stdlib file {filename} did not register module `{name}`",
            );
            let def = reg.get(name).unwrap();
            assert!(
                def.doc.is_some() && !def.doc.as_ref().unwrap().is_empty(),
                "stdlib module {name} missing `// summary:` doc",
            );
        }
    }

    #[test]
    fn parse_summary_extracts_first_line() {
        let src = "// summary: hello world\nmodule \"x\" () { }\n";
        assert_eq!(parse_summary(src).as_deref(), Some("hello world"));
    }

    #[test]
    fn parse_summary_returns_none_when_absent() {
        let src = "// not a summary\nmodule \"x\" () { }\n";
        assert!(parse_summary(src).is_none());
    }

    #[test]
    fn each_stdlib_module_lowers_in_isolation() {
        // Round-trip: instantiate every stdlib module from a minimal scene
        // and ensure it builds a valid scene graph end-to-end.
        //
        // Some humanoid modules reference materials by well-known names
        // (`skin`, `cloth`, `hair`, `eye`, `mouth`, `boot`). Declare a
        // standard palette up front so they can lower without the caller
        // having to redeclare materials per-test.
        let preamble = "\
            material \"skin\"  (color=[0.85, 0.65, 0.55])\n\
            material \"cloth\" (color=[0.30, 0.40, 0.60])\n\
            material \"hair\"  (color=[0.20, 0.15, 0.10])\n\
            material \"eye\"   (color=[0.08, 0.08, 0.10])\n\
            material \"mouth\" (color=[0.50, 0.20, 0.20])\n\
            material \"boot\"  (color=[0.15, 0.10, 0.05])\n";
        let reg = stdlib_registry();
        for name in reg.names() {
            let def = reg.get(name).unwrap();
            // Build a scene that only uses default args (tests the defaults
            // are sensible). Skip modules with required-no-default params.
            if def.params.iter().any(|p| p.default.is_none()) {
                continue;
            }
            // Animation-clip modules document a dependency on humanoid_full's
            // `rig` skeleton + named bones. Lowering them in isolation would
            // fail with "track target … is not a joint nor a scene node". For
            // those, prepend a humanoid_full instance so the test exercises
            // the realistic call site.
            let depends_on_humanoid = name.starts_with("humanoid_")
                && matches!(name.as_str(),
                    "humanoid_walk" | "humanoid_run"
                    | "humanoid_idle" | "humanoid_jump");
            let scaffold = if depends_on_humanoid && name != "humanoid_full" {
                "use \"humanoid_full\" ()\n"
            } else {
                ""
            };
            let src = format!("{preamble}scene {{ {scaffold}use \"{name}\" () }}");
            let ast = parse(&src).unwrap_or_else(|e| panic!("parse {name}: {e}"));
            let scene = lower(&ast).unwrap_or_else(|e| panic!("lower {name} failed: {e}"));
            assert!(
                !scene.nodes.is_empty(),
                "stdlib module `{name}` produced an empty scene graph",
            );
            assert!(
                scene.nodes.iter().any(|n| n.mesh.is_some()),
                "stdlib module `{name}` lowered without producing any mesh",
            );
        }
    }
}
