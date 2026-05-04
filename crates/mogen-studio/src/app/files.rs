use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

/// Read `path`'s modified time, dropping any platform error (we treat
/// "no mtime" the same as an untitled buffer — watcher disabled, no prompts).
pub(super) fn mtime_of(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

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
                let tab_id = self.next_tab_id();
                let mtime = mtime_of(path);
                let f = FileState::loaded(tab_id, path.to_path_buf(), src, mtime);
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
        // Skip when prev == i: that's not a real tab switch (e.g. open_path
        // replacing a pristine untitled, or activate called on the current
        // tab) and would clobber a freshly-loaded file's `camera = None`
        // with the stale viewer pose, defeating the first-render auto-fit.
        let prev = self.active;
        if prev != i && prev < self.files.len() {
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
                    self.viewer.set_scene(scene.clone(), base_dir, fit);
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

    /// Close every tab except `anchor`. Dirty tabs are skipped with a status
    /// message — same single-shot constraint as `close_tabs_to_right`. Walks
    /// right-to-left so removals don't shift the anchor underneath us.
    pub(super) fn close_other_tabs(&mut self, anchor: usize) {
        if self.files.len() <= 1 || anchor >= self.files.len() {
            return;
        }
        // Resolve the anchor by tab id, not index — closing any clean tab to
        // its left would otherwise shift the target index, and the loop would
        // close the wrong tab next.
        let anchor_id = self.files[anchor].tab_id;
        let mut closed = 0usize;
        let mut skipped = 0usize;
        let mut i = self.files.len();
        while i > 0 {
            i -= 1;
            if self.files[i].tab_id == anchor_id {
                continue;
            }
            if self.files[i].dirty {
                skipped += 1;
            } else {
                self.close_file(i);
                closed += 1;
            }
        }
        self.set_batch_close_status(closed, skipped);
    }

    /// Close every tab strictly to the right of `anchor`. Dirty tabs are
    /// skipped (the per-tab confirmation modal is single-shot, so a batch
    /// close would clobber it); the remaining clean tabs are removed
    /// right-to-left so indices stay valid mid-loop.
    pub(super) fn close_tabs_to_right(&mut self, anchor: usize) {
        if anchor + 1 >= self.files.len() {
            return;
        }
        let mut closed = 0usize;
        let mut skipped = 0usize;
        let mut i = self.files.len();
        while i > anchor + 1 {
            i -= 1;
            if self.files[i].dirty {
                skipped += 1;
            } else {
                self.close_file(i);
                closed += 1;
            }
        }
        self.set_batch_close_status(closed, skipped);
    }

    /// Close every tab. Dirty tabs are skipped with a status message — the
    /// user can save them and re-run. If only one clean tab remains,
    /// `close_file` replaces it with a fresh untitled buffer.
    pub(super) fn close_all_tabs(&mut self) {
        let mut closed = 0usize;
        let mut skipped = 0usize;
        let mut i = self.files.len();
        while i > 0 {
            i -= 1;
            if self.files[i].dirty {
                skipped += 1;
            } else {
                self.close_file(i);
                closed += 1;
            }
        }
        self.set_batch_close_status(closed, skipped);
    }

    fn set_batch_close_status(&mut self, closed: usize, skipped: usize) {
        let msg = match (closed, skipped) {
            (0, 0) => return,
            (n, 0) => format!("closed {n} tab{}", if n == 1 { "" } else { "s" }),
            (0, n) => format!(
                "skipped {n} tab{} with unsaved changes",
                if n == 1 { "" } else { "s" }
            ),
            (c, s) => format!(
                "closed {c} tab{}, skipped {s} with unsaved changes",
                if c == 1 { "" } else { "s" }
            ),
        };
        self.active_mut().status = msg;
    }

    /// Close the tab at `i`. If it's the only open tab, replace it with a
    /// fresh untitled buffer rather than leaving the app with zero files.
    /// Dropping `llm_rx` silently abandons any in-flight Gemini call for
    /// that file. Titled tabs are pushed onto `recently_closed` so
    /// Ctrl+Shift+T can re-open them; untitled tabs are skipped (no path
    /// to re-open).
    pub(super) fn close_file(&mut self, i: usize) {
        if let Some(p) = self.files.get(i).and_then(|f| f.path.clone()) {
            self.push_recently_closed(p);
        }
        if self.files.len() <= 1 {
            let tab_id = self.next_tab_id();
            self.files[0] = FileState::untitled(tab_id);
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
        let src = mogen_dsl::stamp_mogen_version(
            &self.files[i].source,
            env!("CARGO_PKG_VERSION"),
        );
        // Reflect the stamped version back into the live buffer so the editor,
        // dirty-tracking, and parser all agree on what's on disk.
        self.files[i].source = src.clone();
        if let Err(e) = fs::write(path, &src) {
            self.files[i].status = format!("save failed: {e}");
            return;
        }
        // Capture the new on-disk mtime so the watcher's next tick treats this
        // save as our own write rather than as an external edit. If the
        // filesystem won't give us an mtime we leave it `None` — better to
        // disable watching for this file than to spuriously prompt.
        let mtime = mtime_of(path);
        let f = &mut self.files[i];
        f.path = Some(path.to_path_buf());
        f.last_saved_source = src;
        f.dirty = false;
        f.disk_mtime = mtime;
        f.last_watch_check = Some(Instant::now());
        f.status = format!("saved {}", path.display());
        if i == self.active {
            self.remember_last_opened();
        }
        // Always persist — a save on any tab can promote an untitled buffer
        // into a titled one, which mutates the open-tabs list.
        self.persist_open_tabs();
    }

    pub(super) fn save(&mut self) {
        if self.files[self.active].path.is_some() {
            self.save_index(self.active);
        } else {
            // Untitled buffer — defer to the custom picker so Cmd+S on a
            // fresh tab gets the same `.mog` preview list as Save As. The
            // synchronous rfd path on `save_index` is still used by the
            // quit / external-conflict modals where blocking is expected.
            self.save_as();
        }
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
        let default_name = self
            .active()
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                let mut name = self.active().display_name();
                if Path::new(&name).extension().is_none() {
                    name.push_str(".mog");
                }
                name
            });
        self.open_picker(super::file_picker::PickerMode::SaveAs { default_name });
    }

    pub(super) fn open_dialog(&mut self) {
        self.open_picker(super::file_picker::PickerMode::Open);
    }

    /// Pick one or more `.mog` files and splice `import "<path>"` lines into
    /// the active buffer at the top. Paths are emitted relative to the active
    /// file's directory when one exists and the picked file lives somewhere
    /// reachable; otherwise they fall back to absolute. Already-imported and
    /// self-imports are filtered out.
    pub(super) fn import_dialog(&mut self) {
        self.open_picker(super::file_picker::PickerMode::Import);
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
        let tab_id = self.next_tab_id();
        let mut f = FileState::untitled(tab_id);
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

    /// Push a path onto the recently-closed stack. Dedupes — if the path is
    /// already on the stack we move it to the top rather than letting the same
    /// file occupy multiple slots. Bounded by `RECENTLY_CLOSED_CAP` (oldest
    /// drops off the front).
    fn push_recently_closed(&mut self, path: PathBuf) {
        self.recently_closed.retain(|p| p != &path);
        self.recently_closed.push_back(path);
        while self.recently_closed.len() > super::RECENTLY_CLOSED_CAP {
            self.recently_closed.pop_front();
        }
    }

    /// Has at least one path on the reopen stack — used to enable/disable the
    /// File → Reopen Closed Tab menu item.
    pub(super) fn has_recently_closed(&self) -> bool {
        !self.recently_closed.is_empty()
    }

    /// Pop the most recently closed tab and re-open it, falling through to
    /// the next entry if the file is gone from disk. No-op when the stack
    /// drains without finding a survivor.
    pub(super) fn reopen_last_closed(&mut self) {
        while let Some(path) = self.recently_closed.pop_back() {
            if path.is_file() {
                self.open_path(&path);
                return;
            }
        }
        self.active_mut().status = "no recently closed tabs to reopen".into();
    }

    pub(super) fn new_untitled(&mut self) {
        let tab_id = self.next_tab_id();
        let mut f = FileState::untitled(tab_id);
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
