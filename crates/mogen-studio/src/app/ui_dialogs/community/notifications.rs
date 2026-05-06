//! Notifications bell + dropdown. Pops `module_updated` events; "Mark
//! all read" calls the server. Lives on the status-bar chip and inside
//! the Community window's auth strip.

use crate::app::moghub::{fetch_notifications, mark_notifications_read};
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Notifications bell — shows an unread badge when the inbox has
    /// any unread items. Click pops a dropdown with the most recent
    /// `module_updated` events; "Mark all read" calls the server.
    pub(super) fn draw_notif_bell(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let unread = self
            .community
            .notifications
            .as_ref()
            .map(|n| n.unread)
            .unwrap_or(0);
        let label = if unread > 0 {
            format!("🔔 {unread}")
        } else {
            "🔔".to_string()
        };
        ui.menu_button(label, |ui| {
            self.draw_notif_dropdown(ui, ctx);
        });
    }

    fn draw_notif_dropdown(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let pending = self.community.pending_notifications.is_some()
            || self.community.pending_notifications_read.is_some();
        let unread_count = self
            .community
            .notifications
            .as_ref()
            .map(|n| n.unread)
            .unwrap_or(0);
        ui.horizontal(|ui| {
            ui.heading("Notifications");
            ui.weak(format!("· {unread_count} unread"));
        });
        ui.separator();
        if pending {
            ui.weak("loading…");
            return;
        }
        let items = self
            .community
            .notifications
            .as_ref()
            .map(|n| n.items.clone())
            .unwrap_or_default();
        if items.is_empty() {
            ui.weak("Nothing new.");
        } else {
            // Cap how many we show in the dropdown so the menu stays
            // bounded; everything stays available via the upcoming
            // /notifications page on the web side.
            for n in items.iter().take(20) {
                ui.horizontal(|ui| {
                    let dot = if !n.read { "● " } else { "  " };
                    let summary = match (&n.source_model, n.source_version) {
                        (Some(s), Some(v)) => format!(
                            "{dot}{} pushed @{}/{} v{}",
                            n.kind, s.user.handle, s.slug, v
                        ),
                        (Some(s), None) => {
                            format!("{dot}{} updated @{}/{}", n.kind, s.user.handle, s.slug)
                        }
                        _ => format!("{dot}{}", n.kind),
                    };
                    if let Some(s) = &n.source_model {
                        if ui.link(summary).clicked() {
                            self.community.open = true;
                            self.kick_detail(ctx, s.user.handle.clone(), s.slug.clone());
                            ui.close_menu();
                        }
                    } else {
                        ui.label(summary);
                    }
                });
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.kick_notifications(ctx);
            }
            if unread_count > 0
                && ui
                    .add_enabled(!pending, egui::Button::new("Mark all read"))
                    .clicked()
            {
                self.kick_mark_notifications_read(ctx);
            }
        });
    }

    pub(super) fn kick_notifications(&mut self, ctx: &egui::Context) {
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        if token.is_empty() {
            return;
        }
        self.community.pending_notifications =
            Some(fetch_notifications(url, token, ctx.clone()));
    }

    fn kick_mark_notifications_read(&mut self, ctx: &egui::Context) {
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        if token.is_empty() {
            return;
        }
        self.community.pending_notifications_read =
            Some(mark_notifications_read(url, token, ctx.clone()));
    }
}
