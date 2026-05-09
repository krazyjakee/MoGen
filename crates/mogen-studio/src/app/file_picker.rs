//! Custom in-app file picker with rendered `.mog` thumbnails.
//!
//! Replaces the native `rfd` dialog for the Open / Import / Save As flows
//! so the body can be a grid of real 3D thumbnails instead of a system file
//! list. Thumbnails are produced by [`super::thumbnail::ThumbnailManager`]
//! — the picker just lays them out and falls back to a placeholder cell
//! while a render is in flight.
//!
//! Layout:
//! - Top: Up button + editable path bar (paste a path + Enter to jump).
//! - Body: wrapping grid of fixed-size cells (folders + `.mog` files).
//!   Each `.mog` cell shows the thumbnail (or a "rendering…" placeholder)
//!   above its filename. Folders show a folder-icon cell.
//! - Bottom: filename input (Save As only) + Confirm / Cancel buttons.
//!
//! Persists the last-browsed directory to `Settings::last_picker_dir` so
//! reopening the picker lands the user back where they were. Save As mode
//! routes through the existing `save_to` plumbing; Open routes through
//! `open_path`; Import splices `import "…"` lines into the active buffer.

use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui;

use super::thumbnail::{ThumbStatus, ThumbnailManager, THUMB_SIZE};
use super::types::UndoKey;
use super::MogenStudioApp;

/// Pixel dimensions of one grid cell. Width matches the thumbnail edge plus
/// a little padding; the height fits the thumbnail plus a one-line filename
/// label underneath.
const CELL_W: f32 = THUMB_SIZE as f32 + 16.0;
const CELL_H: f32 = THUMB_SIZE as f32 + 36.0;
const CELL_GAP: f32 = 12.0;

/// What flow opened the picker. Drives the title, primary-button label,
/// and the action that fires on confirm.
#[derive(Clone, Debug)]
pub(super) enum PickerMode {
    /// Pick a single `.mog` to load into a tab.
    Open,
    /// Pick a destination path for the active buffer. `default_name` seeds
    /// the filename input.
    SaveAs { default_name: String },
    /// Pick one or more `.mog` files to splice into the active buffer as
    /// `import "…"` lines.
    Import,
}

impl PickerMode {
    fn title(&self) -> &'static str {
        match self {
            PickerMode::Open => "Open MoG file",
            PickerMode::SaveAs { .. } => "Save MoG file as",
            PickerMode::Import => "Import MoG modules",
        }
    }

    fn confirm_label(&self) -> &'static str {
        match self {
            PickerMode::Open => "Open",
            PickerMode::SaveAs { .. } => "Save",
            PickerMode::Import => "Import",
        }
    }

    fn allows_multi(&self) -> bool {
        matches!(self, PickerMode::Import)
    }

    fn is_save(&self) -> bool {
        matches!(self, PickerMode::SaveAs { .. })
    }
}

/// One row in the directory listing.
#[derive(Clone)]
struct PickerEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

/// Modal state for the picker. Held on `MogenStudioApp::picker` while the
/// dialog is open; cleared on confirm / cancel.
pub(super) struct FilePickerState {
    pub(super) mode: PickerMode,
    current_dir: PathBuf,
    /// Directory listing for `current_dir`, sorted folders-first then
    /// case-insensitively by name.
    entries: Vec<PickerEntry>,
    /// Multi-select highlight set. For Open / Save modes only the last
    /// toggle is honoured (single-select); Import mode keeps the full set.
    selected: Vec<PathBuf>,
    /// Editable path bar — paste a directory and press Enter to jump there.
    path_input: String,
    /// Save mode: filename draft, prefilled with the source tab's display
    /// name. Combined with `current_dir` to form the destination on confirm.
    save_name_draft: String,
    /// Status string shown under the breadcrumbs ("can't read /tmp/foo: …").
    /// Cleared on every successful navigation.
    error: Option<String>,
    /// Latched on open so the path input grabs focus on the first frame.
    /// Mutually exclusive with `focus_save_name_pending`.
    focus_path_pending: bool,
    /// SaveAs mode: latches focus onto the filename input on the first frame
    /// and selects the basename so a single keystroke replaces the user-
    /// supplied portion while the `.mog` extension is left intact.
    focus_save_name_pending: bool,
    /// Inline "create folder" prompt: `Some(draft)` while the user is typing
    /// a new folder name, `None` otherwise. Cleared on confirm/cancel.
    new_folder_draft: Option<String>,
    /// Latches focus onto the new-folder input the frame it appears.
    focus_new_folder_pending: bool,
}

