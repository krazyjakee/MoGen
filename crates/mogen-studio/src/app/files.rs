use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// If the user saved via the Save dialog without typing any extension, append
/// `.mog`. rfd's `add_filter` doesn't enforce an extension on every platform,
/// so extensionless paths slip through. A path with *any* extension is left
/// alone — even a non-`.mog` one — since that reflects an explicit user choice.
fn ensure_mog_extension(path: &Path) -> PathBuf {
    if path.extension().is_some() {
        path.to_path_buf()
    } else {
        path.with_extension("mog")
    }
}

use eframe::egui;

use super::types::FileState;
use super::MogenStudioApp;

impl MogenStudioApp {
    pub(super) fn active(&self) -> &FileState {
        &self.files[self.active]
    }

    pub(super) fn active_mut(&mut self) -> &mut FileState {
        &mut self.files[self.active]
    }

    pub(super) fn file_index_by_path(&self, path: &Path) -> Option<usize> {
        self.files
            .iter()
            .position(|f| f.path.as_deref() == Some(path))
    }

    pub(super) fn open_path(&mut self, path: &Path) {
        if let Some(i) = self.file_index_by_path(path) {
            self.activate(i);
            return;
        }
        match fs::read_to_string(path) {
            Ok(src) => {
                let f = FileState::loaded(path.to_path_buf(), src);
                if self.files.len() == 1 && self.files[0].is_pristine_untitled() {
                    self.files[0] = f;
                    self.activate(0);
                } else {
                    self.files.push(f);
                    let i = self.files.len() - 1;
                    self.activate(i);
                }
                self.remember_last_opened();
                self.persist_open_tabs();
            }
            Err(e) => {
                self.active_mut().status = format!("open failed: {e}");
            }
        }
    }

    /// Snapshot the current tab strip into `settings.open_tabs` (titled files
    /// only, in order) and the active tab's path into `last_opened`, then save
    /// if either changed. Saves are best-effort — a failed write is ignored
    /// since this is convenience persistence, not load-bearing state.
    pub(super) fn persist_open_tabs(&mut self) {
        let paths: Vec<String> = self
            .files
            .iter()
            .filter_map(|f| f.path.as_ref().map(|p| p.display().to_string()))
            .collect();
        let active_path = self
            .files
            .get(self.active)
            .and_then(|f| f.path.as_ref())
            .map(|p| p.display().to_string());
        let tabs_changed = self.settings.open_tabs != paths;
        let active_changed = active_path.is_some() && self.settings.last_opened != active_path;
        self.settings.open_tabs = paths;
        if active_path.is_some() {
            self.settings.last_opened = active_path;
        }
        if tabs_changed || active_changed {
            let _ = self.settings.save();
        }
    }

    /// Save the active file's path as `last_opened` and push it to the
    /// recent-files list so the File → Open Recent menu picks it up. Quietly
    /// ignores save errors — this is a nice-to-have, not load-bearing.
    pub(super) fn remember_last_opened(&mut self) {
        let path_str = self
            .active()
            .path
            .as_ref()
            .map(|p| p.display().to_string());
        let changed = self.settings.last_opened != path_str;
        self.settings.last_opened = path_str.clone();
        if let Some(s) = path_str {
            self.settings.push_recent(&s);
        }
        if changed || !self.settings.recent_files.is_empty() {
            let _ = self.settings.save();
        }
    }

    /// Remove `path` from the recent-files list (used when an entry no longer
    /// exists on disk). Persists immediately so the next launch doesn't show
    /// the stale entry.
    pub(super) fn forget_recent(&mut self, path: &Path) {
        let s = path.display().to_string();
        self.settings.forget_recent(&s);
        let _ = self.settings.save();
    }

    pub(super) fn clear_recent(&mut self) {
        self.settings.recent_files.clear();
        let _ = self.settings.save();
    }

    pub(super) fn activate(&mut self, i: usize) {
        // Snapshot the previous tab's camera so we can restore it on return.
        let prev = self.active;
        if prev < self.files.len() {
            self.files[prev].camera = Some(self.viewer.camera_snapshot());
        }
        self.active = i;
        if self.active().last_result.is_none() {
            self.compile_active();
        } else {
            self.refresh_viewer_from_active();
        }
        // Restore stored camera if we have one; otherwise leave Viewer's
        // freshly-fitted view in place.
        if let Some(snap) = self.files[self.active].camera {
            self.viewer.restore_camera(snap);
        }
        self.persist_open_tabs();
    }

    pub(super) fn refresh_viewer_from_active(&mut self) {
        use crate::pipeline::Stage;
        let i = self.active;
        let base_dir = self.files[i].path.as_deref().and_then(|p| p.parent());
        match &self.files[i].last_result {
            Some(r) if matches!(r.stage, Stage::Ok) => {
                if let Some(scene) = &r.scene {
                    let fit = self.files[i].first_render;
                    self.viewer.set_scene(scene, base_dir, fit);
                    self.files[i].first_render = false;
                    return;
                }
            }
            _ => {}
        }
        self.viewer.clear();
    }

    /// Entry point for every user-driven tab close (menu, Ctrl+W, tab-strip
    /// X). Clean buffers close immediately; dirty buffers raise the
    /// confirmation modal so edits aren't silently discarded.
    pub(super) fn request_close_file(&mut self, i: usize) {
        if i >= self.files.len() {
            return;
        }
        if self.files[i].dirty {
            self.pending_close_index = Some(i);
        } else {
            self.close_file(i);
        }
    }

