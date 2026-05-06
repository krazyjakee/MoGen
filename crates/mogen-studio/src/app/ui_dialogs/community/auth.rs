//! Auth strip + sign-in / whoami / sign-out kicks. Owns the
//! `pending_signin` / `pending_whoami` workers.

use crate::app::moghub::{fetch_whoami, start_signin};
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Render the auth chip at the top of the Community window.
    /// Signed-out: a "Sign in with GitHub" button kicks the loopback
    /// OAuth flow. Signed-in: shows `@handle` with a Sign out menu.
    pub(super) fn draw_auth_strip(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if let Some(user) = self.community.me.clone() {
                if let Some(url) = &user.avatar_url {
                    if let Some(tex) = self.community_image(ctx, url) {
                        ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(20.0, 20.0)));
                    }
                }
                ui.label(format!("Signed in as @{}", user.handle));
                if ui.small_button("Sign out").clicked() {
                    self.sign_out_moghub();
                }
            } else if self.community.pending_signin.is_some() {
                ui.label("Sign-in: complete the flow in your browser…");
            } else if self.community.pending_whoami.is_some() {
                ui.label("Checking session…");
            } else if ui.button("Sign in with GitHub").clicked() {
                self.kick_signin(ctx);
            }
        });
        if let Some(err) = &self.community.auth_error {
            ui.colored_label(egui::Color32::LIGHT_RED, err);
        }
    }

    /// Spawn the loopback OAuth flow. Browser opens to the desktop
    /// start endpoint; the worker holds the listener until GitHub
    /// redirects back or [`OAUTH_TIMEOUT`] elapses.
    pub(in crate::app) fn kick_signin(&mut self, ctx: &egui::Context) {
        let url = self.settings.moghub_url.clone();
        self.community.pending_signin = Some(start_signin(url, ctx.clone()));
        self.community.auth_error = None;
    }

    /// Refresh `whoami`. Called after a successful sign-in (to load the
    /// chip's `@handle`) and on app start when a persisted token is
    /// present (to validate it before showing a stale chip).
    pub(in crate::app) fn kick_whoami(&mut self, ctx: &egui::Context) {
        if self.settings.moghub_session.is_empty() {
            self.community.me = None;
            return;
        }
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_whoami = Some(fetch_whoami(url, token, ctx.clone()));
    }

    /// Clear the persisted token + cached `me`. Best-effort save —
    /// failure to persist leaves the in-memory state signed-out, which
    /// is the conservative choice (next launch will reload from disk
    /// and re-sign-in if the file write actually failed).
    pub(in crate::app) fn sign_out_moghub(&mut self) {
        let _ = self.settings.clear_moghub_session();
        self.community.me = None;
        self.community.auth_error = None;
    }
}