impl FilePickerState {
    fn new(mode: PickerMode, start_dir: PathBuf) -> Self {
        let save_name_draft = match &mode {
            PickerMode::SaveAs { default_name } => default_name.clone(),
            _ => String::new(),
        };
        let is_save = matches!(mode, PickerMode::SaveAs { .. });
        let mut s = Self {
            mode,
            current_dir: start_dir.clone(),
            entries: Vec::new(),
            selected: Vec::new(),
            path_input: start_dir.display().to_string(),
            save_name_draft,
            error: None,
            focus_path_pending: !is_save,
            focus_save_name_pending: is_save,
            new_folder_draft: None,
            focus_new_folder_pending: false,
        };
        s.refresh_entries();
        s
    }

    /// Read `current_dir` into `entries`. On error, leaves the previous
    /// listing in place and stashes the message in `error` so the user
    /// can recover without losing browse context.
    fn refresh_entries(&mut self) {
        let dir = match fs::read_dir(&self.current_dir) {
            Ok(d) => d,
            Err(e) => {
                self.error = Some(format!("can't read {}: {e}", self.current_dir.display()));
                return;
            }
        };
        self.error = None;
        let mut out: Vec<PickerEntry> = Vec::new();
        for entry in dir.flatten() {
            let path = entry.path();
            let name = entry
                .file_name()
                .to_str()
                .map(|s| s.to_owned())
                .unwrap_or_else(|| path.display().to_string());
            // Hide dotfiles — they're virtually never the file the user is
            // after and they crowd the grid.
            if name.starts_with('.') {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let is_dir = meta.is_dir();
            if !is_dir && !is_browseable_extension(&path) {
                continue;
            }
            out.push(PickerEntry {
                path,
                name,
                is_dir,
            });
        }
        out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        self.entries = out;
        self.path_input = self.current_dir.display().to_string();
    }

    fn navigate_to(&mut self, dir: PathBuf) {
        if !dir.is_dir() {
            self.error = Some(format!("not a directory: {}", dir.display()));
            return;
        }
        self.current_dir = dir;
        self.selected.clear();
        self.refresh_entries();
    }

    fn navigate_up(&mut self) {
        if let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) {
            self.navigate_to(parent);
        }
    }

    /// Create `name` as a subdirectory of `current_dir` and step into it so
    /// the user can immediately save / browse there. Rejects empty names and
    /// names containing path separators — callers should treat the resulting
    /// `error` as the surfaced reason on failure.
    fn create_folder(&mut self, name: &str) {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            self.error = Some("folder name can't be empty".into());
            return;
        }
        if trimmed.contains('/') || trimmed.contains('\\') {
            self.error = Some("folder name can't contain path separators".into());
            return;
        }
        let new_path = self.current_dir.join(trimmed);
        if let Err(e) = fs::create_dir(&new_path) {
            self.error = Some(format!("can't create {}: {e}", new_path.display()));
            return;
        }
        self.new_folder_draft = None;
        self.navigate_to(new_path);
    }

    fn toggle_select(&mut self, path: PathBuf, multi: bool) {
        if !multi {
            self.selected.clear();
            self.selected.push(path);
            return;
        }
        if let Some(pos) = self.selected.iter().position(|p| p == &path) {
            self.selected.remove(pos);
        } else {
            self.selected.push(path);
        }
    }

    fn primary_selection(&self) -> Option<&Path> {
        self.selected.last().map(|p| p.as_path())
    }

    /// Iterator of the `.mog` paths in this listing — the picker uses it
    /// to pre-warm the thumbnail engine on directory change.
    fn mog_paths(&self) -> impl Iterator<Item = &Path> {
        self.entries
            .iter()
            .filter(|e| !e.is_dir && is_mog_extension(&e.path))
            .map(|e| e.path.as_path())
    }
}

