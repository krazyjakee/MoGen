//! Publish-time bundling: walk transitive `import "path.mog"` directives
//! and return the source for every reachable sibling so the publisher can
//! upload them in one `PublishRequest`. Distinct from the runtime import
//! resolver — this pass doesn't lift declarations or rewrite paths, it
//! just collects raw `(filename, source)` pairs and refuses to reach
//! outside the entry's directory.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};

use crate::parser::parse;

use super::super::loader::{FsLoader, Loader};

/// Walk transitive `import "path.mog"` directives starting from `entry_source`
/// and return every reachable sibling `.mog` file as `(filename, source)` pairs.
/// Used by the publisher to bundle a scene with its local imports into a
/// single multi-file `PublishRequest`. Registry uses (`use "@user/slug[@v]"`)
/// are external dependencies and intentionally skipped — those resolve through
/// `mog.lock` on the consumer side, not as bundled bytes.
///
/// Each filename is the imported file's path relative to `entry_dir`, with
/// platform-native separators normalised to forward slashes so the same
/// filename round-trips through the moghub server (which stores
/// `model_files.filename` as a string and joins it back on the consumer side).
///
/// Errors:
/// - the entry source fails to parse;
/// - an `import` resolves to a path outside `entry_dir` (publishers can't
///   reach into a parent directory — the user must move the file or flatten
///   the layout);
/// - any imported file fails to load or parse.
///
/// `entry_dir` is canonicalised before traversal so symlink hops and
/// `..`/`.` segments resolve consistently with the cycle-detection used by
/// [`super::resolve_imports`].
pub fn collect_local_import_files(
    entry_dir: &Path,
    entry_source: &str,
) -> Result<Vec<(String, String)>> {
    let entry_dir_canonical = std::fs::canonicalize(entry_dir)
        .with_context(|| format!("canonicalising publish base dir {}", entry_dir.display()))?;
    let mut loader = FsLoader::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut out: Vec<(String, String)> = Vec::new();
    collect_local_imports_into(
        entry_source,
        Some(entry_dir_canonical.as_path()),
        &entry_dir_canonical,
        &mut loader,
        &mut visited,
        &mut out,
    )?;
    Ok(out)
}

fn collect_local_imports_into(
    source: &str,
    base_dir: Option<&Path>,
    entry_dir_canonical: &Path,
    loader: &mut FsLoader,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<(String, String)>,
) -> Result<()> {
    let ast = parse(source).map_err(|e| anyhow!("parsing source for publish bundling: {e}"))?;
    for n in &ast {
        if n.kind != "import" {
            continue;
        }
        let raw = n.name.as_deref().ok_or_else(|| {
            anyhow!("`import` requires a quoted file path, e.g. `import \"shared.mog\"`")
        })?;
        let loaded = loader.load(raw, base_dir)?;
        if !visited.insert(loaded.canonical.clone()) {
            continue;
        }
        let rel = loaded
            .canonical
            .strip_prefix(entry_dir_canonical)
            .map_err(|_| {
                anyhow!(
                    "import \"{raw}\" resolves to {} which is outside the entry's directory \
                     {} — publish-bundling can't reach into a parent dir; move the file \
                     beside the entry or flatten the layout",
                    loaded.canonical.display(),
                    entry_dir_canonical.display()
                )
            })?;
        let filename = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let inner_dir = loaded.canonical.parent().map(|p| p.to_path_buf());
        out.push((filename, loaded.source.clone()));
        collect_local_imports_into(
            &loaded.source,
            inner_dir.as_deref(),
            entry_dir_canonical,
            loader,
            visited,
            out,
        )?;
    }
    Ok(())
}
