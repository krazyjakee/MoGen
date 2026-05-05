//! Bundle the texture/image assets referenced by a publish into
//! `PublishTextureInput` rows.
//!
//! Walks the entry source plus every locally bundled import (the same
//! `(filename, source)` pairs `kick_publish` already gathers via
//! `mogen_dsl::collect_local_import_files`), collects every string-valued
//! attribute whose value ends in `.png`/`.jpg`/`.jpeg`/`.webp`, resolves
//! each path against the directory of the source file that referenced
//! it, and reads the bytes off disk.
//!
//! Each texture is shipped under its **path relative to the entry's
//! directory**, with forward slashes, so the published filename matches
//! the path the `.mog` source already references (`textures/foo/wood.png`
//! stays `textures/foo/wood.png`). The moghub server's
//! `assets::sanitise_filename` accepts forward-slash relative paths,
//! rejecting traversal/absolute forms; mirroring the on-disk layout
//! means the consumer's lookup just works.
//!
//! Textures that resolve outside `entry_dir` are rejected for the same
//! reason `collect_local_import_files` rejects out-of-tree imports —
//! publish-bundling can't reach into a parent directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use mogen_dsl::{parse, Node, Value};
use mogen_moghub_client::PublishTextureInput;

/// Image extensions that match `is_texture_filename` on the moghub
/// server. Lowercase; comparisons strip the input first.
const TEXTURE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp"];

/// Collect every texture referenced by `entry_source` or any of the
/// bundled `imports`, resolve each path on disk, and return the rows the
/// publish payload should ship.
///
/// `entry_dir` is the directory of the entry `.mog`. Each import filename
/// is interpreted relative to it (matching
/// `mogen_dsl::collect_local_import_files`); the importing file's own
/// directory is used as the base when resolving texture paths inside
/// that file. The returned filenames are forward-slash relative paths
/// from `entry_dir`.
///
/// Errors are returned as `String` so the caller can drop them straight
/// into the publish dialog's `error` field.
pub(super) fn collect_publish_textures(
    entry_dir: &Path,
    entry_source: &str,
    imports: &[(String, String)],
) -> Result<Vec<PublishTextureInput>, String> {
    let entry_dir_canonical = std::fs::canonicalize(entry_dir).map_err(|e| {
        format!(
            "canonicalising publish base dir {}: {e}",
            entry_dir.display()
        )
    })?;

    // canonical disk path -> relative-to-entry forward-slash filename.
    // Dedupes shared textures referenced from multiple .mog files.
    let mut refs: HashMap<PathBuf, String> = HashMap::new();

    add_refs_from_source(entry_source, &entry_dir_canonical, &entry_dir_canonical, &mut refs)?;

    for (rel_filename, src) in imports {
        let import_path = entry_dir_canonical.join(rel_filename);
        let import_dir = import_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| entry_dir_canonical.clone());
        add_refs_from_source(src, &import_dir, &entry_dir_canonical, &mut refs)?;
    }

    let mut out = Vec::with_capacity(refs.len());
    for (disk_path, filename) in refs {
        let bytes = std::fs::read(&disk_path).map_err(|e| {
            format!(
                "reading texture {}: {e}",
                disk_path.display()
            )
        })?;
        out.push(PublishTextureInput {
            filename,
            bytes_base64: STANDARD.encode(bytes),
        });
    }
    // Stable order so successive publishes diff cleanly in tests.
    out.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(out)
}

fn add_refs_from_source(
    source: &str,
    base_dir: &Path,
    entry_dir_canonical: &Path,
    refs: &mut HashMap<PathBuf, String>,
) -> Result<(), String> {
    let ast = parse(source).map_err(|e| format!("parsing source for texture bundling: {e}"))?;
    for node in &ast {
        collect_from_node(node, base_dir, entry_dir_canonical, refs)?;
    }
    Ok(())
}

fn collect_from_node(
    node: &Node,
    base_dir: &Path,
    entry_dir_canonical: &Path,
    refs: &mut HashMap<PathBuf, String>,
) -> Result<(), String> {
    for (_, value) in &node.attrs {
        if let Value::String(s) = value {
            if !looks_like_texture_path(s) {
                continue;
            }
            let disk_path = base_dir.join(s);
            let canonical = std::fs::canonicalize(&disk_path)
                .map_err(|e| format!("resolving texture {}: {e}", disk_path.display()))?;
            let rel = canonical.strip_prefix(entry_dir_canonical).map_err(|_| {
                format!(
                    "texture {} resolves to {} which is outside the entry's directory {} \
                     — publish-bundling can't reach into a parent dir; move the file beside \
                     the entry or flatten the layout",
                    s,
                    canonical.display(),
                    entry_dir_canonical.display()
                )
            })?;
            let filename = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            refs.entry(canonical).or_insert(filename);
        }
    }
    for child in &node.children {
        collect_from_node(child, base_dir, entry_dir_canonical, refs)?;
    }
    Ok(())
}