/// Files we offer in the picker grid. `.mog` is the primary case; `.glb`
/// is listed because users frequently want to cross-reference an exported
/// asset from the same dir. Other extensions are hidden so the list isn't
/// noise.
fn is_browseable_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            e == "mog" || e == "glb"
        })
        .unwrap_or(false)
}

fn is_mog_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mog"))
        .unwrap_or(false)
}

impl MogenStudioApp {
    /// Initialise picker state with a sensible starting directory and
    /// stash it on the app. Pre-warms the thumbnail engine for every
    /// `.mog` in the starting directory.
    pub(super) fn open_picker(&mut self, mode: PickerMode) {
        let start_dir = self.picker_start_dir();
        let state = FilePickerState::new(mode, start_dir);
        // Snapshot the live viewer so we can put it back when the picker
        // closes — the thumbnail engine swaps the viewer's scene under us
        // for each render. Camera + scene live separately because the app
        // already has the source-of-truth scene Arc on `files[active]`.
        self.picker_prev_camera = Some(self.viewer.camera_snapshot());
        for path in state.mog_paths() {
            self.thumbnail_mgr.request(path);
        }
        self.picker = Some(state);
    }

    /// Resolve the directory the picker should land in on open. Falls
    /// through saved `last_picker_dir` → active file's parent → project
    /// root, skipping any path that doesn't exist on disk.
    fn picker_start_dir(&self) -> PathBuf {
        if let Some(s) = self
            .settings
            .last_picker_dir
            .as_ref()
            .map(PathBuf::from)
        {
            if s.is_dir() {
                return s;
            }
        }
        if let Some(p) = self.active().path.as_ref() {
            if let Some(parent) = p.parent() {
                if parent.is_dir() {
                    return parent.to_path_buf();
                }
            }
        }
        self.project_root.clone()
    }

