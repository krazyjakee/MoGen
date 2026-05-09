use eframe::egui;

use crate::app::types::ExternalChangeKind;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Modal raised by the on-disk watcher when an open MOG file changed
    /// outside MoGen Studio while the buffer had unsaved edits. Offers three
    /// resolutions for `Modified` — Reload from disk (discard local edits),
    /// Keep mine (re-baseline the watcher so we stop prompting), Save over
    /// disk (overwrite). For `Deleted` the choices collapse to: Save (recreate
    /// at the original path), Keep buffer (treat the path as gone — buffer
    /// becomes effectively untitled-with-a-suggested-name), or Close tab.
    pub(in crate::app) fn ui_external_conflict(&mut self, ctx: &egui::Context) {
        let Some(conflict) = self.pending_external.as_ref() else {
            return;
        };
        let i = conflict.file_index;
        if i >= self.files.len() {
            self.pending_external = None;
            return;
        }
        let kind = conflict.kind;
        let name = self.files[i].display_name();
        let path_disp = self
            .files[i]
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        let mut open = true;
        let mut do_reload = false;
        let mut do_keep = false;
        let mut do_overwrite = false;
        let mut do_close = false;
        egui::Window::new(match kind {
            ExternalChangeKind::Modified => "File changed on disk",
            ExternalChangeKind::Deleted => "File deleted on disk",
        })
        .id(egui::Id::new("external_conflict"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            match kind {
                ExternalChangeKind::Modified => {
                    ui.label(format!(
                        "“{name}” was modified outside MoGen Studio while you have \
                         unsaved edits."
                    ));
                }
                ExternalChangeKind::Deleted => {
                    ui.label(format!(
                        "“{name}” no longer exists at its original path."
                    ));
                }
            }
            if !path_disp.is_empty() {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(&path_disp).monospace().weak());
            }
            // Tell the user up front that closing the dialog without picking
            // an option keeps their buffer (i.e. the "Keep mine" / "Keep
            // buffer" branch).
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Closing this dialog without choosing keeps your buffer.",
                )
                .weak(),
            );
            ui.add_space(10.0);
            match kind {
                ExternalChangeKind::Modified => {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Reload from disk")
                            .on_hover_text(
                                "Discard your unsaved edits and load the on-disk version. \
                                 Cannot be undone from the editor's history.",
                            )
                            .clicked()
                        {
                            do_reload = true;
                        }
                        if ui
                            .button("Keep mine")
                            .on_hover_text(
                                "Keep your unsaved buffer as-is. Stops prompting for this \
                                 change; saving later will overwrite the disk version.",
                            )
                            .clicked()
                        {
                            do_keep = true;
                        }
                        if ui
                            .button("Save (overwrite disk)")
                            .on_hover_text("Write your buffer over the disk file now.")
                            .clicked()
                        {
                            do_overwrite = true;
                        }
                    });
                }
                ExternalChangeKind::Deleted => {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Save (recreate file)")
                            .on_hover_text("Write your buffer back to the original path.")
                            .clicked()
                        {
                            do_overwrite = true;
                        }
                        if ui
                            .button("Keep buffer")
                            .on_hover_text(
                                "Keep the in-memory buffer; the original path is treated as \
                                 gone. Save As to give it a new home.",
                            )
                            .clicked()
                        {
                            do_keep = true;
                        }
                        // Spacer so destructive Close tab is visually separated
                        // from the safe options on the left.
                        ui.add_space(20.0);
                        if ui
                            .button("Close tab")
                            .on_hover_text(
                                "Close this tab without saving — destructive, can't be undone.",
                            )
                            .clicked()
                        {
                            do_close = true;
                        }
                    });
                }
            }
        });

        if !open {
            // X button — same as Keep: dismiss without disk side effects but
            // re-baseline so we don't keep firing the modal every tick.
            do_keep = true;
        }

        if do_reload {
            // Replace buffer with the snapshot we read at detection time so
            // the user resolves against exactly what they were prompted on.
            let conflict = self.pending_external.take().expect("checked above");
            if let Some(disk_src) = conflict.disk_source {
                let f = &mut self.files[i];
                f.source = disk_src.clone();
                f.last_saved_source = disk_src;
                f.dirty = false;
                f.disk_mtime = conflict.disk_mtime;
                f.last_edit_at = None;
                f.needs_compile = false;
                f.status = format!("reloaded {name} (discarded unsaved edits)");
            }
            self.compile_file(i);
            if i == self.active {
                self.refresh_viewer_from_active();
            }
            return;
        }

        if do_keep {
            let conflict = self.pending_external.take().expect("checked above");
            let f = &mut self.files[i];
            // For Modified: re-baseline mtime so the next watcher tick treats
            // the *current* on-disk content as known and doesn't immediately
            // re-prompt. The buffer stays dirty against the new on-disk
            // content, which is what the user chose.
            // For Deleted: clear the mtime so a subsequent re-creation of the
            // file fires the watcher again (lets the user resolve the new
            // conflict explicitly instead of silently overwriting).
            f.disk_mtime = match conflict.kind {
                ExternalChangeKind::Modified => conflict.disk_mtime,
                ExternalChangeKind::Deleted => None,
            };
            f.dirty = f.source != f.last_saved_source;
            f.status = match conflict.kind {
                ExternalChangeKind::Modified => format!("kept buffer for {name} (disk diverged)"),
                ExternalChangeKind::Deleted => format!("kept buffer for {name} (file deleted)"),
            };
            return;
        }

        if do_overwrite {
            // Take the conflict before the borrow checker complains about
            // save_index using `self` mutably while we hold a reference.
            let _ = self.pending_external.take();
            self.save_index(i);
            return;
        }

        if do_close {
            let _ = self.pending_external.take();
            self.close_file(i);
        }
    }
}
