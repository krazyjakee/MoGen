use eframe::egui;

use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Confirmation shown when a window-close is requested while any buffer
    /// is dirty. Lists the unsaved files and offers Save All / Discard /
    /// Cancel. Save All walks every dirty buffer and invokes `save_index`,
    /// which opens a Save As dialog for untitled tabs — if the user cancels
    /// any of those, the modal stays open so no work is silently lost.
    pub(in crate::app) fn ui_quit_confirm(&mut self, ctx: &egui::Context) {
        if !self.show_quit_confirm {
            return;
        }
        let mut open = true;
        let mut do_save_all = false;
        let mut do_discard = false;
        let mut do_cancel = false;
        let dirty_names: Vec<String> = self
            .files
            .iter()
            .filter(|f| f.dirty)
            .map(|f| f.display_name())
            .collect();
        egui::Window::new("Unsaved changes")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} unsaved file{} will be lost if you quit now:",
                    dirty_names.len(),
                    if dirty_names.len() == 1 { "" } else { "s" },
                ));
                ui.add_space(4.0);
                for name in &dirty_names {
                    ui.label(format!("  • {name}"));
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Save All")
                        .on_hover_text(
                            "Save each unsaved MOG file, then quit. \
                             Untitled MOG files open a Save As dialog.",
                        )
                        .clicked()
                    {
                        do_save_all = true;
                    }
                    if ui
                        .button("Discard")
                        .on_hover_text("Quit without saving — unsaved edits are lost")
                        .clicked()
                    {
                        do_discard = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                });
            });

        if !open || do_cancel {
            self.show_quit_confirm = false;
            return;
        }

        if do_save_all {
            let mut all_clean = true;
            for i in 0..self.files.len() {
                if self.files[i].dirty && !self.save_index(i) {
                    all_clean = false;
                    break;
                }
            }
            if all_clean {
                self.show_quit_confirm = false;
                self.confirmed_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            // Otherwise leave the modal open so the user can retry or cancel.
            return;
        }

        if do_discard {
            self.show_quit_confirm = false;
            self.confirmed_quit = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Confirmation shown when the user tries to close a single dirty tab.
    /// Mirrors `ui_quit_confirm` but scoped to one buffer — Save invokes
    /// `save_index` (which may open a Save As dialog for untitled tabs; the
    /// modal stays open if the dialog is cancelled), Discard closes without
    /// saving, Cancel dismisses the modal.
    pub(in crate::app) fn ui_close_confirm(&mut self, ctx: &egui::Context) {
        let Some(i) = self.pending_close_index else {
            return;
        };
        if i >= self.files.len() {
            self.pending_close_index = None;
            return;
        }
        let name = self.files[i].display_name();
        let mut open = true;
        let mut do_save = false;
        let mut do_discard = false;
        let mut do_cancel = false;
        egui::Window::new("Unsaved changes")
            .id(egui::Id::new("close_tab_confirm"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "“{name}” has unsaved changes. Close this tab anyway?"
                ));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Save")
                        .on_hover_text(
                            "Save this MOG file, then close the tab. \
                             Untitled MOG files open a Save As dialog.",
                        )
                        .clicked()
                    {
                        do_save = true;
                    }
                    if ui
                        .button("Discard")
                        .on_hover_text("Close the tab without saving — unsaved edits are lost")
                        .clicked()
                    {
                        do_discard = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                });
            });

        if !open || do_cancel {
            self.pending_close_index = None;
            return;
        }

        if do_save {
            if self.save_index(i) {
                self.pending_close_index = None;
                self.close_file(i);
            }
            // Save As cancelled — leave the modal open so the user can retry.
            return;
        }

        if do_discard {
            self.pending_close_index = None;
            self.close_file(i);
        }
    }
}