    /// Paint the picker modal if open. On confirm, invokes the matching
    /// file action (`open_path`, `save_to`, `apply_import_selection`)
    /// and clears its own state.
    pub(super) fn ui_file_picker(&mut self, ctx: &egui::Context) {
        if self.picker.is_none() {
            return;
        }
        let mut open = true;
        let mut do_confirm = false;
        let mut do_cancel = false;
        let mut nav_into: Option<PathBuf> = None;
        let mut nav_up = false;
        let mut path_input_submitted = false;
        let mut select_toggle: Option<(PathBuf, bool)> = None;
        let mut newly_selected_for_thumb: Option<PathBuf> = None;
        let mut new_folder_create: Option<String> = None;
        let mut new_folder_cancel = false;

        // Pin the window inside the screen rect. `egui::Window` auto-sizes
        // to fit its content, so capping width/height here keeps the modal
        // reachable on any display size.
        let screen = ctx.screen_rect();
        let max_w = (screen.width() - 40.0).max(360.0);
        let max_h = (screen.height() - 80.0).max(280.0);
        let win_w = 886.0_f32.min(max_w);
        let win_h = 600.0_f32.min(max_h);

        // Split state out of `self` so the egui closures can borrow the
        // picker mutably while the rest of the app is still reachable on
        // the next line.
        let picker = self.picker.as_mut().expect("guarded above");
        let thumbs = &self.thumbnail_mgr;

        egui::Window::new(picker.mode.title())
            .id(egui::Id::new("file_picker_modal"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([win_w, win_h])
            .min_width(360.0_f32.min(max_w))
            .min_height(280.0_f32.min(max_h))
            .max_width(max_w)
            .max_height(max_h)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                // ── Header ────────────────────────────────────────────
                egui::TopBottomPanel::top("file_picker_header")
                    .resizable(false)
                    .show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    picker.current_dir.parent().is_some(),
                                    egui::Button::new("Up"),
                                )
                                .on_hover_text("Go to parent directory")
                                .clicked()
                            {
                                nav_up = true;
                            }
                            if ui
                                .button("New folder")
                                .on_hover_text("Create a new folder in the current directory")
                                .clicked()
                            {
                                if picker.new_folder_draft.is_some() {
                                    picker.new_folder_draft = None;
                                } else {
                                    picker.new_folder_draft = Some(String::new());
                                    picker.focus_new_folder_pending = true;
                                }
                            }
                            let path_resp = ui.add(
                                egui::TextEdit::singleline(&mut picker.path_input)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("/path/to/folder"),
                            );
                            if picker.focus_path_pending {
                                path_resp.request_focus();
                                picker.focus_path_pending = false;
                            }
                            if path_resp.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                path_input_submitted = true;
                            }
                        });
                        if picker.new_folder_draft.is_some() {
                            ui.horizontal(|ui| {
                                ui.label("Folder name:");
                                let draft = picker
                                    .new_folder_draft
                                    .as_mut()
                                    .expect("guarded above");
                                let resp = ui.add(
                                    egui::TextEdit::singleline(draft)
                                        .desired_width(240.0)
                                        .hint_text("subfolder"),
                                );
                                if picker.focus_new_folder_pending {
                                    resp.request_focus();
                                    picker.focus_new_folder_pending = false;
                                }
                                let enter_pressed = resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                let escape_pressed = resp.has_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Escape));
                                let create_clicked = ui
                                    .add_enabled(
                                        !draft.trim().is_empty(),
                                        egui::Button::new("Create"),
                                    )
                                    .clicked();
                                let cancel_clicked = ui.button("Cancel").clicked();
                                if enter_pressed || create_clicked {
                                    new_folder_create = Some(draft.clone());
                                } else if escape_pressed || cancel_clicked {
                                    new_folder_cancel = true;
                                }
                            });
                        }
                        if let Some(err) = &picker.error {
                            ui.colored_label(ui.style().visuals.warn_fg_color, err);
                        }
                        ui.add_space(4.0);
                    });

                // ── Footer ────────────────────────────────────────────
                egui::TopBottomPanel::bottom("file_picker_footer")
                    .resizable(false)
                    .show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if picker.mode.is_save() {
                                ui.label("Filename:")
                                    .on_hover_text(
                                        "The .mog extension is added automatically \
                                         if you leave it off.",
                                    );
                                let save_resp = ui.add(
                                    egui::TextEdit::singleline(&mut picker.save_name_draft)
                                        .desired_width(240.0)
                                        .hint_text("scene.mog (or scene — .mog is added)"),
                                );
                                if picker.focus_save_name_pending {
                                    save_resp.request_focus();
                                    // Select the basename (everything before the last
                                    // '.') so a single keystroke replaces just the
                                    // user-supplied name and leaves the extension in
                                    // place. Falls back to selecting the whole field
                                    // when the draft has no extension.
                                    let select_end_chars = picker
                                        .save_name_draft
                                        .rfind('.')
                                        .map(|byte_idx| {
                                            picker.save_name_draft[..byte_idx]
                                                .chars()
                                                .count()
                                        })
                                        .unwrap_or_else(|| {
                                            picker.save_name_draft.chars().count()
                                        });
                                    use egui::text::{CCursor, CCursorRange};
                                    let mut st = egui::TextEdit::load_state(
                                        ui.ctx(),
                                        save_resp.id,
                                    )
                                    .unwrap_or_default();
                                    st.cursor.set_char_range(Some(CCursorRange::two(
                                        CCursor::new(0),
                                        CCursor::new(select_end_chars),
                                    )));
                                    st.store(ui.ctx(), save_resp.id);
                                    picker.focus_save_name_pending = false;
                                }
                            } else {
                                let n = picker.selected.len();
                                let label = if n == 0 {
                                    "no selection".to_string()
                                } else if n == 1 {
                                    picker
                                        .selected
                                        .last()
                                        .map(|p| {
                                            p.file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_else(|| p.display().to_string())
                                        })
                                        .unwrap_or_default()
                                } else {
                                    format!("{n} files selected")
                                };
                                ui.weak(label);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let confirm_enabled = match &picker.mode {
                                        PickerMode::Open => {
                                            picker.primary_selection().is_some()
                                        }
                                        PickerMode::Import => !picker.selected.is_empty(),
                                        PickerMode::SaveAs { .. } => {
                                            !picker.save_name_draft.trim().is_empty()
                                        }
                                    };
                                    // Spell out the gating condition for the
                                    // disabled state — silent disabled buttons
                                    // make users assume the picker is broken.
                                    let confirm_tip = if confirm_enabled {
                                        match &picker.mode {
                                            PickerMode::SaveAs { .. } => {
                                                "Save as .mog (extension auto-appended)"
                                            }
                                            _ => "Open the selected file",
                                        }
                                        .to_string()
                                    } else {
                                        match &picker.mode {
                                            PickerMode::Open => {
                                                "Disabled — pick a .mog file from the grid"
                                                    .into()
                                            }
                                            PickerMode::Import => {
                                                "Disabled — pick at least one .mog module"
                                                    .into()
                                            }
                                            PickerMode::SaveAs { .. } => {
                                                "Disabled — type a filename above (.mog \
                                                 extension is auto-appended)"
                                                    .into()
                                            }
                                        }
                                    };
                                    if ui
                                        .add_enabled(
                                            confirm_enabled,
                                            egui::Button::new(picker.mode.confirm_label()),
                                        )
                                        .on_hover_text(confirm_tip)
                                        .clicked()
                                    {
                                        do_confirm = true;
                                    }
                                    if ui.button("Cancel").clicked() {
                                        do_cancel = true;
                                    }
                                },
                            );
                        });
                        ui.add_space(4.0);
                    });

                // ── Body: wrapping grid of cells ──────────────────────
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("picker_grid")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing =
                                egui::vec2(CELL_GAP, CELL_GAP);
                            ui.horizontal_wrapped(|ui| {
                                for entry in &picker.entries {
                                    let is_selected = picker
                                        .selected
                                        .iter()
                                        .any(|p| p == &entry.path);
                                    let outcome = paint_cell(ui, entry, is_selected, thumbs);
                                    if outcome.clicked {
                                        if entry.is_dir {
                                            nav_into = Some(entry.path.clone());
                                        } else {
                                            let multi = picker.mode.allows_multi()
                                                && ui.input(|i| {
                                                    i.modifiers.command
                                                        || i.modifiers.shift
                                                });
                                            select_toggle =
                                                Some((entry.path.clone(), multi));
                                            if is_mog_extension(&entry.path) {
                                                newly_selected_for_thumb =
                                                    Some(entry.path.clone());
                                            }
                                        }
                                    }
                                    if outcome.double_clicked && !entry.is_dir {
                                        // Double-click = select + confirm.
                                        // Mirrors the native dialog
                                        // convention so muscle memory
                                        // transfers.
                                        select_toggle =
                                            Some((entry.path.clone(), false));
                                        do_confirm = true;
                                    }
                                }
                                if picker.entries.is_empty() {
                                    ui.add_space(8.0);
                                    ui.label(
                                        egui::RichText::new("No .mog files in this folder")
                                            .strong(),
                                    );
                                    ui.label(
                                        "Browse to a different directory using the path \
                                         bar above, or use \"New folder\" to create one.",
                                    );
                                }
                            });
                        });
                });
            });

        // Apply navigation + selection deltas before deciding whether to
        // close, so a same-frame double-click into a folder doesn't try
        // to confirm with the folder as the chosen file.
        if let Some(name) = new_folder_create {
            self.picker.as_mut().unwrap().create_folder(&name);
            self.prewarm_picker_thumbs();
            return;
        }
        if new_folder_cancel {
            let p = self.picker.as_mut().unwrap();
            p.new_folder_draft = None;
            p.error = None;
            return;
        }
        if nav_up {
            self.picker.as_mut().unwrap().navigate_up();
            self.prewarm_picker_thumbs();
            return;
        }
        if let Some(path) = nav_into {
            self.picker.as_mut().unwrap().navigate_to(path);
            self.prewarm_picker_thumbs();
            return;
        }
        if path_input_submitted {
            let typed = self.picker.as_ref().unwrap().path_input.clone();
            let typed = PathBuf::from(typed);
            self.picker.as_mut().unwrap().navigate_to(typed);
            self.prewarm_picker_thumbs();
            return;
        }
        if let Some((path, multi)) = select_toggle {
            self.picker.as_mut().unwrap().toggle_select(path, multi);
        }
        if let Some(path) = newly_selected_for_thumb {
            // Cheap nudge in case the user navigated in via path bar before
            // the directory pre-warm fired.
            self.thumbnail_mgr.request(&path);
        }

        if !open || do_cancel {
            self.close_picker();
            return;
        }
        if !do_confirm {
            return;
        }
        self.commit_picker();
    }

    /// Request thumbnails for every `.mog` file in the picker's current
    /// listing. Idempotent — the manager skips files it's already cached
    /// at the right mtime.
    fn prewarm_picker_thumbs(&mut self) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let paths: Vec<PathBuf> = picker.mog_paths().map(Path::to_path_buf).collect();
        for p in paths {
            self.thumbnail_mgr.request(&p);
        }
    }

    /// Cancel out of the picker without acting. Restores the live viewer's
    /// scene + camera so the viewport doesn't show whichever thumbnail was
    /// last rendered.
    fn close_picker(&mut self) {
        self.picker = None;
        self.refresh_viewer_from_active();
        if let Some(snap) = self.picker_prev_camera.take() {
            self.viewer.restore_camera(snap);
        }
    }

    /// Apply the picker's selection to the right file action, persist
    /// `last_picker_dir`, and clear state. Always restores the viewer
    /// scene before returning so the next central-panel paint shows the
    /// active file's content rather than the last thumbnail.
    fn commit_picker(&mut self) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        self.settings.set_last_picker_dir(&picker.current_dir);
        let _ = self.settings.save();

        match picker.mode {
            PickerMode::Open => {
                if let Some(path) = picker.selected.into_iter().next() {
                    self.open_path(&path);
                }
            }
            PickerMode::Import => {
                let picked: Vec<PathBuf> = picker.selected;
                if !picked.is_empty() {
                    self.apply_import_selection_via_picker(&picked);
                }
            }
            PickerMode::SaveAs { .. } => {
                let mut name = picker.save_name_draft.trim().to_string();
                if name.is_empty() {
                    return;
                }
                if Path::new(&name).extension().is_none() {
                    name.push_str(".mog");
                }
                let dest = picker.current_dir.join(&name);
                self.save_to(&dest);
            }
        }

        // Restore viewer state. `open_path` already calls into the viewer
        // for the freshly-opened scene; for Save / Import we still need to
        // put back whatever was active before the picker session.
        self.refresh_viewer_from_active();
        if let Some(snap) = self.picker_prev_camera.take() {
            self.viewer.restore_camera(snap);
        }
    }

    /// Splice `import "<path>"` lines into the active buffer using the
    /// same path-resolution rules as the original `apply_import_selection`
    /// (relative when the picked file lives somewhere reachable from the
    /// active file's directory; absolute fallback otherwise; self-imports
    /// dropped).
    fn apply_import_selection_via_picker(&mut self, picked: &[PathBuf]) {
        use crate::edit;
        use std::time::Instant;

        let i = self.active;
        let active_path = self.files[i].path.clone();
        let active_dir = active_path
            .as_ref()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));

        let mut paths: Vec<String> = Vec::new();
        let mut skipped_self = 0usize;
        for p in picked {
            let canonical = fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            if let Some(active) = &active_path {
                if let Ok(active_canon) = fs::canonicalize(active) {
                    if active_canon == canonical {
                        skipped_self += 1;
                        continue;
                    }
                }
            }
            let rel = active_dir
                .as_deref()
                .and_then(|base| relative_path(base, &canonical));
            let rendered = rel
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|| canonical.to_string_lossy().into_owned());
            paths.push(rendered);
        }
        if paths.is_empty() {
            self.active_mut().status = if skipped_self > 0 {
                "import: skipped self-import".into()
            } else {
                "import: nothing selected".into()
            };
            return;
        }

        let undo_before = self.files[i].source.clone();
        let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        let new_source = edit::insert_imports(&self.files[i].source, &path_refs);
        if new_source == self.files[i].source {
            self.active_mut().status =
                "import: every selected file is already imported".into();
            return;
        }

        let added = paths.len();
        {
            let f = &mut self.files[i];
            f.source = new_source;
            f.dirty = f.source != f.last_saved_source;
            f.needs_compile = true;
            f.last_edit_at = Some(Instant::now());
            f.status = format!(
                "import: added {added} file{}",
                if added == 1 { "" } else { "s" }
            );
        }
        let key = UndoKey {
            surface: "menu",
            attr: Some("import".into()),
            node_path: Vec::new(),
        };
        self.push_undo(i, undo_before, key);
        self.compile_active();
    }
}

