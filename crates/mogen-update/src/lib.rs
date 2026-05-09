//! Self-updater for the MoGen CLI and Studio.
//!
//! Checks the GitHub Releases API for a newer tagged release, downloads the
//! matching archive for the current host triple, extracts the bundled binaries
//! (`mogen` + `mogen-studio`), and atomically replaces them on disk. The
//! release artifact naming follows `release.yml`:
//!
//! - Unix:    `mogen-<tag>-<target>.tar.gz`  containing `mogen` and `mogen-studio`
//! - Windows: `mogen-<tag>-<target>.zip`     containing `mogen.exe` and `mogen-studio.exe`
//!
//! The crate exposes a small two-step API so both surfaces (CLI subcommand
//! and Studio dialog) can share the heavy lifting:
//!
//! 1. [`check`]                   — query the GitHub API, return the latest tag.
//! 2. [`download_and_apply`]      — download, extract, and replace binaries.
//!
//! Both functions are synchronous and block; callers that need a UI thread
//! (Studio) run them on a worker and forward [`Progress`] events through a
//! channel.
//!
//! ## Privileged installs
//!
//! When the install directory is owned by another user — the canonical case
//! is a system-managed `/usr/bin/mogen` on Linux — `download_and_apply`
//! detects this up-front (before downloading) and routes the file-swap step
//! through [`elevate::apply_with_elevation`], which re-launches the sibling
//! `mogen` CLI with the hidden subcommand `__apply-update --plan <path>`
//! under the platform's auth dialog (polkit / sudo / osascript / UAC). The
//! plan is a small JSON file describing the moves the privileged helper must
//! perform; see [`Plan`] for the wire format.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

mod archive;
mod elevate;

/// GitHub repo the updater talks to. Hardcoded — auto-update can only point at
/// one canonical release feed.
pub const REPO_OWNER: &str = "krazyjakee";
pub const REPO_NAME: &str = "MoGen";

const USER_AGENT: &str = concat!("mogen-update/", env!("CARGO_PKG_VERSION"));

/// Description of a release we found on GitHub. Returned by [`check`] so the
/// caller can decide whether to confirm with the user before downloading.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    /// The release tag (e.g. `v0.2.0`). Used in the asset filename.
    pub tag: String,
    /// Tag with any leading `v` stripped, for human-friendly version compare.
    pub version: String,
    /// HTML release page on GitHub, surfaced in the "What's new" link.
    pub html_url: String,
    /// Direct download URL of the archive that matches this host's target
    /// triple. `tar.gz` on Unix, `zip` on Windows.
    pub asset_url: String,
    /// Filename of `asset_url`, kept so the temp file can carry a recognisable
    /// extension (the extractor branches on `.zip` vs `.tar.gz`).
    pub asset_name: String,
    /// Compressed download size in bytes, taken from the GitHub asset record.
    /// Drives the progress bar denominator.
    pub asset_size: u64,
    /// Markdown-ish release notes from the GitHub release. May be empty.
    pub body: String,
}

/// Streaming progress events emitted while [`download_and_apply`] runs.
#[derive(Debug, Clone)]
pub enum Progress {
    /// The current stage label (e.g. "downloading", "extracting", "installing").
    /// Sent at every stage transition so a UI can update its caption.
    Stage(String),
    /// Bytes downloaded so far / total expected. `total` mirrors
    /// [`UpdateInfo::asset_size`] and is repeated for callers that throw away
    /// the original info.
    Download { downloaded: u64, total: u64 },
}

/// Outcome of a successful [`download_and_apply`] run.
#[derive(Debug, Clone)]
pub struct Applied {
    /// Path of the now-replaced binary that was running when the update
    /// started (i.e. the result of `current_exe()`).
    pub replaced_self: PathBuf,
    /// Path of the sibling binary that was replaced alongside, if it lived in
    /// the same directory. `None` when the sibling wasn't found on disk
    /// (custom installs, single-binary distributions).
    pub replaced_sibling: Option<PathBuf>,
    /// The tag we installed.
    pub tag: String,
    /// True when the swap was performed under platform-elevated privileges
    /// (pkexec / sudo / UAC / osascript-with-admin). Studio surfaces this in
    /// the "Updated" success line so the user knows the auth prompt they
    /// just answered did the right thing.
    pub elevated: bool,
}

