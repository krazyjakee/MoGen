//! Community window — browse + open published `.mog` files from MoGHub.
//!
//! Read-only browse: Discover feed with kind filters + search + paged
//! results, model detail with thumbnails and tag chips, "Open in editor"
//! that pulls every file body inline (no second round-trip — the detail
//! response carries them) into untitled tabs.
//!
//! All HTTP I/O runs on `std::thread` workers in `app/moghub.rs`; this
//! module only renders the UI and dispatches calls. Image bytes also
//! arrive via the same worker channel and are uploaded to the GPU on
//! the UI thread.
//!
//! State preservation across close/reopen: `me` (the cached signed-in
//! user) survives so the chip doesn't flicker; the rest of the dialog
//! resets so the next open re-fetches the front page.
//!
//! ## Submodule layout
//!
//! - `state` — `CommunityState` and friends (publish, palette, update target)
//! - `util` — pure helpers (URL parsing, image decode, bbcode strip,
//!   `MoghubError` formatting)
//! - `auth` — sign-in / whoami / sign-out + the in-window auth strip
//! - `notifications` — bell + dropdown shared with the status chip
//! - `discover` — feed, search, kind pills, summary rows, image cache
//! - `detail` — model page, like button, deps section, comments, "Open
//!   in editor"
//! - `module_palette` — Cmd+Shift+M registry palette
//! - `publish` — publish dialog + thumbnail capture round-trip
//! - `workers` — single drain pass over every Community worker channel

mod auth;
mod detail;
mod discover;
mod module_palette;
mod notifications;
mod publish;
mod state;
mod util;
mod workers;

pub(crate) use state::CommunityState;

use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Paint the Community window. No-op when `community.open` is false.
    pub(in crate::app) fn community_window(&mut self, ctx: &egui::Context) {
        if !self.community.open {
            return;
        }
        // Lazy initial fetch — first frame after the window opens with no
        // discover state cached. Also gate on `error` so a failed initial
        // load doesn't re-fire on every repaint (the worker clears
        // `pending_discover` on completion, so without this gate the next
        // frame would see the empty state and kick again, hammering
        // MoGHub as fast as connections fail).
        if !self.community.loaded_once
            && self.community.pending_discover.is_none()
            && self.community.error.is_none()
        {
            self.kick_discover(ctx, false);
        }
        // Lazy whoami — only when a persisted token exists and we
        // haven't yet validated it this session. `me` survives close /
        // reopen so a re-open doesn't refire the call.
        if self.community.me.is_none()
            && !self.settings.moghub_session.is_empty()
            && self.community.pending_whoami.is_none()
            && self.community.pending_signin.is_none()
            && self.community.auth_error.is_none()
        {
            self.kick_whoami(ctx);
        }
        // Drain worker channels before painting so the UI reflects
        // any results that arrived during this frame.
        self.poll_community_workers(ctx);

        let mut keep_open = true;
        egui::Window::new("Community")
            .open(&mut keep_open)
            .default_width(640.0)
            .default_height(720.0)
            .resizable(true)
            .show(ctx, |ui| {
                self.draw_auth_strip(ui, ctx);
                ui.separator();
                match self.community.view {
                    state::View::Discover => self.draw_discover(ui, ctx),
                    state::View::Detail => self.draw_detail(ui, ctx),
                }
            });
        if !keep_open {
            // Preserve auth state across close/reopen — the persisted
            // token is the source of truth, and `me` is just the cached
            // whoami response. Leave the dialog state otherwise empty so
            // the next open re-fetches discover.
            let me = self.community.me.take();
            self.community = CommunityState {
                me,
                ..CommunityState::default()
            };
        }
    }

    /// Render a compact MoGHub auth chip — meant for places like the
    /// main-window status bar where vertical space is precious. Signed
    /// in: avatar + `@handle` + notification bell with unread badge,
    /// chip menu has "Open Community" + "Sign out". Signed out: the
    /// chip draws nothing; sign-in lives in the Community window.
    ///
    /// Cooperates with the same workers as the in-window auth strip:
    /// `kick_whoami` runs lazily on first paint when a persisted token
    /// exists.
    pub(in crate::app) fn draw_moghub_status_chip(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
    ) {
        // Drain worker channels here too — `community_window` only
        // polls when it's open, so without this the chip's
        // `pending_whoami`/`pending_notifications` stay set forever
        // when the user never opens the Community window.
        self.poll_community_workers(ctx);
        // Lazy whoami the first time the chip is shown with a stored
        // token but no cached `me`. Mirrors the gate in
        // `community_window` so the status bar bootstraps even if the
        // Community window is never opened.
        if self.community.me.is_none()
            && !self.settings.moghub_session.is_empty()
            && self.community.pending_whoami.is_none()
            && self.community.pending_signin.is_none()
            && self.community.auth_error.is_none()
        {
            self.kick_whoami(ctx);
        }
        // Lazy first poll of the notifications inbox once we know who
        // we are. Subsequent polls fire on bell click + on app focus.
        if self.community.me.is_some()
            && self.community.notifications.is_none()
            && self.community.pending_notifications.is_none()
        {
            self.kick_notifications(ctx);
        }

        if let Some(user) = self.community.me.clone() {
            let avatar = user
                .avatar_url
                .as_ref()
                .and_then(|u| self.community_image(ctx, u));
            self.draw_notif_bell(ui, ctx);
            ui.menu_button(format!("@{}", user.handle), |ui| {
                if ui.button("Open Community").clicked() {
                    self.community.open = true;
                    ui.close_menu();
                }
                if ui.button("Refresh notifications").clicked() {
                    self.kick_notifications(ctx);
                    ui.close_menu();
                }
                if ui.button("Sign out").clicked() {
                    self.sign_out_moghub();
                    ui.close_menu();
                }
            });
            if let Some(tex) = avatar {
                ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(18.0, 18.0)));
            }
        } else if self.community.pending_signin.is_some() {
            ui.weak("MoGHub: complete sign-in in browser…");
        } else if self.community.pending_whoami.is_some() {
            ui.weak("MoGHub: checking…");
        }
    }
}
