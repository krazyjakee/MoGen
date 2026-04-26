use std::fs;
use std::time::Instant;

use eframe::egui;

use super::types::{ExternalChangeKind, ExternalConflict, WATCH_INTERVAL};
use super::MogenStudioApp;

impl MogenStudioApp {
    /// Walk every titled, non-LLM-busy buffer and check whether the on-disk
    /// file diverged from what we last loaded or saved. Clean buffers reload
    /// silently; dirty buffers raise a single conflict modal (the next is
    /// picked up on the following tick).
    ///
    /// Skipped while:
    ///   - the active file has an LLM call in flight (the buffer is in a
    ///     transitional state — we don't want to prompt mid-stream and risk
    ///     the user resolving against a buffer that's about to be replaced)
    ///   - a conflict modal is already open (resolve that one first)
    ///   - any other modal is open (avoid stacked dialogs)
    pub(super) fn check_external_changes(&mut self, ctx: &egui::Context) {
        if self.pending_external.is_some() {
            return;
        }
        if self.show_quit_confirm
            || self.pending_close_index.is_some()
            || self.show_export
            || self.show_options
            || self.show_new_prompt
            || self.show_about
            || self.show_onboarding
        {
            return;
        }

        let now = Instant::now();
        let mut auto_reloaded: Option<(usize, String)> = None;
        let mut new_conflict: Option<ExternalConflict> = None;

        for i in 0..self.files.len() {
            let Some(path) = self.files[i].path.clone() else {
                continue;
            };
            if self.files[i].llm_in_flight.is_some() {
                continue;
            }
            // The watcher needs a baseline mtime to compare against. Untitled-
            // turned-saved tabs always have one; explicitly skip when missing
            // so we don't treat platform mtime quirks as external edits.
            let Some(known_mtime) = self.files[i].disk_mtime else {
                self.files[i].last_watch_check = Some(now);
                continue;
            };
            if let Some(t) = self.files[i].last_watch_check {
                if now.saturating_duration_since(t) < WATCH_INTERVAL {
                    continue;
                }
            }
            self.files[i].last_watch_check = Some(now);

            // Two outcomes worth handling: the file is gone (Deleted), or it
            // exists but its mtime moved. A read failure that isn't NotFound
            // is treated as a transient I/O error — leave state as-is and try
            // again next tick.
            match fs::metadata(&path) {
                Ok(meta) => {
                    let on_disk = meta.modified().ok();
                    if on_disk == Some(known_mtime) {
                        continue;
                    }
                    let Ok(disk_src) = fs::read_to_string(&path) else {
                        continue;
                    };

                    // External rewrite that happens to match our buffer
                    // exactly (e.g. user reverted in another editor to the
                    // version we were holding) — silently re-baseline so we
                    // don't keep prompting.
                    if disk_src == self.files[i].source {
                        let f = &mut self.files[i];
                        f.last_saved_source = disk_src;
                        f.disk_mtime = on_disk;
                        f.dirty = false;
                        continue;
                    }

                    if !self.files[i].dirty {
                        // Clean buffer: silently reload, recompile, refresh viewer.
                        let f = &mut self.files[i];
                        f.source = disk_src.clone();
                        f.last_saved_source = disk_src;
                        f.disk_mtime = on_disk;
                        f.dirty = false;
                        f.needs_compile = false;
                        f.last_edit_at = None;
                        let name = f.display_name();
                        auto_reloaded = Some((i, name));
                        // Only one auto-reload per tick is enough — keeps the
                        // status footer's last message coherent. The next file
                        // will be picked up on the next watch interval.
                        break;
                    }

                    if new_conflict.is_none() {
                        new_conflict = Some(ExternalConflict {
                            file_index: i,
                            kind: ExternalChangeKind::Modified,
                            disk_source: Some(disk_src),
                            disk_mtime: on_disk,
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if new_conflict.is_none() {
                        new_conflict = Some(ExternalConflict {
                            file_index: i,
                            kind: ExternalChangeKind::Deleted,
                            disk_source: None,
                            disk_mtime: None,
                        });
                    }
                }
                Err(_) => {
                    // Permission flips, network FS hiccups, etc. — try again
                    // next tick rather than declaring a conflict on a transient.
                }
            }
        }

        if let Some((i, name)) = auto_reloaded {
            self.compile_file(i);
            if i == self.active {
                self.refresh_viewer_from_active();
            }
            self.files[i].status = format!("reloaded {name} (changed on disk)");
            ctx.request_repaint();
        }

        if let Some(c) = new_conflict {
            self.pending_external = Some(c);
            ctx.request_repaint();
        }
    }
}