/// A single source-to-destination move performed by the install step. Listed
/// in [`Plan::moves`] in the order they must be applied — siblings before the
/// running binary, so a partial failure leaves the install in a well-defined
/// "current binary still works" state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanMove {
    /// File the helper should read. Lives under the per-update temp dir
    /// produced by extraction.
    pub src: PathBuf,
    /// Final on-disk path the new file should occupy.
    pub dst: PathBuf,
}

/// JSON wire format passed from the unprivileged process to the privileged
/// helper via `mogen __apply-update --plan <path>`. Kept tiny on purpose —
/// the helper performs zero policy decisions, just file moves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// The release tag being installed. Carried so the helper can print a
    /// useful one-line confirmation on success.
    pub tag: String,
    /// File moves to perform, in order.
    pub moves: Vec<PlanMove>,
}

/// Compare a release tag against the running version. `tag` may be `v0.2.0`
/// or `0.2.0`; both parse the same. Returns true when `tag` strictly newer.
pub fn is_newer(tag: &str, current: &str) -> bool {
    let strip = |s: &str| s.trim().trim_start_matches('v').to_string();
    let lhs = match semver::Version::parse(&strip(tag)) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let rhs = match semver::Version::parse(&strip(current)) {
        Ok(v) => v,
        Err(_) => return false,
    };
    lhs > rhs
}

/// Query the GitHub Releases API for the latest published release and pick
/// the asset matching this host. `current_version` is the version the running
/// binary was built with (caller passes `env!("CARGO_PKG_VERSION")`).
pub fn check(current_version: &str) -> Result<CheckResult> {
    let url =
        format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");
    let resp = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .with_context(|| format!("GET {url}"))?;
    let release: GhRelease = resp
        .into_json()
        .context("parse GitHub releases JSON")?;

    let target = host_target()?;
    let asset = pick_asset(&release.assets, &target)
        .ok_or_else(|| anyhow!(
            "no release asset matches host target `{target}` in release {}",
            release.tag_name
        ))?;

    let info = UpdateInfo {
        tag: release.tag_name.clone(),
        version: release.tag_name.trim_start_matches('v').to_string(),
        html_url: release.html_url,
        asset_url: asset.browser_download_url.clone(),
        asset_name: asset.name.clone(),
        asset_size: asset.size,
        body: release.body.unwrap_or_default(),
    };
    let newer = is_newer(&info.tag, current_version);
    Ok(CheckResult { info, newer })
}

/// Wraps an [`UpdateInfo`] with a flag indicating whether the running binary
/// is older than what the release advertises.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub info: UpdateInfo,
    /// True when [`UpdateInfo::tag`] parses to a strictly higher semver than
    /// the version the caller passed to [`check`]. UIs gate the "Install"
    /// button on this so users aren't tempted to "downgrade" to the current
    /// version.
    pub newer: bool,
}