    /// Close the tab at `i`. If it's the only open tab, replace it with a
    /// fresh untitled buffer rather than leaving the app with zero files.
    /// Dropping `llm_rx` silently abandons any in-flight Gemini call for
    /// that file.
    pub(super) fn close_file(&mut self, i: usize) {
        if self.files.len() <= 1 {
            self.files[0] = FileState::untitled();
            self.active = 0;
            self.viewer.clear();
            self.persist_open_tabs();
            return;
        }
        self.files.remove(i);
        if self.active == i {
            if self.active >= self.files.len() {
                self.active = self.files.len() - 1;
            }
            if self.active().last_result.is_none() {
                self.compile_active();
            } else {
                self.refresh_viewer_from_active();
            }
            if let Some(snap) = self.files[self.active].camera {
                self.viewer.restore_camera(snap);
            }
        } else if i < self.active {
            self.active -= 1;
        }
        self.persist_open_tabs();
    }

    pub(super) fn save_to(&mut self, path: &Path) {
        self.save_index_to(self.active, path);
    }

    /// Write `files[i].source` to `path` and update that file's bookkeeping.
    /// Extracted from `save_to` so Save All in the quit dialog can save a
    /// specific buffer without having to activate it first.
    pub(super) fn save_index_to(&mut self, i: usize, path: &Path) {
        let src = self.files[i].source.clone();
        if let Err(e) = fs::write(path, &src) {
            self.files[i].status = format!("save failed: {e}");
            return;
        }
        let f = &mut self.files[i];
        f.path = Some(path.to_path_buf());
        f.last_saved_source = src;
        f.dirty = false;
        f.status = format!("saved {}", path.display());
        if i == self.active {
            self.remember_last_opened();
        }
        // Always persist — a save on any tab can promote an untitled buffer
        // into a titled one, which mutates the open-tabs list.
        self.persist_open_tabs();
    }

    pub(super) fn save(&mut self) {
        self.save_index(self.active);
    }

    /// Save file `i` to its known path, falling back to a Save As dialog when
    /// the buffer is untitled. Returns whether the buffer is clean afterwards
    /// (false if the user cancelled the dialog).
    pub(super) fn save_index(&mut self, i: usize) -> bool {
        if let Some(p) = self.files[i].path.clone() {
            self.save_index_to(i, &p);
        } else {
            let mut dialog = rfd::FileDialog::new()
                .add_filter("MoGen DSL", &["mog"])
                .set_directory(&self.project_root);
            dialog = dialog.set_file_name(self.files[i].display_name());
            if let Some(chosen) = dialog.save_file() {
                let chosen = ensure_mog_extension(&chosen);
                self.save_index_to(i, &chosen);
            } else {
                return false;
            }
        }
        !self.files[i].dirty
    }

    pub(super) fn save_as(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("MoGen DSL", &["mog"])
            .set_directory(&self.project_root);
        if let Some(p) = &self.active().path {
            if let Some(name) = p.file_name() {
                dialog = dialog.set_file_name(name.to_string_lossy());
            }
        }
        if let Some(chosen) = dialog.save_file() {
            let chosen = ensure_mog_extension(&chosen);
            self.save_to(&chosen);
        }
    }

    pub(super) fn open_dialog(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("MoGen DSL", &["mog"])
            .set_directory(&self.project_root);
        if let Some(chosen) = dialog.pick_file() {
            self.open_path(&chosen);
        }
    }

    /// Route OS drag-and-drop onto the window into the tab stack. Each `.mog`
    /// drop goes through `open_path` so it reuses the "focus if already open"
    /// and "replace pristine untitled" logic; non-`.mog` drops are reported
    /// in the status bar rather than silently ignored.
    pub(super) fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let mut skipped = 0usize;
        for f in dropped {
            let Some(path) = f.path else {
                skipped += 1;
                continue;
            };
            let is_mog = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("mog"))
                .unwrap_or(false);
            if !is_mog {
                skipped += 1;
                continue;
            }
            self.open_path(&path);
        }
        if skipped > 0 {
            self.active_mut().status =
                format!("ignored {skipped} dropped file(s) (only .mog supported)");
        }
    }

    /// Clone the buffer at `i` into a fresh untitled tab. Source and prompts
    /// carry over; the path is intentionally dropped so saving prompts the
    /// user for a new location rather than clobbering the original.
    pub(super) fn duplicate_file(&mut self, i: usize) {
        let src = self.files[i].source.clone();
        let gen_prompt = self.files[i].gen_prompt.clone();
        let mod_prompt = self.files[i].mod_prompt.clone();
        let anim_prompt = self.files[i].anim_prompt.clone();
        let mut f = FileState::untitled();
        f.source = src;
        // Leave `last_saved_source` empty so the copy shows a dirty marker
        // — this is an unsaved duplicate until the user picks a path.
        f.dirty = true;
        f.needs_compile = true;
        f.last_edit_at = Some(Instant::now());
        f.gen_prompt = gen_prompt;
        f.mod_prompt = mod_prompt;
        f.anim_prompt = anim_prompt;
        f.status = "duplicated — save to give it a path".into();
        self.files.push(f);
        let idx = self.files.len() - 1;
        self.activate(idx);
    }

    pub(super) fn new_untitled(&mut self) {
        let mut f = FileState::untitled();
        f.source = "scene {\n  box \"b\" (size=[1, 1, 1])\n}\n".to_string();
        f.last_saved_source = f.source.clone();
        f.status = "new scene".into();
        if self.files.len() == 1 && self.files[0].is_pristine_untitled() {
            self.files[0] = f;
            self.activate(0);
        } else {
            self.files.push(f);
            let i = self.files.len() - 1;
            self.activate(i);
        }
    }
}
