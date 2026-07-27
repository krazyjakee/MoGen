//! `mogen import` — read a foreign scene format and write `.mog` source.
//!
//! Note what this does *not* produce: a GLB. Every other command that takes a
//! scene ends at a binary, but an import that handed back geometry would defeat
//! the point. The output is source the user owns and can edit, and `mogen
//! build` takes it from there.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub(crate) fn import(input: PathBuf, out: Option<PathBuf>, name: Option<String>) -> Result<()> {
    let out = out.unwrap_or_else(|| input.with_extension("mog"));
    let scene_name = name.unwrap_or_else(|| stem_of(&out));

    let json = fs::read_to_string(&input)
        .with_context(|| format!("reading {}", input.display()))?;
    let result = mogen_pascal::import(&json, &scene_name)
        .with_context(|| format!("{} is not a pascalorg/editor scene", input.display()))?;

    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    fs::write(&out, &result.source)
        .with_context(|| format!("writing {}", out.display()))?;

    // The same summary is already in the file's header, so this is a
    // convenience rather than the record — nothing here is the only copy.
    println!("{} → {}", input.display(), out.display());
    for line in result.report.summary() {
        println!("  {line}");
    }
    for line in build_readiness(&result.source) {
        println!("  {line}");
    }
    println!("  next: mogen build {}", out.display());
    Ok(())
}

/// Warn about anything that will stop `mogen build`, while the user still has
/// the import in mind.
///
/// A foreign scene is under no obligation to satisfy our validator. Their
/// editor draws walls wherever they are put, so a plan can be a scattering of
/// disconnected runs — perfectly reasonable there, and E1101 here. Saying so
/// now beats a build failure ten minutes later that reads like an importer bug.
///
/// Deliberately *not* suppressed by tagging the import `floating`: that would
/// silence the same check on scenes where it is telling the truth.
fn build_readiness(source: &str) -> Vec<String> {
    let Ok(ast) = mogen_dsl::parse(source) else {
        return vec!["note: the emitted source did not parse — please report this".into()];
    };
    let Ok(graph) = mogen_dsl::lower(&ast) else {
        return vec!["note: the emitted source did not lower — please report this".into()];
    };

    let diags = mogen_validate::validate_graph(&graph);
    if !mogen_core::has_errors(&diags) {
        return Vec::new();
    }
    vec![
        format!(
            "note: {} scene will not build yet — run `mogen check` for detail",
            if diags.iter().any(|d| d.code == "E1101") {
                "parts of this"
            } else {
                "this"
            }
        ),
        "      their editor allows disconnected parts; add `tags=\"floating\"`".into(),
        "      to any that are meant to stand alone".into(),
    ]
}

/// A scene name derived from the output path, falling back to something valid
/// rather than an empty string.
fn stem_of(out: &Path) -> String {
    out.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "imported".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scene_name_comes_from_the_output_file() {
        assert_eq!(stem_of(Path::new("out/garden-house.mog")), "garden-house");
    }

    #[test]
    fn a_pathological_output_path_still_names_the_scene() {
        // `scene ""` would not lower, so a path with no usable stem needs a
        // fallback. A leading-dot name is not one of those cases — Rust reads
        // `.mog` as a dotfile whose whole name is the stem, which is a
        // perfectly good scene name even if an odd one.
        assert_eq!(stem_of(Path::new("/")), "imported");
        assert_eq!(stem_of(Path::new("..")), "imported");
        assert_eq!(stem_of(Path::new(".mog")), ".mog");
    }
}