/// Outcome of painting one cell. Click bubbling is the only thing the
/// caller actually needs back; all the visual chrome (border, label,
/// thumbnail) is handled inside the helper.
struct CellOutcome {
    clicked: bool,
    double_clicked: bool,
}

/// Draw one grid cell. Cells are fixed-size so the wrapping layout stays
/// even regardless of how long a filename runs.
fn paint_cell(
    ui: &mut egui::Ui,
    entry: &PickerEntry,
    is_selected: bool,
    thumbs: &ThumbnailManager,
) -> CellOutcome {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(CELL_W, CELL_H),
        egui::Sense::click(),
    );
    let painter = ui.painter_at(rect);
    let visuals = ui.style().visuals.clone();

    // Cell background. Selected gets the accent fill; hovered gets a soft
    // tint so the click target is visible before the user commits.
    let bg = if is_selected {
        visuals.selection.bg_fill
    } else if response.hovered() {
        visuals.widgets.hovered.bg_fill
    } else {
        visuals.widgets.inactive.bg_fill
    };
    painter.rect_filled(rect, egui::Rounding::same(6.0), bg);
    // Border: heavier when selected so the picked cell is unambiguous.
    let stroke = if is_selected {
        visuals.selection.stroke
    } else {
        visuals.widgets.noninteractive.bg_stroke
    };
    painter.rect_stroke(rect, egui::Rounding::same(6.0), stroke);

    // Image area at the top of the cell.
    let img_size = THUMB_SIZE as f32;
    let img_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2((CELL_W - img_size) * 0.5, 8.0),
        egui::vec2(img_size, img_size),
    );

    if entry.is_dir {
        paint_folder_glyph(&painter, img_rect, &visuals);
    } else if is_mog_extension(&entry.path) {
        match thumbs.texture(&entry.path) {
            Some(handle) => {
                painter.image(
                    handle.id(),
                    img_rect,
                    egui::Rect::from_min_max(
                        egui::pos2(0.0, 0.0),
                        egui::pos2(1.0, 1.0),
                    ),
                    egui::Color32::WHITE,
                );
            }
            None => {
                paint_thumb_placeholder(
                    &painter,
                    img_rect,
                    &visuals,
                    thumbs.status(&entry.path),
                );
            }
        }
    } else {
        paint_glb_glyph(&painter, img_rect, &visuals);
    }

    // Filename label at the bottom — single line, ellipsised so a long
    // name doesn't bust the cell width.
    let label_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 6.0, img_rect.bottom() + 4.0),
        egui::pos2(rect.right() - 6.0, rect.bottom() - 4.0),
    );
    let label_color = if is_selected {
        visuals.selection.stroke.color
    } else {
        visuals.text_color()
    };
    painter.text(
        label_rect.center_top() + egui::vec2(0.0, 2.0),
        egui::Align2::CENTER_TOP,
        truncate_for_cell(&entry.name, 18),
        egui::TextStyle::Body.resolve(ui.style()),
        label_color,
    );

    let response = response.on_hover_text(entry.path.display().to_string());
    CellOutcome {
        clicked: response.clicked(),
        double_clicked: response.double_clicked(),
    }
}

