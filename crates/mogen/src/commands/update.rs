use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use indicatif::{ProgressBar, ProgressStyle};

use mogen_update::{apply_plan, check, download_and_apply, Progress};

pub(crate) struct UpdateArgs {
    /// Don't prompt — install the latest release if it's newer than the
    /// running binary. Off prints what would happen and exits successfully
    /// without writing anything.
    pub(crate) yes: bool,
    /// Just print the latest release tag and exit. Skips the download.
    pub(crate) check_only: bool,
    /// Re-download and reinstall even if the running binary already matches
    /// the latest release. Useful for repairing a botched install.
    pub(crate) force: bool,
}

pub(crate) fn update(args: UpdateArgs) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("mogen {current}");
    println!("checking for updates from github.com/{}/{}…",
        mogen_update::REPO_OWNER, mogen_update::REPO_NAME);

    let result = check(current).map_err(|e| {
        anyhow!("update check failed: {e:#}")
    })?;
    let info = &result.info;
    println!("latest release: {} ({} bytes)", info.tag, info.asset_size);
    if !info.html_url.is_empty() {
        println!("release page: {}", info.html_url);
    }

    if args.check_only {
        if result.newer {
            println!(
                "an update is available: {current} -> {}. Run `mogen update --yes` to install.",
                info.version
            );
        } else {
            println!("you are up to date.");
        }
        return Ok(());
    }

    if !result.newer && !args.force {
        println!("already running the latest version ({current}). Pass --force to reinstall.");
        return Ok(());
    }

    if !args.yes {
        println!(
            "an update is available: {current} -> {}. Re-run with --yes to install \
             (`mogen update --yes`).",
            info.version
        );
        return Ok(());
    }

    let total = info.asset_size.max(1);
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan.bold} [{elapsed:>3}] {msg:>14} {bar:30.cyan/blue} {bytes}/{total_bytes} ({eta})",
        )
        .expect("static template")
        .progress_chars("=> "),
    );
    pb.enable_steady_tick(Duration::from_millis(120));
    pb.set_message("downloading");
    let started = Instant::now();
    let pb_for_cb = pb.clone();
    let outcome = download_and_apply(info, move |evt| match evt {
        Progress::Stage(label) => {
            pb_for_cb.set_message(label);
        }
        Progress::Download { downloaded, total } => {
            // GitHub sometimes reports a different content-length than the
            // asset record, so cap the bar at its declared length.
            pb_for_cb.set_position(downloaded.min(total.max(1)));
        }
    });
    match outcome {
        Ok(applied) => {
            pb.finish_with_message("installed");
            let elapsed = started.elapsed();
            println!(
                "updated to {} in {:.1}s.",
                applied.tag,
                elapsed.as_secs_f32()
            );
            println!("replaced: {}", applied.replaced_self.display());
            if let Some(s) = applied.replaced_sibling {
                println!("replaced: {}", s.display());
            }
            if applied.elevated {
                println!("(install performed under elevated privileges)");
            }
            println!("restart `mogen` (and `mogen-studio`) to pick up the new version.");
            Ok(())
        }
        Err(e) => {
            pb.abandon_with_message("failed");
            Err(anyhow!("update failed: {e:#}"))
        }
    }
}

/// Hidden `mogen __apply-update --plan <path>` entry point.
///
/// Invoked by [`mogen_update::download_and_apply`] under platform elevation
/// (pkexec / sudo / UAC / osascript) when the install directory isn't
/// writable by the unprivileged process. The caller has already extracted
/// the new binaries to a per-update temp dir and serialised a plan
/// describing the moves the privileged half of the updater should perform.
///
/// Kept out of `--help` because there's no scenario where a user types this
/// by hand — it's strictly the elevated half of the auto-updater RPC.
pub(crate) fn apply_update(plan: PathBuf) -> Result<()> {
    let outcome = apply_plan(&plan)
        .map_err(|e| anyhow!("apply update plan: {e:#}"))?;
    println!("installed {}", outcome.tag);
    for mv in &outcome.moves {
        println!("  -> {}", mv.dst.display());
    }
    Ok(())
}