fn looks_like_texture_path(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    TEXTURE_EXTS.iter().any(|ext| {
        let needle = format!(".{ext}");
        lower.ends_with(&needle)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fresh_tempdir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mogen-publish-textures-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collects_textures_with_relative_path() {
        let dir = fresh_tempdir("entry-only");
        fs::create_dir_all(dir.join("textures/foo")).unwrap();
        fs::write(dir.join("textures/foo/wood.png"), b"PNGDATA").unwrap();
        let src = r#"
            material "wood" (color=[0.5, 0.4, 0.3], base_color_texture="textures/foo/wood.png")
            box (size=[1,1,1], material="wood")
        "#;
        let out = collect_publish_textures(&dir, src, &[]).expect("collect");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].filename, "textures/foo/wood.png");
        assert_eq!(STANDARD.decode(&out[0].bytes_base64).unwrap(), b"PNGDATA");
    }

    #[test]
    fn dedupes_when_shared_across_imports() {
        let dir = fresh_tempdir("shared");
        fs::create_dir_all(dir.join("textures")).unwrap();
        fs::write(dir.join("textures/wood.png"), b"BYTES").unwrap();
        let entry = r#"
            material "a" (base_color_texture="textures/wood.png")
        "#;
        let imp_src = r#"
            material "b" (base_color_texture="textures/wood.png")
        "#;
        let imports = vec![("part.mog".to_string(), imp_src.to_string())];
        let out = collect_publish_textures(&dir, entry, &imports).expect("collect");
        assert_eq!(out.len(), 1, "shared texture should appear once");
        assert_eq!(out[0].filename, "textures/wood.png");
    }

    #[test]
    fn same_basename_distinct_paths_both_published() {
        let dir = fresh_tempdir("twins");
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::create_dir_all(dir.join("b")).unwrap();
        fs::write(dir.join("a/wood.png"), b"A").unwrap();
        fs::write(dir.join("b/wood.png"), b"B").unwrap();
        let src = r#"
            material "x" (base_color_texture="a/wood.png")
            material "y" (base_color_texture="b/wood.png")
        "#;
        let out = collect_publish_textures(&dir, src, &[]).expect("collect");
        assert_eq!(out.len(), 2);
        let names: Vec<&str> = out.iter().map(|t| t.filename.as_str()).collect();
        assert!(names.contains(&"a/wood.png"));
        assert!(names.contains(&"b/wood.png"));
    }

    #[test]
    fn imports_resolve_relative_to_their_own_dir_paths_remain_entry_relative() {
        let dir = fresh_tempdir("import-dir");
        fs::create_dir_all(dir.join("sub/textures")).unwrap();
        fs::write(dir.join("sub/textures/leaf.png"), b"LEAF").unwrap();
        let entry = r#""#;
        let imp_src = r#"
            material "leaf" (base_color_texture="textures/leaf.png")
        "#;
        let imports = vec![("sub/part.mog".to_string(), imp_src.to_string())];
        let out = collect_publish_textures(&dir, entry, &imports).expect("collect");
        assert_eq!(out.len(), 1);
        // The import's `textures/leaf.png` resolves against `sub/`, but the
        // published filename is the path relative to the entry dir so the
        // server stores it under `sub/textures/leaf.png`.
        assert_eq!(out[0].filename, "sub/textures/leaf.png");
    }

    #[test]
    fn ignores_non_image_strings() {
        let dir = fresh_tempdir("non-image");
        let src = r#"
            meta(prompt="a wooden chair", seed=42)
            material "wood" (color=[0.5, 0.4, 0.3])
        "#;
        let out = collect_publish_textures(&dir, src, &[]).expect("collect");
        assert!(out.is_empty());
    }

    #[test]
    fn rejects_texture_outside_entry_dir() {
        let dir = fresh_tempdir("escape-parent");
        let outside = dir.parent().unwrap().to_path_buf();
        let stray = outside.join(format!(
            "mogen-publish-stray-{}.png",
            std::process::id()
        ));
        fs::write(&stray, b"X").unwrap();
        let rel = format!("../{}", stray.file_name().unwrap().to_string_lossy());
        let src = format!(r#"material "x" (base_color_texture="{rel}")"#);
        let err = collect_publish_textures(&dir, &src, &[]).unwrap_err();
        let _ = fs::remove_file(&stray);
        assert!(err.contains("outside the entry"), "got: {err}");
    }
}
