//! Compose the per-object `.mog` modules into a single assembly file. The
//! assembly is a plain text file with one `import` per generated object and
//! one `group … { use … }` per manifest entry, so the existing build
//! pipeline handles it without any wizard awareness.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use super::state::{ObjectEntry, WizardState};

/// Write the assembly `.mog` to `<project>/wizard/scene.mog`. Returns the
/// path the file was written to. Idempotent — the caller may call this
/// repeatedly across correction-loop iterations to refresh the placements.
pub fn write_assembly(state: &WizardState) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&state.project_dir)
        .map_err(|e| format!("mkdir {}: {e}", state.project_dir.display()))?;

    let mut src = String::new();
    writeln!(
        &mut src,
        "// Scene Wizard assembly — generated from \"{}\"",
        escape_dsl_string(&state.prompt)
    )
    .ok();
    writeln!(&mut src).ok();
    writeln!(
        &mut src,
        "meta (name = \"wizard_scene\", description = \"{}\")",
        escape_dsl_string(state.brief.as_deref().unwrap_or(&state.prompt))
    )
    .ok();
    writeln!(&mut src).ok();

    // One `import` per object module that has been generated. The relative
    // path is `objects/<id>.mog`, which resolves against the assembly file's
    // own directory (`<project>/wizard/`).
    let mut imported: Vec<&ObjectEntry> = state
        .manifest
        .iter()
        .filter(|o| {
            o.mog_path
                .as_ref()
                .map(|p| p.exists())
                .unwrap_or(false)
        })
        .collect();
    imported.sort_by(|a, b| a.id.cmp(&b.id));
    for obj in &imported {
        writeln!(&mut src, "import \"objects/{}.mog\"", obj.id).ok();
    }
    if !imported.is_empty() {
        writeln!(&mut src).ok();
    }

    writeln!(&mut src, "scene {{").ok();
    if imported.is_empty() {
        writeln!(
            &mut src,
            "  // No object modules generated yet — wizard wrote a placeholder."
        )
        .ok();
        writeln!(
            &mut src,
            "  group \"placeholder\" {{ box \"floor\" (size=[4,0.05,4]) }}"
        )
        .ok();
    } else {
        for (i, obj) in imported.iter().enumerate() {
            let safe = sanitize_group_name(&obj.id);
            // `pos` carries the manifest placement; `rot` rotates around Y.
            // Tag every instance `floating` so the connectivity validator
            // doesn't flag the gap between independent props — the wizard
            // composes free-standing objects, not a connected mechanism.
            writeln!(
                &mut src,
                "  group \"{name}_{i}\" (pos=[{x:.3}, {y:.3}, {z:.3}], rot=[0, {ry:.2}, 0], tags=\"floating\") {{ use \"{stem}\" () }}",
                name = safe,
                i = i,
                x = obj.position[0],
                y = obj.position[1],
                z = obj.position[2],
                ry = obj.rotation_y_deg,
                stem = obj.id,
            )
            .ok();
        }
    }
    writeln!(&mut src, "}}").ok();

    let path = state.project_dir.join("scene.mog");
    std::fs::write(&path, src.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Escape a free-form string for embedding inside a DSL double-quoted literal.
/// Strips control chars and escapes `\` / `"`.
fn escape_dsl_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 => continue,
            c => out.push(c),
        }
    }
    out
}

/// Strip any characters that aren't valid in a DSL identifier-ish string,
/// keeping the name a safe key for `group "..."`. Underscores and ASCII
/// alphanumerics survive; everything else becomes `_`.
pub(crate) fn sanitize_group_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('x');
    }
    out
}

/// Strip the assembly file path back to its workspace root so other tooling
/// (the LLM correction loop) can refer to objects by relative path.
#[allow(dead_code)]
pub(crate) fn relative_objects_dir(assembly_path: &Path) -> PathBuf {
    assembly_path
        .parent()
        .map(|p| p.join("objects"))
        .unwrap_or_else(|| PathBuf::from("objects"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fresh_tmp(slot: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("mogen-studio-wizard-tests")
            .join(format!(
                "{}-{}-{}",
                slot,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn assembly_with_no_objects_writes_placeholder() {
        let tmp = fresh_tmp("placeholder");
        let state = WizardState {
            project_dir: tmp.join("wizard"),
            prompt: "a cosy reading nook".into(),
            ..Default::default()
        };
        let path = write_assembly(&state).unwrap();
        let src = std::fs::read_to_string(&path).unwrap();
        assert!(src.contains("placeholder"));
        assert!(src.contains("a cosy reading nook"));
    }

    #[test]
    fn assembly_emits_one_import_per_generated_object() {
        let tmp = fresh_tmp("import");
        let project_dir = tmp.join("wizard");
        std::fs::create_dir_all(project_dir.join("objects")).unwrap();
        let chair_path = project_dir.join("objects/chair.mog");
        std::fs::write(&chair_path, "module \"chair\" () {}\n").unwrap();
        let state = WizardState {
            project_dir,
            prompt: "x".into(),
            manifest: vec![ObjectEntry {
                id: "chair".into(),
                name: "Chair".into(),
                role: "hero".into(),
                prompt: "a chair".into(),
                size: [1.0, 1.0, 1.0],
                position: [1.5, 0.0, -2.0],
                rotation_y_deg: 30.0,
                reference_image: None,
                mog_path: Some(chair_path),
                thumb_path: None,
                position_guide: None,
            }],
            ..Default::default()
        };
        let path = write_assembly(&state).unwrap();
        let src = std::fs::read_to_string(&path).unwrap();
        assert!(src.contains("import \"objects/chair.mog\""));
        assert!(src.contains("use \"chair\" ()"));
        assert!(src.contains("pos=[1.500, 0.000, -2.000]"));
        assert!(src.contains("rot=[0, 30.00, 0]"));
    }

    #[test]
    fn dsl_string_escape_handles_quotes_and_newlines() {
        let s = escape_dsl_string("a \"cosy\"\nreading nook\\");
        assert!(!s.contains('\n'));
        assert!(s.contains("\\\""));
        assert!(s.ends_with("\\\\"));
    }

    #[test]
    fn sanitize_group_name_replaces_invalid_chars() {
        assert_eq!(sanitize_group_name("hello world"), "hello_world");
        assert_eq!(sanitize_group_name("foo-bar.1"), "foo_bar_1");
        assert_eq!(sanitize_group_name(""), "x");
    }
}
