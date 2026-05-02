//! Auto-update worker plumbing for MoGen Studio.
//!
//! Two background phases live behind one `Help → Check for Updates…` modal:
//!
//! 1. **Check.** Hit the GitHub Releases API and report whether a newer tag
//!    is available. Cheap (one HTTPS call); always runs on a worker thread
//!    so the UI never stalls.
//! 2. **Install.** Stream the matching archive to disk, extract it, and swap
//!    the running `mogen-studio` (and sibling `mogen` CLI) in place. Reports
//!    download / install progress through an `mpsc` so the dialog can show
//!    a live status line and bar.
//!
//! Both stages mirror the channel-based pattern used by `build.rs`: the worker
//! sends events on a sender and the UI thread polls a receiver each frame in
//! `MogenStudioApp::poll_update`. The state machine is intentionally tiny —
//! one optional state struct on the app, transitioned by user clicks.

use std::sync::mpsc::{channel, Receiver};
use std::thread;

use eframe::egui;

use mogen_update::{check, download_and_apply, CheckResult, Progress, UpdateInfo};

use super::MogenStudioApp;

/// Where the update dialog is in its lifecycle. Drives both the modal's
/// content and which buttons are enabled.
pub(super) enum UpdateState {
    /// Initial render before the user clicks "Check". Holds nothing — kept as
    /// its own variant so the dialog can show an explanatory blurb before the
    /// first network call instead of an empty spinner.
    Idle,
    /// Background check running. The receiver carries the eventual result.
    Checking { rx: Receiver<CheckResultMsg> },
    /// Check returned successfully; we know the latest version. The user
    /// decides whether to download.
    Ready(CheckResult),
    /// Check failed. Carries a human-readable error message; user can retry.
    CheckFailed(String),
    /// Download / install in flight. Status text and bar fed by `rx`.
    Installing {
        info: UpdateInfo,
        rx: Receiver<InstallMsg>,
        stage: String,
        downloaded: u64,
        total: u64,
    },
    /// Install finished successfully — user just needs to restart.
    Installed { tag: String },
    /// Install failed mid-stream. Carries the error message; user can retry
    /// (which falls back to a fresh check).
    InstallFailed(String),
}

/// Messages from the check worker. Single-shot — sent once and the channel
/// hangs up.
pub(super) enum CheckResultMsg {
    Ok(CheckResult),
    Err(String),
}

/// Streamed messages from the install worker. The worker emits zero or more
/// `Progress` followed by exactly one `Done`.
pub(super) enum InstallMsg {
    Progress(Progress),
    Done(Result<String, String>),
}

impl MogenStudioApp {
    /// Open the update dialog. If a check or install is already in flight,
    /// reuses that state (so re-clicking the menu item just brings the modal
    /// back rather than starting fresh).
    pub(super) fn open_update_dialog(&mut self) {
        self.show_update = true;
        if self.update_state.is_none() {
            self.update_state = Some(UpdateState::Idle);
        }
    }

    /// Spawn the GitHub release lookup on a worker. Cheap call — one HTTPS
    /// request — but threaded so the UI never blocks on DNS or a slow
    /// connection.
    pub(super) fn spawn_update_check(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        self.update_state = Some(UpdateState::Checking { rx });
        let ctx = ctx.clone();
        let current = env!("CARGO_PKG_VERSION").to_string();
        thread::spawn(move || {
            let msg = match check(&current) {
                Ok(r) => CheckResultMsg::Ok(r),
                Err(e) => CheckResultMsg::Err(format!("{e:#}")),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    /// Spawn the download + install worker for the given release. The
    /// `Progress` events are forwarded onto an `mpsc` so the UI thread can
    /// paint them without holding any locks.
    pub(super) fn spawn_update_install(&mut self, ctx: &egui::Context, info: UpdateInfo) {
        let (tx, rx) = channel();
        let total = info.asset_size;
        self.update_state = Some(UpdateState::Installing {
            info: info.clone(),
            rx,
            stage: "starting".into(),
            downloaded: 0,
            total,
        });
        let ctx_for_thread = ctx.clone();
        thread::spawn(move || {
            let tx_progress = tx.clone();
            let ctx_for_progress = ctx_for_thread.clone();
            let outcome = download_and_apply(&info, move |evt| {
                let _ = tx_progress.send(InstallMsg::Progress(evt));
                ctx_for_progress.request_repaint();
            });
            let done = match outcome {
                Ok(applied) => InstallMsg::Done(Ok(applied.tag)),
                Err(e) => InstallMsg::Done(Err(format!("{e:#}"))),
            };
            let _ = tx.send(done);
            ctx_for_thread.request_repaint();
        });
    }

    /// True when a check or install worker is producing events. Used by the
    /// repaint heartbeat so the progress bar keeps moving without the user
    /// having to nudge the window.
    pub(super) fn update_in_flight(&self) -> bool {
        matches!(
            self.update_state,
            Some(UpdateState::Checking { .. }) | Some(UpdateState::Installing { .. })
        )
    }

    /// Drain whatever the in-flight worker has produced this frame and
    /// transition the state machine. Idempotent — a no-op when nothing is in
    /// flight.
    pub(super) fn poll_update(&mut self) {
        // Step 1: extract whatever update we want to apply, then mutate the
        // field. This dance avoids holding a borrow on `self.update_state`
        // while we reassign it.
        let next = match self.update_state.as_mut() {
            Some(UpdateState::Checking { rx }) => match rx.try_recv() {
                Ok(CheckResultMsg::Ok(res)) => Some(UpdateState::Ready(res)),
                Ok(CheckResultMsg::Err(e)) => Some(UpdateState::CheckFailed(e)),
                Err(_) => None,
            },
            Some(UpdateState::Installing {
                rx,
                stage,
                downloaded,
                total,
                info: _,
            }) => {
                // Drain every queued progress message in one frame so the bar
                // catches up instead of moving one tick per repaint.
                let mut done_with: Option<Result<String, String>> = None;
                loop {
                    match rx.try_recv() {
                        Ok(InstallMsg::Progress(Progress::Stage(s))) => {
                            *stage = s;
                        }
                        Ok(InstallMsg::Progress(Progress::Download {
                            downloaded: d,
                            total: t,
                        })) => {
                            *downloaded = d;
                            if t > 0 {
                                *total = t;
                            }
                        }
                        Ok(InstallMsg::Done(r)) => {
                            done_with = Some(r);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                done_with.map(|r| match r {
                    Ok(tag) => UpdateState::Installed { tag },
                    Err(e) => UpdateState::InstallFailed(e),
                })
            }
            _ => None,
        };
        if let Some(s) = next {
            self.update_state = Some(s);
        }
    }
}
