use eframe::egui;

use super::MogenStudioApp;

impl MogenStudioApp {
    /// First-launch privacy prompt. Shown once when
    /// `Settings::crash_reports_enabled` is `None` — the user picks Allow or
    /// Decline, the choice is persisted, and the modal never reappears
    /// (Edit → Preferences exposes the toggle if they change their mind).
    /// The prompt is suppressed entirely when `MOGEN_DISABLE_TELEMETRY` /
    /// `DO_NOT_TRACK` are set so opted-out environments stay quiet.
    ///
    /// The chosen value only takes effect on the *next* launch: `crash::init`
    /// runs in `main` before the UI exists, and re-initialising Sentry
    /// mid-session would mean smuggling its guard onto the App for no real
    /// benefit (a session that has already started is unlikely to crash in
    /// the seconds between the prompt and the user noticing).
    pub(super) fn ui_crash_consent(&mut self, ctx: &egui::Context) {
        if !self.show_crash_consent {
            return;
        }

        let mut do_allow = false;
        let mut do_decline = false;
        // Deliberately not `.open(&mut open)` — we want an explicit choice so
        // the saved value always lands on `Some(_)`. An X close would leave
        // it `None` and re-prompt next launch.
        egui::Window::new("Help improve MoGen Studio")
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "MoGen Studio can send anonymous crash reports when something \
                     goes wrong, so we can fix bugs you'd otherwise have to report \
                     manually.",
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("What's included")
                        .strong(),
                );
                ui.label(
                    "  • Error type and stack trace\n  \
                       • App version and build (debug / release)\n  \
                       • OS family",
                );
                ui.add_space(6.0);
                ui.label(egui::RichText::new("What's not").strong());
                ui.label(
                    "  • No source code, .mog files, prompts, or generated assets\n  \
                       • No API keys or filesystem paths beyond what appears in a panic\n  \
                       • No advertising / analytics SDKs — reports go straight to \
                         our self-hosted GlitchTip server",
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "You can change this any time in Edit → Preferences…",
                    )
                    .weak(),
                );

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Allow crash reports")
                        .on_hover_text(
                            "Send anonymous crash reports. Takes effect from the \
                             next launch.",
                        )
                        .clicked()
                    {
                        do_allow = true;
                    }
                    if ui
                        .button("Don't send")
                        .on_hover_text("Disable crash reporting. No data leaves your machine.")
                        .clicked()
                    {
                        do_decline = true;
                    }
                });
            });

        if !do_allow && !do_decline {
            return;
        }

        self.settings.crash_reports_enabled = Some(do_allow);
        if let Err(e) = self.settings.save() {
            self.active_mut().status = format!("crash consent: save failed: {e}");
        } else {
            self.active_mut().status = if do_allow {
                "crash reports enabled — thanks! takes effect next launch".into()
            } else {
                "crash reports disabled".into()
            };
        }

        self.show_crash_consent = false;
    }
}