/// Download the asset described by `info`, extract it, and replace the
/// running binary plus any sibling MoGen binary in the same directory.
///
/// `on_progress` is invoked from the calling thread inline with the download
/// and stage transitions; it's expected to be cheap (sending on an mpsc
/// channel is the canonical use). It can be a no-op for headless callers.
///
/// When the install directory isn't writable by the running user — typically
/// `/usr/bin` on Linux — the file-swap step is performed under platform
/// elevation (polkit / sudo / UAC / osascript). The user sees the system
/// auth prompt; cancelling it surfaces as a clean error.
pub fn download_and_apply(
    info: &UpdateInfo,
    mut on_progress: impl FnMut(Progress),
) -> Result<Applied> {
    let current_exe =
        std::env::current_exe().context("locate current executable")?;
    // Canonicalise so the sibling lookup below compares apples to apples
    // (Windows symlinked /Program Files paths in particular).
    let current_exe = current_exe
        .canonicalize()
        .unwrap_or(current_exe);

    let install_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow!(
            "current executable has no parent directory: {}",
            current_exe.display()
        ))?
        .to_path_buf();

    // Pre-flight permission probe. Failing here turns a wasted 30 MB
    // download + obscure post-install error into a clean "go elevate"
    // branch *before* anything has touched the network.
    let install_writable = check_dir_writable(&install_dir).is_ok();

    on_progress(Progress::Stage("downloading".into()));
    let tmp_dir = tempdir_for_update()?;
    let archive_path = tmp_dir.join(&info.asset_name);
    download_to(&info.asset_url, &archive_path, info.asset_size, |downloaded| {
        on_progress(Progress::Download {
            downloaded,
            total: info.asset_size,
        })
    })?;

    on_progress(Progress::Stage("extracting".into()));
    let extract_dir = tmp_dir.join("extract");
    fs::create_dir_all(&extract_dir).context("create extract dir")?;
    archive::extract(&archive_path, &extract_dir)?;

    let self_basename = binary_basename_for_path(&current_exe);
    let new_self =
        find_binary(&extract_dir, self_basename)
            .ok_or_else(|| {
                anyhow!(
                    "release archive did not contain `{}`",
                    self_basename
                )
            })?;

    // Sibling: if we are `mogen`, look for `mogen-studio` next to us; vice
    // versa. The CLI and Studio ship together so updating one without the
    // other leaves the install in a half-upgraded state.
    let sibling_basename_str = sibling_basename(&current_exe);
    let sibling_on_disk = install_dir.join(&sibling_basename_str);
    let sibling_present = sibling_on_disk.is_file();
    let new_sibling = if sibling_present {
        find_binary(&extract_dir, &sibling_basename_str)
    } else {
        None
    };

    if install_writable {
        on_progress(Progress::Stage("installing".into()));
        return install_inline(
            &current_exe,
            &new_self,
            sibling_present.then_some(sibling_on_disk.as_path()),
            new_sibling.as_deref(),
            info.tag.clone(),
        );
    }

    // Elevated path: the install dir isn't writable, so we have to hand the
    // file moves to a privileged helper. Build a Plan describing every move
    // (sibling first, running binary last — same ordering as the inline path
    // for the same "fail safe" reason), serialise it next to the extracted
    // binaries, and re-launch the sibling `mogen` CLI under the platform's
    // auth dialog.
    on_progress(Progress::Stage("waiting for authorisation".into()));
    let mut moves = Vec::with_capacity(2);
    if let (true, Some(sib_src)) = (sibling_present, new_sibling.as_ref()) {
        moves.push(PlanMove {
            src: sib_src.clone(),
            dst: sibling_on_disk.clone(),
        });
    }
    moves.push(PlanMove {
        src: new_self.clone(),
        dst: current_exe.clone(),
    });
    let plan = Plan {
        tag: info.tag.clone(),
        moves,
    };
    let plan_path = tmp_dir.join("plan.json");
    let plan_bytes = serde_json::to_vec_pretty(&plan)
        .context("serialize update plan")?;
    fs::write(&plan_path, &plan_bytes)
        .with_context(|| format!("write plan to {}", plan_path.display()))?;

    let helper = elevate::helper_path_for(&current_exe);
    on_progress(Progress::Stage("installing (elevated)".into()));
    elevate::apply_with_elevation(&helper, &plan_path)?;

    Ok(Applied {
        replaced_self: current_exe,
        replaced_sibling: sibling_present.then_some(sibling_on_disk),
        tag: info.tag.clone(),
        elevated: true,
    })
}

/// Drop-in install path used when the install dir is writable by the running
/// user. Mirrors the pre-elevation behavior: sibling first (plain rename),
/// running binary last (`self_replace`).
fn install_inline(
    current_exe: &Path,
    new_self: &Path,
    sibling_dest: Option<&Path>,
    new_sibling: Option<&Path>,
    tag: String,
) -> Result<Applied> {
    if let (Some(target), Some(source)) = (sibling_dest, new_sibling) {
        replace_sibling(source, target).with_context(|| {
            format!("replace sibling binary at {}", target.display())
        })?;
    }
    // Replace ourselves last. `self_replace` handles the platform-specific
    // dance: on Unix it relies on the kernel keeping the open inode alive so
    // a plain rename works; on Windows it renames the running .exe to .old
    // so the new file can take its place in the same directory.
    self_replace::self_replace(new_self).context("replace self")?;
    // self_replace doesn't preserve mode bits on Unix when the source is a
    // freshly-extracted file with the wrong permissions. Re-mark the
    // installed binary executable to be safe.
    #[cfg(unix)]
    ensure_executable(current_exe);

    Ok(Applied {
        replaced_self: current_exe.to_path_buf(),
        replaced_sibling: sibling_dest.map(Path::to_path_buf),
        tag,
        elevated: false,
    })
}

