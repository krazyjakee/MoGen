//! Archive extraction for downloaded release assets.
//!
//! `tar.gz` is decoded with `flate2` + `tar` (Unix releases); `.zip` with the
//! `zip` crate (Windows). The branching is on filename rather than host OS so
//! a future switch to a uniform format only changes one place.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// Extract `archive` into `dest`. Caller is responsible for ensuring `dest`
/// exists and is empty.
pub(crate) fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let lname = archive
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if lname.ends_with(".tar.gz") || lname.ends_with(".tgz") {
        extract_tar_gz(archive, dest)
    } else if lname.ends_with(".zip") {
        extract_zip(archive, dest)
    } else {
        Err(anyhow!(
            "unsupported release archive format: {}",
            archive.display()
        ))
    }
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    let f = fs::File::open(archive)
        .with_context(|| format!("open archive {}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut tar = tar::Archive::new(gz);
    tar.set_preserve_permissions(true);
    tar.unpack(dest)
        .with_context(|| format!("extract tar.gz into {}", dest.display()))?;
    Ok(())
}

#[cfg(windows)]
fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let f = fs::File::open(archive)
        .with_context(|| format!("open archive {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(f)
        .with_context(|| format!("read zip {}", archive.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).context("read zip entry")?;
        let rel = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        let out_path = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out_path).ok();
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).ok();
        }
        let mut out = fs::File::create(&out_path)
            .with_context(|| format!("create {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("write {}", out_path.display()))?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn extract_zip(_archive: &Path, _dest: &Path) -> Result<()> {
    // The release pipeline doesn't ship .zip on non-Windows hosts (they get
    // .tar.gz), so this path is unreachable in practice. Kept as an explicit
    // error so a future change doesn't silently fail.
    Err(anyhow!("zip extraction is only enabled on Windows builds"))
}