/// Folder cell glyph. A simple two-tone rectangle reads as a folder
/// without dragging an icon font in.
fn paint_folder_glyph(painter: &egui::Painter, rect: egui::Rect, visuals: &egui::Visuals) {
    let body_color = visuals.widgets.active.bg_fill;
    let tab_color = visuals.widgets.hovered.bg_fill;
    let tab_height = rect.height() * 0.18;
    let tab_width = rect.width() * 0.45;
    let tab_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2(rect.width() * 0.10, rect.height() * 0.20),
        egui::vec2(tab_width, tab_height),
    );
    let body_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + rect.width() * 0.10, tab_rect.bottom() - 2.0),
        egui::pos2(rect.right() - rect.width() * 0.10, rect.bottom() - rect.height() * 0.18),
    );
    painter.rect_filled(tab_rect, egui::Rounding::same(4.0), tab_color);
    painter.rect_filled(body_rect, egui::Rounding::same(6.0), body_color);
}

/// Placeholder for a `.glb` (we don't render those). Distinguishes them
/// from `.mog` cells at a glance.
fn paint_glb_glyph(painter: &egui::Painter, rect: egui::Rect, visuals: &egui::Visuals) {
    painter.rect_filled(rect, egui::Rounding::same(6.0), visuals.faint_bg_color);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "GLB",
        egui::TextStyle::Heading.resolve(&egui::Style::default()),
        visuals.weak_text_color(),
    );
}

