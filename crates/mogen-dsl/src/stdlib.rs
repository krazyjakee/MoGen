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

use std::sync::OnceLock;

use crate::module::{collect_modules, ModuleRegistry};
use crate::parser::parse;

/// Each entry: `(filename for diagnostics, embedded source)`.
const STDLIB_FILES: &[(&str, &str)] = &[
    ("humanoid_head.mog",         include_str!("../stdlib/humanoid_head.mog")),
    ("humanoid_torso.mog",        include_str!("../stdlib/humanoid_torso.mog")),
    ("humanoid_arm.mog",          include_str!("../stdlib/humanoid_arm.mog")),
    ("humanoid_leg.mog",          include_str!("../stdlib/humanoid_leg.mog")),
    ("humanoid_hand_5fingers.mog",include_str!("../stdlib/humanoid_hand_5fingers.mog")),
    ("quadruped_torso.mog",       include_str!("../stdlib/quadruped_torso.mog")),
    ("quadruped_leg.mog",         include_str!("../stdlib/quadruped_leg.mog")),
    ("tail.mog",                  include_str!("../stdlib/tail.mog")),
    ("ear.mog",                   include_str!("../stdlib/ear.mog")),
    ("eye.mog",                   include_str!("../stdlib/eye.mog")),
    ("leaf.mog",                  include_str!("../stdlib/leaf.mog")),
    ("branch.mog",                include_str!("../stdlib/branch.mog")),
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
        // Each stdlib file declares one module; attach its summary doc and
        // fold it into the combined registry.
        let names: Vec<String> = reg.names().cloned().collect();
        for name in names {
            if let Some(mut def) = reg.remove(&name) {
                def.doc = summary.clone();
                combined.insert(def);
            }
        }
    }
    combined
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
        let reg = stdlib_registry();
        for name in reg.names() {
            let def = reg.get(name).unwrap();
            // Build a scene that only uses default args (tests the defaults
            // are sensible). Skip modules with required-no-default params.
            if def.params.iter().any(|p| p.default.is_none()) {
                continue;
            }
            let src = format!("scene {{ use \"{name}\" () }}");
            let ast = parse(&src).unwrap_or_else(|e| panic!("parse: {e}"));
            let scene = lower(&ast)
                .unwrap_or_else(|e| panic!("lower {name} failed: {e}"));
            assert!(
                !scene.nodes.is_empty(),
                "stdlib module `{name}` produced an empty scene graph",
            );
        }
    }
}
