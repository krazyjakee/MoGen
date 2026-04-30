//! Spatial summaries of `import "X.mog"` declarations, used to give the LLM
//! enough information to *position* `use` invocations sensibly when editing a
//! composition file. For each top-level import we lower the referenced file
//! standalone and report the union AABB of its scene roots in the same frame
//! a `use "X"` would expand into. The composing scene's prompt then ships
//! these alongside the user instruction.
//!
//! Best-effort by design: a malformed import (missing file, parse error,
//! lowering error, no `scene` block) is silently dropped from the summary —
//! the validator will still complain about it during the build.

use std::fs;
use std::path::{Path, PathBuf};

use mogen_core::{subtree_local_aabb, Aabb};
use mogen_dsl::{lower_with_source, parse, Node, Value};

#[derive(Debug, Clone)]
pub struct ImportSummary {
    /// The name a `use "<name>"` invocation in the composing scene refers to.
    /// Either the import's `(as=<ident>)` alias or the file stem.
    pub name: String,
    /// The path string as written in the `import "..."` declaration.
    pub raw_path: String,
    /// Local-frame AABB of the imported file's scene body. `None` when the
    /// file has no `scene` block, parsing/lowering failed, or the body
    /// contains no geometry.
    pub aabb: Option<Aabb>,
}

/// Parse `source` for top-level `import` declarations and compute each
/// import's local-frame AABB. Returns one entry per import, in source order;
/// imports whose AABB cannot be computed still appear (with `aabb: None`) so
/// the caller can decide whether to mention them.
pub fn summarize_imports(source: &str, base_dir: Option<&Path>) -> Vec<ImportSummary> {
    let Ok(ast) = parse(source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for node in &ast {
        if node.kind != "import" {
            continue;
        }
        let Some(raw) = node.name.as_deref() else {
            continue;
        };
        let resolved = resolve_path(raw, base_dir);
        let alias = import_alias(node);
        let stem = resolved
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| raw.to_string());
        let name = alias.unwrap_or(stem);
        let aabb = compute_import_aabb(&resolved);
        out.push(ImportSummary {
            name,
            raw_path: raw.to_string(),
            aabb,
        });
    }
    out
}