/// Stream a remote URL into `dest`, calling `on_chunk` after each network read
/// with the running byte total so the caller can drive a progress bar.
fn download_to(
    url: &str,
    dest: &Path,
    expected_size: u64,
    mut on_chunk: impl FnMut(u64),
) -> Result<()> {
    let resp = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/octet-stream")
        .timeout(std::time::Duration::from_secs(60))
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut reader = resp.into_reader();
    let mut file = fs::File::create(dest)
        .with_context(|| format!("create {}", dest.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = reader.read(&mut buf).context("read chunk from server")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("write chunk to disk")?;
        total += n as u64;
        on_chunk(total);
    }
    file.flush().ok();
    if expected_size > 0 && total != expected_size {
        // Not fatal — GitHub sometimes serves a slightly different
        // content-length when assets are re-encoded — but worth surfacing
        // when it disagrees materially. We let extraction validate the bytes.
    }
    Ok(())
}

/// Walk `root` for a regular file whose basename equals `name`. Releases nest
/// the binaries one or two directories deep depending on packaging, so a
/// shallow recursive scan keeps us robust to layout changes.
fn find_binary(root: &Path, name: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, out: &mut Option<PathBuf>) -> io::Result<()> {
        if out.is_some() {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                walk(&path, name, out)?;
            } else if ft.is_file()
                && path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case(name))
                    .unwrap_or(false)
            {
                *out = Some(path);
                return Ok(());
            }
        }
        Ok(())
    }
    let mut found = None;
    let _ = walk(root, name, &mut found);
    found
}

/// Move `source` over `dest`, falling back to copy+remove when a cross-device
/// rename fails (the temp dir may live on a different filesystem than the
/// install root). Used for the sibling binary; the running binary uses
/// `self_replace::self_replace` instead.
fn replace_sibling(source: &Path, dest: &Path) -> Result<()> {
    // Try a rename first — atomic when source and dest are on the same fs.
    if fs::rename(source, dest).is_ok() {
        #[cfg(unix)]
        ensure_executable(dest);
        return Ok(());
    }
    // Cross-device fallback: copy with a `.new` suffix, then atomic rename
    // over the destination so we never leave a half-written binary visible.
    let staging = dest.with_extension("mogen-update.new");
    fs::copy(source, &staging)
        .with_context(|| format!("copy to staging {}", staging.display()))?;
    #[cfg(unix)]
    ensure_executable(&staging);
    fs::rename(&staging, dest)
        .with_context(|| format!("rename staging onto {}", dest.display()))?;
    Ok(())
}

#[cfg(unix)]
fn ensure_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path) {
        let mut perm = meta.permissions();
        let mode = perm.mode() | 0o755;
        perm.set_mode(mode);
        let _ = fs::set_permissions(path, perm);
    }
}

/// Probe whether the running user can create files in `dir`. We deliberately
/// don't try to interpret mode bits / ACLs / capabilities ourselves — the
/// only reliable answer comes from attempting an actual filesystem write.
fn check_dir_writable(dir: &Path) -> Result<()> {
    let probe = dir.join(format!(
        ".mogen-update-probe-{}-{}",
        std::process::id(),
        nano_unique(),
    ));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(anyhow::Error::new(e).context(format!(
            "writable probe failed for {}",
            dir.display()
        ))),
    }
}

/// Read a `Plan` from disk and apply every move in order. Called from the
/// elevated helper subcommand (`mogen __apply-update --plan <path>`).
///
/// On Linux/macOS, plain `fs::rename` works even if the destination is the
/// running executable of *another* process, because the kernel keeps the
/// inode alive while it's open. On Windows we have to rename the destination
/// out of the way first (the OS holds an exclusive lock on running .exes).
pub fn apply_plan(plan_path: &Path) -> Result<Plan> {
    let bytes = fs::read(plan_path)
        .with_context(|| format!("read plan from {}", plan_path.display()))?;
    let plan: Plan =
        serde_json::from_slice(&bytes).context("parse plan JSON")?;
    for mv in &plan.moves {
        external_replace(&mv.src, &mv.dst)
            .with_context(|| format!("install {}", mv.dst.display()))?;
    }
    Ok(plan)
}