/// Placeholder shown while a thumbnail is compiling / rendering / loading
/// or after a failure. Conveys progress without animating, since the
/// repaint cadence is already driven by the engine.
fn paint_thumb_placeholder(
    painter: &egui::Painter,
    rect: egui::Rect,
    visuals: &egui::Visuals,
    status: Option<ThumbStatus>,
) {
    painter.rect_filled(rect, egui::Rounding::same(6.0), visuals.faint_bg_color);
    let label = match status {
        Some(ThumbStatus::Compiling) => "compiling…",
        Some(ThumbStatus::Rendering) => "rendering…",
        Some(ThumbStatus::Loading) => "loading…",
        Some(ThumbStatus::Failed) => "render failed",
        Some(ThumbStatus::Ready) | None => "queued…",
    };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::TextStyle::Body.resolve(&egui::Style::default()),
        visuals.weak_text_color(),
    );
}

/// Trim a name down to `max_chars`, appending `…` when we drop something.
/// Cheap unicode-aware truncation — counts grapheme-ish units (Rust
/// `chars`) which is good enough for filenames.
fn truncate_for_cell(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Same `relative_path` helper as `app::files::relative_path` previously.
/// Duplicated here so the picker module isn't entangled with `files`'s
/// private section.
fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from_components: Vec<_> = from.components().collect();
    let to_components: Vec<_> = to.components().collect();
    let same_root = from_components
        .first()
        .zip(to_components.first())
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    if !same_root {
        return None;
    }
    let common = from_components
        .iter()
        .zip(to_components.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut rel = PathBuf::new();
    for _ in common..from_components.len() {
        rel.push("..");
    }
    for c in &to_components[common..] {
        rel.push(c.as_os_str());
    }
    Some(rel)
}