/// Format a "Imports with bounds" preamble suitable for prepending to an LLM
/// user prompt. Returns `None` when no import has a usable AABB.
pub fn format_import_aabb_preamble(summaries: &[ImportSummary]) -> Option<String> {
    let usable: Vec<&ImportSummary> = summaries.iter().filter(|s| s.aabb.is_some()).collect();
    if usable.is_empty() {
        return None;
    }
    let max_name = usable.iter().map(|s| s.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    out.push_str(
        "Imports with bounds (axis-aligned, meters; in the local frame each `use \"<name>\"` \
         expands into — the composing scene's per-`use` `pos`/`rot`/`scale` is applied on top):\n",
    );
    for s in usable {
        let aabb = s.aabb.unwrap();
        let dx = aabb.max.x - aabb.min.x;
        let dy = aabb.max.y - aabb.min.y;
        let dz = aabb.max.z - aabb.min.z;
        out.push_str(&format!(
            "- {name:<width$}  size=[{dx:.2},{dy:.2},{dz:.2}]  \
             min=[{nx:.2},{ny:.2},{nz:.2}]  max=[{xx:.2},{xy:.2},{xz:.2}]\n",
            name = s.name,
            width = max_name,
            dx = dx,
            dy = dy,
            dz = dz,
            nx = aabb.min.x,
            ny = aabb.min.y,
            nz = aabb.min.z,
            xx = aabb.max.x,
            xy = aabb.max.y,
            xz = aabb.max.z,
        ));
    }
    Some(out)
}

fn compute_import_aabb(path: &Path) -> Option<Aabb> {
    let src = fs::read_to_string(path).ok()?;
    let inner_ast = parse(&src).ok()?;
    let dir = path.parent();
    let graph = lower_with_source(&inner_ast, dir).ok()?;
    let mut acc = Aabb::empty();
    for &root in &graph.roots {
        if let Some(local) = subtree_local_aabb(&graph, root) {
            let m = graph.nodes[root.0 as usize].transform.to_mat4();
            acc.merge(local.transformed(m));
        }
    }
    if acc.is_empty() {
        None
    } else {
        Some(acc)
    }
}

fn import_alias(n: &Node) -> Option<String> {
    for (k, v) in &n.attrs {
        if k != "as" {
            continue;
        }
        match v {
            Value::Ident(s) | Value::String(s) => return Some(s.clone()),
            _ => {}
        }
    }
    None
}

fn resolve_path(raw: &str, base: Option<&Path>) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(base) = base {
        base.join(p)
    } else {
        p.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("mogen-imports-{tag}-{stamp}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn no_imports_yields_empty_summary() {
        let src = "scene { box \"b\" (size=[1,1,1]) }\n";
        let s = summarize_imports(src, None);
        assert!(s.is_empty());
    }

    #[test]
    fn missing_file_returns_entry_with_none_aabb() {
        let dir = tmp_dir("missing");
        let scene = "import \"does_not_exist.mog\"\nscene { use \"does_not_exist\" }\n";
        let s = summarize_imports(scene, Some(&dir));
        assert_eq!(s.len(), 1);
        assert!(s[0].aabb.is_none());
        assert_eq!(s[0].name, "does_not_exist");
    }

    #[test]
    fn unit_box_import_reports_unit_aabb() {
        let dir = tmp_dir("unitbox");
        let inner = "scene { box \"b\" (size=[1,1,1]) }\n";
        fs::write(dir.join("widget.mog"), inner).unwrap();
        let scene = "import \"widget.mog\"\nscene { use \"widget\" }\n";
        let s = summarize_imports(scene, Some(&dir));
        assert_eq!(s.len(), 1);
        let entry = &s[0];
        assert_eq!(entry.name, "widget");
        let aabb = entry.aabb.expect("AABB present");
        assert!((aabb.min.x - -0.5).abs() < 1e-4);
        assert!((aabb.max.x - 0.5).abs() < 1e-4);
        assert!((aabb.min.y - -0.5).abs() < 1e-4);
        assert!((aabb.max.y - 0.5).abs() < 1e-4);
    }

    #[test]
    fn alias_overrides_file_stem_in_summary_name() {
        let dir = tmp_dir("alias");
        let inner = "scene { box \"b\" (size=[2,1,1]) }\n";
        fs::write(dir.join("thing.mog"), inner).unwrap();
        let scene = "import \"thing.mog\" (as=widget)\nscene { use \"widget\" }\n";
        let s = summarize_imports(scene, Some(&dir));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "widget");
    }

    #[test]
    fn pos_in_imported_scene_shifts_aabb_in_use_frame() {
        // A `box (pos=[2,0,0])` inside the imported scene should land at
        // x ∈ [1.5, 2.5] in the frame where `use "shifted"` expands. This
        // is the property the LLM relies on when computing collision-free
        // placements.
        let dir = tmp_dir("shifted");
        let inner = "scene { box \"b\" (pos=[2,0,0], size=[1,1,1]) }\n";
        fs::write(dir.join("shifted.mog"), inner).unwrap();
        let scene = "import \"shifted.mog\"\nscene { use \"shifted\" }\n";
        let s = summarize_imports(scene, Some(&dir));
        let aabb = s[0].aabb.expect("AABB present");
        assert!((aabb.min.x - 1.5).abs() < 1e-4);
        assert!((aabb.max.x - 2.5).abs() < 1e-4);
    }

    #[test]
    fn preamble_renders_only_when_some_aabb_is_known() {
        let no_aabb = ImportSummary {
            name: "x".into(),
            raw_path: "x.mog".into(),
            aabb: None,
        };
        assert!(format_import_aabb_preamble(&[no_aabb]).is_none());

        let with_aabb = ImportSummary {
            name: "desk".into(),
            raw_path: "desk.mog".into(),
            aabb: Some(Aabb {
                min: glam::Vec3::new(-0.8, 0.0, -0.4),
                max: glam::Vec3::new(0.8, 0.78, 0.4),
            }),
        };
        let s = format_import_aabb_preamble(&[with_aabb]).expect("rendered");
        assert!(s.contains("desk"));
        assert!(s.contains("size=[1.60,0.78,0.80]"));
        assert!(s.contains("min=[-0.80,0.00,-0.40]"));
        assert!(s.contains("max=[0.80,0.78,0.40]"));
    }
}
