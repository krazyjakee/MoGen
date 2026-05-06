//! Bundle the texture/image assets referenced by a publish into
//! `PublishTextureInput` rows. CLI-side mirror of
//! `mogen-studio/src/app/publish_textures.rs` — kept in lockstep with
//! that file so `mogen moghub publish` matches Studio byte-for-byte.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use mogen_dsl::{parse, Node, Value};
use mogen_moghub_client::PublishTextureInput;

const TEXTURE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp"];

pub(crate) fn collect_publish_textures(
    entry_dir: &Path,
    entry_source: &str,
    imports: &[(String, String)],
) -> Result<Vec<PublishTextureInput>> {
    let entry_dir_canonical = std::fs::canonicalize(entry_dir).map_err(|e| {
        anyhow!(
            "canonicalising publish base dir {}: {e}",
            entry_dir.display()
        )
    })?;

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
        let bytes = std::fs::read(&disk_path)
            .map_err(|e| anyhow!("reading texture {}: {e}", disk_path.display()))?;
        out.push(PublishTextureInput {
            filename,
            bytes_base64: STANDARD.encode(bytes),
        });
    }
    out.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(out)
}

fn add_refs_from_source(
    source: &str,
    base_dir: &Path,
    entry_dir_canonical: &Path,
    refs: &mut HashMap<PathBuf, String>,
) -> Result<()> {
    let ast = parse(source).map_err(|e| anyhow!("parsing source for texture bundling: {e}"))?;
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
) -> Result<()> {
    for (_, value) in &node.attrs {
        if let Value::String(s) = value {
            if !looks_like_texture_path(s) {
                continue;
            }
            let disk_path = base_dir.join(s);
            let canonical = std::fs::canonicalize(&disk_path)
                .map_err(|e| anyhow!("resolving texture {}: {e}", disk_path.display()))?;
            let rel = canonical.strip_prefix(entry_dir_canonical).map_err(|_| {
                anyhow!(
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
