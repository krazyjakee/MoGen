//! Apply LLM-suggested position / rotation corrections to the assembly
//! source. Goes through the span-aware `edit::set_attr` helper so unrelated
//! formatting is preserved and diagnostics keep pointing at the right lines.

use mogen_core::Span;
use mogen_dsl::ast::Node;

use crate::edit;

use super::state::{ObjectEntry, PositionCorrection};

/// Apply each correction in `corrections` to the assembly `src`. Unknown
/// object ids are silently skipped (the LLM may reference a since-removed
/// entry across reruns). Returns the modified source plus the count of
/// corrections actually applied — the UI uses both to update the wizard
/// state and surface a status line.
///
/// The assembly file emits each entry as
/// `group "<sanitized_id>_<index>" (pos=…, rot=…) { use "<id>" () }`,
/// so we match by group name prefix and by the inner `use "<id>"` so two
/// objects sharing a sanitised prefix are still resolved unambiguously.
pub fn apply_corrections(
    src: &str,
    manifest: &[ObjectEntry],
    corrections: &[PositionCorrection],
) -> (String, usize) {
    let mut current = src.to_string();
    let mut applied = 0usize;

    for c in corrections {
        let Some(_) = manifest.iter().find(|o| o.id == c.object_id) else {
            continue;
        };
        // Re-parse on every iteration — each edit shifts spans, so we can't
        // reuse the AST. The assembly is small (one group per object) so
        // this stays cheap.
        let Ok(nodes) = mogen_dsl::parse(&current) else {
            break;
        };
        let Some(group_span) = find_group_span_for_object(&nodes, &c.object_id) else {
            continue;
        };
        if let Some(pos) = c.new_position {
            let value = format!("[{:.3}, {:.3}, {:.3}]", pos[0], pos[1], pos[2]);
            current = edit::set_attr(&current, group_span, "pos", &value);
        }
        if let Some(ry) = c.new_rotation_y_deg {
            // Re-parse to refresh the span after the previous edit, since
            // inserting `pos=` may have shifted the closing `)` of the
            // header. Cheap on a small assembly.
            let Ok(nodes2) = mogen_dsl::parse(&current) else {
                break;
            };
            let Some(group_span) = find_group_span_for_object(&nodes2, &c.object_id) else {
                continue;
            };
            let value = format!("[0, {:.2}, 0]", ry);
            current = edit::set_attr(&current, group_span, "rot", &value);
        }
        applied += 1;
    }

    (current, applied)
}

/// Walk the parsed AST looking for `scene { group "..._N" { use "<id>" () } }`
/// and return the span of the group node so we can rewrite its `pos` / `rot`
/// attributes via `edit::set_attr`.
fn find_group_span_for_object(nodes: &[Node], object_id: &str) -> Option<Span> {
    for node in nodes {
        if node.kind == "scene" {
            for child in &node.children {
                if child.kind == "group" && group_targets_object(child, object_id) {
                    return Some(child.span);
                }
            }
        }
    }
    None
}

/// True when `group` is the wizard's placement wrapper for `object_id` —
/// i.e. its body contains a `use "<object_id>" (...)`. Matching by the inner
/// `use` (not the group name) means a user-renamed group still resolves and
/// a coincidentally-named group without the matching `use` is not rewritten.
fn group_targets_object(group: &Node, object_id: &str) -> bool {
    group.children.iter().any(|child| {
        child.kind == "use" && child.name.as_deref() == Some(object_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(id: &str) -> ObjectEntry {
        ObjectEntry {
            id: id.into(),
            name: id.into(),
            role: "hero".into(),
            prompt: "p".into(),
            size: [1.0, 1.0, 1.0],
            position: [0.0, 0.0, 0.0],
            rotation_y_deg: 0.0,
            reference_image: None,
            mog_path: None,
            thumb_path: None,
            position_guide: None,
        }
    }

    #[test]
    fn applies_position_to_named_group() {
        let src = "scene {\n  group \"chair_0\" (pos=[0, 0, 0], tags=\"floating\") { use \"chair\" () }\n}\n";
        let manifest = vec![make_entry("chair")];
        let corrections = vec![PositionCorrection {
            object_id: "chair".into(),
            new_position: Some([1.5, 0.0, -2.0]),
            new_rotation_y_deg: None,
            rationale: "moved".into(),
        }];
        let (out, n) = apply_corrections(src, &manifest, &corrections);
        assert_eq!(n, 1);
        assert!(
            out.contains("pos=[1.500, 0.000, -2.000]"),
            "expected new pos in: {out}"
        );
    }

    #[test]
    fn applies_rotation_only() {
        let src = "scene {\n  group \"chair_0\" (pos=[0, 0, 0], rot=[0, 0, 0]) { use \"chair\" () }\n}\n";
        let manifest = vec![make_entry("chair")];
        let corrections = vec![PositionCorrection {
            object_id: "chair".into(),
            new_position: None,
            new_rotation_y_deg: Some(90.0),
            rationale: "rotated".into(),
        }];
        let (out, n) = apply_corrections(src, &manifest, &corrections);
        assert_eq!(n, 1);
        assert!(out.contains("rot=[0, 90.00, 0]"), "expected new rot in: {out}");
    }

    #[test]
    fn unknown_object_id_is_skipped() {
        let src = "scene {\n  group \"chair_0\" (pos=[0, 0, 0]) { use \"chair\" () }\n}\n";
        let manifest = vec![make_entry("chair")];
        let corrections = vec![PositionCorrection {
            object_id: "ghost".into(),
            new_position: Some([5.0, 0.0, 5.0]),
            new_rotation_y_deg: None,
            rationale: "not in manifest".into(),
        }];
        let (out, n) = apply_corrections(src, &manifest, &corrections);
        assert_eq!(n, 0);
        assert!(out.contains("pos=[0, 0, 0]"));
    }

    #[test]
    fn group_with_matching_prefix_but_different_use_is_not_rewritten() {
        // "chair_extra" group has a name that starts with "chair_" but its inner
        // `use` targets "chair_extra", not "chair". The correction for "chair"
        // must leave it untouched.
        let src = concat!(
            "scene {\n",
            "  group \"chair_extra_0\" (pos=[0, 0, 0]) { use \"chair_extra\" () }\n",
            "  group \"chair_0\" (pos=[0, 0, 0]) { use \"chair\" () }\n",
            "}\n"
        );
        let manifest = vec![make_entry("chair"), make_entry("chair_extra")];
        let corrections = vec![PositionCorrection {
            object_id: "chair".into(),
            new_position: Some([3.0, 0.0, 0.0]),
            new_rotation_y_deg: None,
            rationale: "moved".into(),
        }];
        let (out, n) = apply_corrections(src, &manifest, &corrections);
        assert_eq!(n, 1);
        assert!(
            out.contains("use \"chair_extra\" ()"),
            "chair_extra group should be untouched: {out}"
        );
        assert!(
            out.contains("pos=[3.000, 0.000, 0.000]"),
            "chair group pos should be updated: {out}"
        );
    }
}