/// File-replace primitive used by [`apply_plan`]. Tolerant of cross-device
/// renames (temp on a different fs to install) and on Windows of the
/// destination being held open by another process.
fn external_replace(source: &Path, dest: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        // The destination is almost always currently-running mogen.exe or
        // mogen-studio.exe; rename it aside so the new binary can take the
        // canonical name. The aside file lives until next reboot or manual
        // cleanup — same trade-off the `self_replace` crate makes.
        if dest.is_file() {
            let aside = dest.with_extension(format!(
                "mogen-old-{}",
                nano_unique()
            ));
            fs::rename(dest, &aside).with_context(|| {
                format!("rename {} aside", dest.display())
            })?;
        }
    }

    // Try a direct rename first — atomic when source and dest share a
    // filesystem and the kernel doesn't object.
    if fs::rename(source, dest).is_ok() {
        #[cfg(unix)]
        ensure_executable(dest);
        return Ok(());
    }
    // Cross-device fallback: stage in dest's directory (which we now have
    // write access to, by construction — we're either root or already past
    // the writability check), then atomic rename onto dest.
    let dest_dir = dest.parent().ok_or_else(|| {
        anyhow!("destination has no parent directory: {}", dest.display())
    })?;
    let staging = dest_dir.join(format!(
        ".mogen-update-stage-{}-{}",
        std::process::id(),
        nano_unique(),
    ));
    fs::copy(source, &staging)
        .with_context(|| format!("stage copy to {}", staging.display()))?;
    #[cfg(unix)]
    ensure_executable(&staging);
    fs::rename(&staging, dest).with_context(|| {
        format!("rename staging {} -> {}", staging.display(), dest.display())
    })?;
    Ok(())
}

/// Returns the basename a current binary should be matched against in the
/// extracted archive, preserving the platform-specific `.exe` suffix on
/// Windows.
fn binary_basename_for_path(path: &Path) -> &str {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("mogen")
}

/// Compute the basename of the *other* MoGen binary that should be replaced
/// alongside the running one. CLI ↔ Studio.
fn sibling_basename(current_exe: &Path) -> String {
    // We can't rely on `Path::file_name` here in cross-platform tests:
    // a Windows-style path passed to a Linux build won't see `\` as a
    // separator. Strip both separators manually so the same code matches
    // `/usr/bin/mogen` and `C:\bin\mogen.exe` regardless of host OS.
    let name = binary_basename_for_path(current_exe);
    let trimmed: &str = name
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(name);
    let (stem, ext) = match trimmed.rsplit_once('.') {
        Some((s, "exe")) => (s, ".exe"),
        _ => (trimmed, ""),
    };
    let other = if stem == "mogen-studio" {
        "mogen"
    } else {
        // Default: assume CLI, sibling is Studio. Covers `mogen` and any
        // unexpected name (better to try and miss than to silently skip).
        "mogen-studio"
    };
    format!("{other}{ext}")
}

/// Detect the Rust target triple this binary was built for. The release
/// archives embed the triple in their filename, so this is what we match
/// against in [`pick_asset`].
fn host_target() -> Result<&'static str> {
    // Compile-time selection is the only way to know the triple — there's no
    // standard runtime API. The list mirrors the `release.yml` matrix.
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    return Ok("x86_64-unknown-linux-gnu");
    #[cfg(all(target_os = "linux", target_arch = "aarch64", target_env = "gnu"))]
    return Ok("aarch64-unknown-linux-gnu");
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return Ok("x86_64-apple-darwin");
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Ok("aarch64-apple-darwin");
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Ok("x86_64-pc-windows-msvc");
    #[allow(unreachable_code)]
    {
        bail!("auto-update is not supported on this host")
    }
}

/// Choose the asset whose name embeds `target` and whose extension matches
/// what the extractor knows how to handle on this platform. `.tar.gz` and
/// `.tgz` are accepted on Unix; `.zip` on Windows.
fn pick_asset<'a>(assets: &'a [GhAsset], target: &str) -> Option<&'a GhAsset> {
    let prefer_zip = cfg!(windows);
    assets.iter().find(|a| {
        if !a.name.contains(target) {
            return false;
        }
        let lname = a.name.to_ascii_lowercase();
        if prefer_zip {
            lname.ends_with(".zip")
        } else {
            lname.ends_with(".tar.gz") || lname.ends_with(".tgz")
        }
    })
}

/// Make a fresh per-process temp directory under the system tempdir. Avoids
/// pulling in the `tempfile` crate — we just need a unique name, and we own
/// the cleanup window.
fn tempdir_for_update() -> Result<PathBuf> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = nano_unique();
    let dir = base.join(format!("mogen-update-{pid}-{nanos}"));
    fs::create_dir_all(&dir)
        .with_context(|| format!("create temp dir {}", dir.display()))?;
    Ok(dir)
}

/// Sub-second component of the wall clock, used as a uniqueness salt for
/// per-update temp paths. Falls back to 0 on systems with a borked clock.
fn nano_unique() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

// --- GitHub release wire types --------------------------------------------

