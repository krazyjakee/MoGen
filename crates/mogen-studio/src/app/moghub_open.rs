//! End-to-end flow for opening a MoGHub model from a `mogen://` URL.
//!
//! Phases (driven by [`Flow`] state on [`MogenStudioApp`]):
//!
//! 1. **Pending** — URL captured at launch; held until the splash drains.
//! 2. **Folder pick** — native `rfd` directory picker (blocking, runs on
//!    the UI thread; matches Save-As behaviour).
//! 3. **Working** — worker thread fetches `model_detail` (for entry
//!    filename + version), pulls `download.zip`, unzips into
//!    `<picked>/<user>-<slug>-v<n>/`, and posts the entry `.mog` path
//!    back via mpsc.
//! 4. **Open** — UI thread receives the path and routes through
//!    `open_path`, the same code Open / Recent uses.
//!
//! No textures or imports need ad-hoc handling: the server-built zip
//! already carries entry + sibling files + textures + transitive
//! registry deps in their canonical relative paths, so a plain unzip
//! gets us a directory the existing pipeline can build out of.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use mogen_moghub_client::MoghubClient;

use crate::protocol::MogenUrl;

/// Flow state. `None` means nothing is in progress; one of these is held
/// across frames while a download is being chosen / running.
pub(super) enum Flow {
    /// URL captured at launch. We wait until the splash drains before
    /// raising the folder picker so the user isn't ambushed mid-init.
    Pending(MogenUrl),
    /// Worker thread is fetching + unzipping. UI shows a status line.
    Working {
        label: String,
        rx: Receiver<Outcome>,
    },
    /// Last attempt failed. Sticky until the user kicks off something
    /// else (or relaunches with a fresh URL).
    Failed(String),
}

/// Result the worker posts back. On success `entry_path` is the on-disk
/// `.mog` to feed to `open_path`.
pub(super) struct Outcome {
    pub(super) result: Result<PathBuf, String>,
}

/// Spawn a worker that resolves the model, downloads the zip, unzips
/// into `dest_root`, and returns the entry `.mog` path.
pub(super) fn spawn(
    base_url: String,
    token: String,
    ctx: egui::Context,
    url: MogenUrl,
    dest_root: PathBuf,
) -> Receiver<Outcome> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = run(&base_url, &token, &url, &dest_root);
        let _ = tx.send(Outcome { result });
        ctx.request_repaint();
    });
    rx
}

fn run(
    base_url: &str,
    token: &str,
    url: &MogenUrl,
    dest_root: &Path,
) -> Result<PathBuf, String> {
    let MogenUrl::MoghubOpen { user, slug, version } = url;

    let mut client = MoghubClient::new(base_url)
        .map_err(|e| format!("constructing moghub client for {base_url}: {e}"))?;
    if !token.is_empty() {
        client = client.with_token(Some(token.to_string()));
    }

    // Resolve the entry filename + version. We only need this for naming
    // the local folder and knowing which `.mog` to open after the unzip
    // — the bytes themselves come from `download.zip`. `version_detail`
    // is used when the URL pinned a specific version; otherwise the
    // latest from `model_detail`.
    let (entry_filename, version_num) = match version {
        Some(v) => {
            let d = client
                .version_detail(user, slug, *v)
                .map_err(|e| format!("fetching @{user}/{slug} v{v}: {e}"))?;
            let entry = d
                .version
                .files
                .iter()
                .find(|f| f.is_entry)
                .ok_or_else(|| format!("@{user}/{slug} v{v} has no entry file"))?
                .filename
                .clone();
            (entry, d.version.version)
        }
        None => {
            let d = client
                .model_detail(user, slug)
                .map_err(|e| format!("fetching @{user}/{slug}: {e}"))?;
            let entry = d
                .version
                .files
                .iter()
                .find(|f| f.is_entry)
                .ok_or_else(|| format!("@{user}/{slug} has no entry file"))?
                .filename
                .clone();
            (entry, d.version.version)
        }
    };

    let zip_bytes = client
        .download_zip(user, slug)
        .map_err(|e| format!("downloading @{user}/{slug}: {e}"))?;

    let folder_name = format!("{user}-{slug}-v{version_num}");
    let dest_dir = dest_root.join(&folder_name);
    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        return Err(format!("creating {}: {e}", dest_dir.display()));
    }

    extract_zip(&zip_bytes, &dest_dir)
        .map_err(|e| format!("unzipping into {}: {e}", dest_dir.display()))?;

    let entry_path = dest_dir.join(&entry_filename);
    if !entry_path.is_file() {
        return Err(format!(
            "entry `{}` not found after unzip in {}",
            entry_filename,
            dest_dir.display()
        ));
    }
    Ok(entry_path)
}

/// Extract every zip entry under `dest`, refusing any path that escapes
/// the destination root. The server controls archive contents but we
/// still apply a path-traversal check so a hostile or malformed archive
/// can't write outside the user's chosen folder.
fn extract_zip(zip_bytes: &[u8], dest: &Path) -> std::io::Result<()> {
    let cursor = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        // `enclosed_name` rejects absolute paths, drive letters, and
        // any `..` components that would escape the archive root —
        // exactly the path-traversal guard we need.
        let Some(rel) = file.enclosed_name() else {
            continue;
        };
        let rel = rel.to_path_buf();
        let out_path = dest.join(&rel);
        if file.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)?;
        std::fs::write(&out_path, &buf)?;
    }
    Ok(())
}
