use std::fs;
use std::path::{Path, PathBuf};

use super::paths::resolve_for_check;
use super::spans::find_material_source_span;

/// `(material_name, slot_name, authored_path)` triples for every populated
/// texture slot on every material in the scene. Ordered deterministically
/// (material order × slot order) so the UI doesn't jitter between rebuilds.
pub(in crate::app) fn gather_texture_refs(
    scene: &mogen_core::SceneGraph,
) -> Vec<(String, &'static str, PathBuf)> {
    const SLOTS: [&str; 5] = [
        "base_color",
        "metallic_roughness",
        "normal",
        "occlusion",
        "emissive",
    ];
    let mut out = Vec::new();
    for m in &scene.materials {
        let refs = [
            &m.base_color_texture,
            &m.metallic_roughness_texture,
            &m.normal_texture,
            &m.occlusion_texture,
            &m.emissive_texture,
        ];
        for (slot, r) in SLOTS.iter().zip(refs.iter()) {
            if let Some(t) = r {
                out.push((m.name.clone(), *slot, t.path.clone()));
            }
        }
    }
    out
}

/// PBR map suffixes the textures pipeline produces alongside each albedo.
/// Kept in sync with `mogen_llm::textures::SlotKind::file_suffix` — listed
/// in the order of a full regenerate so deletions run companion-first.
pub(in crate::app) const TEXTURE_COMPANION_SUFFIXES: [&str; 4] = [
    "_albedo.png",
    "_normal.png",
    "_metallicRoughness.png",
    "_ao.png",
];

/// List PNG files in `dir` whose absolute path isn't in `referenced`. Only
/// top-level `*.png` entries are considered — subdirectories aren't walked
/// because the textures pipeline never writes into them. Returns a sorted
/// list so the UI order is stable across repaints.
pub(in crate::app) fn scan_unused_textures(
    dir: &Path,
    referenced: &std::collections::HashSet<PathBuf>,
) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("png"))
                    .unwrap_or(false)
        })
        .filter(|p| !referenced.contains(p))
        .collect();
    out.sort();
    out
}

/// Stem a textures-pipeline PNG path down to its material stem by stripping
/// any of the known companion suffixes. `None` when the file doesn't match
/// the pipeline's naming convention, in which case we delete only it.
pub(in crate::app) fn texture_material_stem(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    for suffix in TEXTURE_COMPANION_SUFFIXES {
        if let Some(stem) = name.strip_suffix(suffix) {
            return Some(stem.to_string());
        }
    }
    None
}

/// Delete `path` and — when the filename matches the textures pipeline's
/// `_albedo.png` / `_normal.png` / `_metallicRoughness.png` / `_ao.png`
/// convention — every companion PBR map that shares its material stem.
/// Returns a human-readable status string for the footer. Missing files are
/// silently skipped; unlink failures are collected into the message.
pub(in crate::app) fn delete_texture_group(path: &Path) -> String {
    let dir = match path.parent() {
        Some(p) => p,
        None => return format!("delete: {} has no parent dir", path.display()),
    };
    let mut targets: Vec<PathBuf> = Vec::new();
    if let Some(stem) = texture_material_stem(path) {
        for suffix in TEXTURE_COMPANION_SUFFIXES {
            let candidate = dir.join(format!("{stem}{suffix}"));
            if candidate.is_file() {
                targets.push(candidate);
            }
        }
    }
    if targets.is_empty() {
        targets.push(path.to_path_buf());
    }

    let mut deleted: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for t in &targets {
        match fs::remove_file(t) {
            Ok(()) => {
                if let Some(name) = t.file_name().and_then(|n| n.to_str()) {
                    deleted.push(name.to_string());
                }
            }
            Err(e) => {
                errors.push(format!("{}: {e}", t.display()));
            }
        }
    }

    if errors.is_empty() {
        match deleted.len() {
            0 => format!("textures: nothing to delete at {}", path.display()),
            1 => format!("textures: deleted {}", deleted[0]),
            n => format!("textures: deleted {n} files ({})", deleted.join(", ")),
        }
    } else if deleted.is_empty() {
        format!("textures: delete failed: {}", errors.join("; "))
    } else {
        format!(
            "textures: deleted {} but failed: {}",
            deleted.join(", "),
            errors.join("; "),
        )
    }
}

/// Slot attribute names that get cleared when the user deletes a material's
/// textures from the right-click menu. Kept aligned with the slots reported
/// by [`gather_texture_refs`] so the on-disk sweep and the source rewrite
/// agree on what counts as "the textures" for a material.
const MATERIAL_TEXTURE_ATTRS: [&str; 5] = [
    "base_color_texture",
    "metallic_roughness_texture",
    "normal_texture",
    "occlusion_texture",
    "emissive_texture",
];

/// Delete every PNG belonging to `material`'s slots and strip the
/// corresponding `*_texture` attrs from the source. Returns the rewritten
/// source plus a footer-status string. `refs` is the result of
/// [`gather_texture_refs`] for the current scene; only refs whose material
/// matches are touched. The source is left untouched when no attrs are
/// present (e.g. material lives inside an imported module so its span isn't
/// in this file).
pub(in crate::app) fn delete_material_textures(
    source: &str,
    source_dir: Option<&Path>,
    material: &str,
    refs: &[(String, &'static str, PathBuf)],
) -> (String, String) {
    // File sweep — `delete_texture_group` finds the material stem from any
    // one ref and unlinks every companion in its `_albedo/_normal/...`
    // family. Deduplicate by stem so we don't double-report.
    let mut file_status = String::new();
    let mut seen_stems: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (m, _, rel) in refs.iter().filter(|(m, _, _)| m == material) {
        let abs = resolve_for_check(rel, source_dir);
        let stem_key = texture_material_stem(&abs)
            .unwrap_or_else(|| abs.to_string_lossy().into_owned());
        if !seen_stems.insert(stem_key) {
            continue;
        }
        let _ = m;
        if !file_status.is_empty() {
            file_status.push_str("; ");
        }
        file_status.push_str(&delete_texture_group(&abs));
    }

    // Source rewrite — strip every `*_texture` attr from this material. The
    // span shifts after each delete, so re-resolve between iterations.
    let mut new_source = source.to_string();
    let mut stripped: u32 = 0;
    for attr in MATERIAL_TEXTURE_ATTRS {
        let Some(span) = find_material_source_span(&new_source, material) else {
            break;
        };
        let after = crate::edit::delete_attr(&new_source, span, attr);
        if after != new_source {
            new_source = after;
            stripped += 1;
        }
    }

    let cleared_msg = if stripped > 0 {
        format!(
            "; cleared {stripped} attr{} on \"{material}\"",
            if stripped == 1 { "" } else { "s" },
        )
    } else {
        String::new()
    };
    let status = if file_status.is_empty() && stripped == 0 {
        format!("textures: nothing to remove for \"{material}\"")
    } else if file_status.is_empty() {
        format!("textures: cleared {stripped} attr(s) on \"{material}\"")
    } else {
        format!("{file_status}{cleared_msg}")
    };
    (new_source, status)
}
