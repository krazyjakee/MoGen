//! `mogen import` — read a foreign scene format and write `.mog` source.
//!
//! The reader is picked from the input's extension: `.glb` / `.gltf` go to
//! `mogen-import` (mesh import; `.obj` / `.fbx` are recognised but not yet
//! read), anything else is treated as a pascalorg/editor JSON scene.
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

    let (source, assets, summary) = if mogen_import::Format::from_path(&input).is_some() {
        let result = mogen_import::import_file(&input, &scene_name, &mogen_import::Options::default())
            .with_context(|| format!("importing {}", input.display()))?;
        (result.source, result.assets, result.report.summary())
    } else {
        let json = fs::read_to_string(&input)
            .with_context(|| format!("reading {}", input.display()))?;
        let result = mogen_pascal::import(&json, &scene_name)
            .with_context(|| format!("{} is not a pascalorg/editor scene", input.display()))?;
        (result.source, Vec::new(), result.report.summary())
    };

    let out_dir = out.parent().filter(|d| !d.as_os_str().is_empty());
    if let Some(dir) = out_dir {
        fs::create_dir_all(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    // Textures first: the `.mog` references them by relative path, so a
    // half-written import should never be a source file pointing at nothing.
    for asset in &assets {
        let path = out_dir.map(|d| d.join(&asset.path)).unwrap_or_else(|| PathBuf::from(&asset.path));
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        fs::write(&path, &asset.bytes)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    fs::write(&out, &source)
        .with_context(|| format!("writing {}", out.display()))?;

    // The same summary is already in the file's header, so this is a
    // convenience rather than the record — nothing here is the only copy.
    println!("{} → {}", input.display(), out.display());
    for line in summary {
        println!("  {line}");
    }
    if !assets.is_empty() {
        println!("  wrote {} texture file(s) next to it", assets.len());
    }
    for line in build_readiness(&source) {
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
        "      the source allows disconnected parts; add `tags=\"floating\"`".into(),
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