#[derive(Deserialize, Debug)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    body: Option<String>,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize, Debug)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_strips_v_prefix() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.2.0", "v0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
    }

    #[test]
    fn newer_rejects_garbage() {
        assert!(!is_newer("not-a-version", "0.1.0"));
        assert!(!is_newer("v0.1.0", "not-a-version"));
    }

    #[test]
    fn pick_asset_prefers_format_per_os() {
        let assets = vec![
            GhAsset {
                name: "mogen-v0.2.0-x86_64-unknown-linux-gnu.tar.gz".into(),
                size: 1,
                browser_download_url: "u1".into(),
            },
            GhAsset {
                name: "mogen-v0.2.0-x86_64-pc-windows-msvc.zip".into(),
                size: 1,
                browser_download_url: "u2".into(),
            },
        ];
        let triple = if cfg!(windows) {
            "x86_64-pc-windows-msvc"
        } else {
            "x86_64-unknown-linux-gnu"
        };
        let pick = pick_asset(&assets, triple).expect("asset");
        assert!(pick.name.contains(triple));
    }

    #[test]
    fn sibling_swap() {
        assert_eq!(sibling_basename(Path::new("/usr/bin/mogen")), "mogen-studio");
        assert_eq!(sibling_basename(Path::new("/x/mogen-studio")), "mogen");
        assert_eq!(
            sibling_basename(Path::new(r"C:\bin\mogen.exe")),
            "mogen-studio.exe"
        );
        assert_eq!(
            sibling_basename(Path::new(r"C:\bin\mogen-studio.exe")),
            "mogen.exe"
        );
    }

    #[test]
    fn check_dir_writable_passes_for_tempdir() {
        let tmp = std::env::temp_dir();
        assert!(check_dir_writable(&tmp).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn check_dir_writable_fails_for_root_owned_dir() {
        // /usr is root-owned and read-only for normal users on every CI
        // image we care about. Skip the assertion when the test happens
        // to run as root (CI containers occasionally do).
        let uid = unsafe { getuid() };
        if uid == 0 {
            return;
        }
        let dir = Path::new("/usr");
        if !dir.is_dir() {
            return;
        }
        assert!(check_dir_writable(dir).is_err());
    }

    #[cfg(unix)]
    extern "C" {
        fn getuid() -> u32;
    }

    #[test]
    fn plan_round_trips_through_json() {
        let plan = Plan {
            tag: "v9.9.9".into(),
            moves: vec![
                PlanMove {
                    src: PathBuf::from("/tmp/mogen-update-1/mogen"),
                    dst: PathBuf::from("/usr/bin/mogen"),
                },
                PlanMove {
                    src: PathBuf::from("/tmp/mogen-update-1/mogen-studio"),
                    dst: PathBuf::from("/usr/bin/mogen-studio"),
                },
            ],
        };
        let bytes = serde_json::to_vec(&plan).unwrap();
        let parsed: Plan = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.tag, "v9.9.9");
        assert_eq!(parsed.moves.len(), 2);
        assert_eq!(parsed.moves[0].dst, PathBuf::from("/usr/bin/mogen"));
    }

    #[test]
    fn apply_plan_round_trip_in_tempdir() {
        // Build a plan where source files live under one tempdir and the
        // destination dir is another. apply_plan should move them across.
        let base = std::env::temp_dir().join(format!(
            "mogen-update-test-{}-{}",
            std::process::id(),
            nano_unique(),
        ));
        let src_dir = base.join("src");
        let dst_dir = base.join("dst");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&dst_dir).unwrap();

        let src_a = src_dir.join("mogen");
        let src_b = src_dir.join("mogen-studio");
        fs::write(&src_a, b"new-cli").unwrap();
        fs::write(&src_b, b"new-studio").unwrap();

        let dst_a = dst_dir.join("mogen");
        let dst_b = dst_dir.join("mogen-studio");
        fs::write(&dst_a, b"old-cli").unwrap();
        fs::write(&dst_b, b"old-studio").unwrap();

        let plan = Plan {
            tag: "v0.0.1".into(),
            moves: vec![
                PlanMove { src: src_a, dst: dst_a.clone() },
                PlanMove { src: src_b, dst: dst_b.clone() },
            ],
        };
        let plan_path = base.join("plan.json");
        fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();

        apply_plan(&plan_path).expect("apply ok");
        assert_eq!(fs::read(&dst_a).unwrap(), b"new-cli");
        assert_eq!(fs::read(&dst_b).unwrap(), b"new-studio");

        // Best-effort cleanup; not asserted because the test passes either
        // way and on Windows the run-binary aside file may still exist.
        let _ = fs::remove_dir_all(&base);
    }
}
