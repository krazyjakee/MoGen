use eframe::egui;

use crate::app::update::UpdateState;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Help → Check for Updates… modal. State machine lives on the app
    /// (`MogenStudioApp::update_state`) and is advanced each frame by
    /// `poll_update`. This function is purely presentational — every
    /// non-trivial action is deferred via small intent flags so the borrow on
    /// `self.update_state` ends before any mutation runs.
    pub(in crate::app) fn ui_update_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_update {
            return;
        }
        let mut open = true;
        let mut start_check = false;
        let mut start_install: Option<mogen_update::UpdateInfo> = None;
        let mut close = false;

        egui::Window::new("Check for Updates")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "Current version: {}",
                    env!("CARGO_PKG_VERSION")
                ));
                ui.label(format!(
                    "Source: github.com/{}/{}",
                    mogen_update::REPO_OWNER,
                    mogen_update::REPO_NAME,
                ));
                ui.add_space(10.0);

                // Snapshot the variant so the closures below don't need to
                // hold a mutable borrow into `self.update_state`.
                let state = self
                    .update_state
                    .as_ref()
                    .map(state_kind)
                    .unwrap_or(StateKind::Idle);

                match state {
                    StateKind::Idle => {
                        ui.label(
                            "Click \"Check now\" to look for a newer release on \
                             GitHub. Nothing is downloaded until you confirm.",
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Check now").clicked() {
                                start_check = true;
                            }
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        });
                    }
                    StateKind::Checking => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("checking github releases…");
                        });
                        ui.add_space(8.0);
                        if ui.button("Cancel").clicked() {
                            // Drop the receiver and reset to Idle. The worker
                            // thread keeps running but its result is discarded.
                            close = true;
                        }
                    }
                    StateKind::Ready => {
                        if let Some(UpdateState::Ready(res)) = self.update_state.as_ref() {
                            let info = &res.info;
                            ui.label(format!("Latest release: {}", info.tag));
                            ui.label(format!(
                                "Asset: {} ({})",
                                info.asset_name,
                                format_bytes(info.asset_size),
                            ));
                            if !info.html_url.is_empty() {
                                ui.hyperlink_to("Release notes on GitHub", &info.html_url);
                            }
                            ui.add_space(8.0);
                            if res.newer {
                                ui.colored_label(
                                    egui::Color32::from_rgb(120, 200, 140),
                                    format!(
                                        "An update is available: {} → {}.",
                                        env!("CARGO_PKG_VERSION"),
                                        info.version,
                                    ),
                                );
                                ui.add_space(6.0);
                                if !info.body.trim().is_empty() {
                                    egui::CollapsingHeader::new("What's new")
                                        .default_open(false)
                                        .show(ui, |ui| {
                                            egui::ScrollArea::vertical()
                                                .max_height(160.0)
                                                .show(ui, |ui| {
                                                    ui.label(&info.body);
                                                });
                                        });
                                    ui.add_space(6.0);
                                }
                                ui.label(
                                    "Installing replaces the running mogen-studio (and the \
                                     bundled mogen CLI if present alongside it). You'll need \
                                     to restart the app once the swap completes.",
                                );
                                ui.add_space(8.0);
                                let mut want_skip: Option<String> = None;
                                ui.horizontal(|ui| {
                                    if ui
                                        .button("Download and install")
                                        .on_hover_text(
                                            "Download the matching release archive for your \
                                             platform and replace the running binaries in place.",
                                        )
                                        .clicked()
                                    {
                                        start_install = Some(info.clone());
                                    }
                                    if ui
                                        .button("Skip this version")
                                        .on_hover_text(
                                            "Don't prompt me about this release again. \
                                             Newer releases will still surface.",
                                        )
                                        .clicked()
                                    {
                                        want_skip = Some(info.tag.clone());
                                        close = true;
                                    }
                                    if ui.button("Close").clicked() {
                                        close = true;
                                    }
                                });
                                if let Some(tag) = want_skip {
                                    self.settings.skipped_update_tag = tag;
                                    let _ = self.settings.save();
                                }
                            } else {
                                ui.colored_label(
                                    egui::Color32::from_rgb(180, 200, 220),
                                    "You're already running the latest release.",
                                );
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    if ui
                                        .button("Reinstall")
                                        .on_hover_text(
                                            "Re-download and reinstall the current release. \
                                             Useful for repairing a corrupted install.",
                                        )
                                        .clicked()
                                    {
                                        start_install = Some(info.clone());
                                    }
                                    if ui.button("Close").clicked() {
                                        close = true;
                                    }
                                });
                            }
                        }
                    }
                    StateKind::CheckFailed => {
                        if let Some(UpdateState::CheckFailed(e)) = self.update_state.as_ref() {
                            ui.colored_label(
                                egui::Color32::from_rgb(220, 120, 120),
                                "Update check failed.",
                            );
                            ui.add_space(4.0);
                            ui.label(e);
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                if ui.button("Try again").clicked() {
                                    start_check = true;
                                }
                                if ui.button("Close").clicked() {
                                    close = true;
                                }
                            });
                        }
                    }
                    StateKind::Installing => {
                        if let Some(UpdateState::Installing {
                            stage,
                            downloaded,
                            total,
                            info,
                            ..
                        }) = self.update_state.as_ref()
                        {
                            ui.label(format!("Installing {}…", info.tag));
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.spinner();
                                let label = if stage.is_empty() {
                                    "working".to_string()
                                } else {
                                    format!("{stage}…")
                                };
                                ui.label(label);
                            });
                            ui.add_space(6.0);
                            let denom = (*total).max(1);
                            let frac = (*downloaded as f32 / denom as f32).clamp(0.0, 1.0);
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .show_percentage()
                                    .desired_width(360.0),
                            );
                            ui.add_space(2.0);
                            ui.label(format!(
                                "{} / {}",
                                format_bytes(*downloaded),
                                format_bytes(*total),
                            ));
                            ui.add_space(8.0);
                            ui.label(
                                "The new binary is being placed on disk. Don't quit the app \
                                 until this completes.",
                            );
                        }
                    }
                    StateKind::Installed => {
                        if let Some(UpdateState::Installed { tag }) = self.update_state.as_ref() {
                            ui.colored_label(
                                egui::Color32::from_rgb(120, 200, 140),
                                format!("Updated to {tag}."),
                            );
                            ui.add_space(6.0);
                            ui.label(
                                "Quit and relaunch MoGen Studio to start running the new \
                                 version.",
                            );
                            ui.add_space(8.0);
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        }
                    }
                    StateKind::InstallFailed => {
                        if let Some(UpdateState::InstallFailed(e)) = self.update_state.as_ref() {
                            ui.colored_label(
                                egui::Color32::from_rgb(220, 120, 120),
                                "Install failed.",
                            );
                            ui.add_space(4.0);
                            ui.label(e);
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                if ui.button("Check again").clicked() {
                                    start_check = true;
                                }
                                if ui.button("Close").clicked() {
                                    close = true;
                                }
                            });
                        }
                    }
                }
            });

        if !open || close {
            self.show_update = false;
            // Drop any in-flight check. An installing worker is left running:
            // killing it mid-write would leave the binary half-replaced, and
            // there is no safe way to abort the file swap once it has started.
            match self.update_state.as_ref() {
                Some(UpdateState::Installing { .. }) => {
                    // Keep the state so reopening the dialog reattaches.
                }
                _ => {
                    self.update_state = None;
                }
            }
            return;
        }
        if start_check {
            self.spawn_update_check(ctx);
        }
        if let Some(info) = start_install {
            self.spawn_update_install(ctx, info);
        }
    }
}

#[derive(Clone, Copy)]
enum StateKind {
    Idle,
    Checking,
    Ready,
    CheckFailed,
    Installing,
    Installed,
    InstallFailed,
}

fn state_kind(s: &UpdateState) -> StateKind {
    match s {
        UpdateState::Idle => StateKind::Idle,
        UpdateState::Checking { .. } => StateKind::Checking,
        UpdateState::Ready(_) => StateKind::Ready,
        UpdateState::CheckFailed(_) => StateKind::CheckFailed,
        UpdateState::Installing { .. } => StateKind::Installing,
        UpdateState::Installed { .. } => StateKind::Installed,
        UpdateState::InstallFailed(_) => StateKind::InstallFailed,
    }
}

fn format_bytes(n: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let f = n as f64;
    if f >= GIB {
        format!("{:.2} GiB", f / GIB)
    } else if f >= MIB {
        format!("{:.2} MiB", f / MIB)
    } else if f >= KIB {
        format!("{:.1} KiB", f / KIB)
    } else {
        format!("{n} B")
    }
}
