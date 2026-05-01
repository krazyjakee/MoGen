use std::path::PathBuf;

use eframe::egui;

use crate::preview_shader::{preview_shader_label, PreviewShader, PREVIEW_SHADERS};
use crate::settings::DEFAULT_VIEWER_BG_RGB;
use crate::theme::{apply_theme, theme_label, Theme, THEMES};

use super::types::{
    native_shortcut_menu_item, shortcut_menu_item, MenuAction, ShortcutAction, DOCS_URL,
    GITHUB_REPO_URL, LICENSE_URL,
};
use super::MogenStudioApp;

/// Consume a bare-letter shortcut *and* the matching text-input event egui
/// generates alongside it. `consume_shortcut` only removes the `Event::Key`
/// event from the input stream, but egui pushes a separate `Event::Text` for
/// every printable character — TextEdit listens to that event for insertion,
/// so without stripping it the letter is consumed by us *and* still typed
/// into the focused editor. Returns whether the binding fired this frame.
/// Both upper- and lower-case text events are matched so Shift+W still
/// triggers the binding without leaving a stray "W" in the buffer.
fn consume_bare_letter(ctx: &egui::Context, key: egui::Key, letter: &str) -> bool {
    let sc = egui::KeyboardShortcut::new(egui::Modifiers::NONE, key);
    ctx.input_mut(|i| {
        if i.consume_shortcut(&sc) {
            i.events.retain(|e| {
                !matches!(e, egui::Event::Text(t) if t.eq_ignore_ascii_case(letter))
            });
            true
        } else {
            false
        }
    })
}

/// Push a synthetic key-press event into egui's input queue. Used by the Edit
/// menu to drive undo/redo/select-all — egui's `TextEdit` consumes the event
/// on the focused widget exactly as if the user had typed the shortcut.
fn inject_key(ctx: &egui::Context, key: egui::Key, modifiers: egui::Modifiers) {
    ctx.input_mut(|i| {
        i.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        });
    });
}

impl MogenStudioApp {
    pub(super) fn ui_menu_bar(&mut self, ui: &mut egui::Ui) {
        // Collected here so menu actions happen after the menu closes — keeps
        // the immediate-mode borrow of `self.files` / `self.settings` clean.
        let mut action: MenuAction = MenuAction::None;

        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if shortcut_menu_item(
                    ui,
                    "New",
                    ShortcutAction::NewUntitled,
                    "Create a fresh untitled MOG file",
                )
                .clicked()
                {
                    action = MenuAction::NewUntitled;
                    ui.close_menu();
                }
                if shortcut_menu_item(
                    ui,
                    "New from Prompt…",
                    ShortcutAction::OpenNewPromptModal,
                    "Generate a new MOG file from a natural-language prompt via the active LLM provider",
                )
                .clicked()
                {
                    action = MenuAction::OpenNewPromptModal;
                    ui.close_menu();
                }
                ui.separator();
                if shortcut_menu_item(
                    ui,
                    "Open…",
                    ShortcutAction::OpenDialog,
                    "Open a MOG file from disk",
                )
                .clicked()
                {
                    action = MenuAction::OpenDialog;
                    ui.close_menu();
                }
                if ui
                    .button("Import…")
                    .on_hover_text(
                        "Pick one or more .mog object files to add as `import \"…\"` \
                         lines at the top of the active file",
                    )
                    .clicked()
                {
                    action = MenuAction::ImportDsl;
                    ui.close_menu();
                }
                ui.menu_button("Open Recent", |ui| {
                    if self.settings.recent_files.is_empty() {
                        ui.label("(no recent MOG files)");
                    } else {
                        let recents: Vec<String> = self.settings.recent_files.clone();
                        for path in &recents {
                            let pb = PathBuf::from(path);
                            let name = pb
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.clone());
                            if ui.button(name).on_hover_text(path).clicked() {
                                action = MenuAction::OpenPath(pb);
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        if ui.button("Clear recent").clicked() {
                            action = MenuAction::ClearRecent;
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                if shortcut_menu_item(
                    ui,
                    "Save",
                    ShortcutAction::Save,
                    "Save the MOG file",
                )
                .clicked()
                {
                    action = MenuAction::Save;
                    ui.close_menu();
                }
                if shortcut_menu_item(
                    ui,
                    "Save As…",
                    ShortcutAction::SaveAs,
                    "Save the MOG file to a new path",
                )
                .clicked()
                {
                    action = MenuAction::SaveAs;
                    ui.close_menu();
                }
                ui.separator();
                if shortcut_menu_item(
                    ui,
                    "Build GLB",
                    ShortcutAction::Build,
                    "Compile the MOG file and export .glb next to it",
                )
                .clicked()
                {
                    action = MenuAction::Build;
                    ui.close_menu();
                }
                ui.menu_button("Generate", |ui| {
                    let busy = self.generate_in_flight();
                    let busy_tip = if busy {
                        "another render is in flight — wait for it to finish"
                    } else {
                        ""
                    };
                    let thumb = ui
                        .add_enabled(
                            !busy,
                            egui::Button::new("Thumbnail (PNG)"),
                        )
                        .on_hover_text(if busy {
                            busy_tip.to_string()
                        } else {
                            "Render a square 512px PNG of the current scene next to the .mog file"
                                .into()
                        });
                    if thumb.clicked() {
                        action = MenuAction::GenerateThumbnail;
                        ui.close_menu();
                    }
                    let vid = ui
                        .add_enabled(
                            !busy,
                            egui::Button::new("Rotating video (MP4)"),
                        )
                        .on_hover_text(if busy {
                            busy_tip.to_string()
                        } else {
                            "Render a 6s 30fps mp4 of the model rotating, encoded with ffmpeg"
                                .into()
                        });
                    if vid.clicked() {
                        action = MenuAction::GenerateVideo;
                        ui.close_menu();
                    }
                });
                if shortcut_menu_item(
                    ui,
                    "Re-check",
                    ShortcutAction::Recheck,
                    "Re-run validate on the MOG file without exporting",
                )
                .clicked()
                {
                    action = MenuAction::Recheck;
                    ui.close_menu();
                }
                ui.separator();
                if shortcut_menu_item(
                    ui,
                    "Close Tab",
                    ShortcutAction::CloseActive,
                    "Close the active MOG file",
                )
                .clicked()
                {
                    action = MenuAction::CloseActive;
                    ui.close_menu();
                }
                let has_reopen = self.has_recently_closed();
                ui.add_enabled_ui(has_reopen, |ui| {
                    if shortcut_menu_item(
                        ui,
                        "Reopen Closed Tab",
                        ShortcutAction::ReopenClosed,
                        "Re-open the most recently closed MOG file",
                    )
                    .clicked()
                    {
                        action = MenuAction::ReopenClosed;
                        ui.close_menu();
                    }
                });
                if shortcut_menu_item(ui, "Quit", ShortcutAction::Quit, "Quit MoGen Studio")
                    .clicked()
                {
                    action = MenuAction::Quit;
                    ui.close_menu();
                }
            });

            ui.menu_button("Edit", |ui| {
                use egui::{Key, KeyboardShortcut, Modifiers};
                let cmd = Modifiers::COMMAND;
                let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
                // Undo/redo/cut/copy/paste/select-all are handled natively by
                // egui's TextEdit — these menu items are discoverability for
                // the shortcuts and a click fallback. The actions are routed
                // to the focused text widget via event injection in
                // `dispatch_menu_action`, so clicking them works regardless of
                // which TextEdit currently has focus.
                if native_shortcut_menu_item(
                    ui,
                    "Undo",
                    KeyboardShortcut::new(cmd, Key::Z),
                    "Undo the last change in the focused text field",
                    true,
                )
                .clicked()
                {
                    action = MenuAction::Undo;
                    ui.close_menu();
                }
                if native_shortcut_menu_item(
                    ui,
                    "Redo",
                    KeyboardShortcut::new(cmd_shift, Key::Z),
                    "Redo the last undone change in the focused text field",
                    true,
                )
                .clicked()
                {
                    action = MenuAction::Redo;
                    ui.close_menu();
                }
                ui.separator();
                if native_shortcut_menu_item(
                    ui,
                    "Cut",
                    KeyboardShortcut::new(cmd, Key::X),
                    "Cut the current selection to the clipboard",
                    true,
                )
                .clicked()
                {
                    action = MenuAction::Cut;
                    ui.close_menu();
                }
                if native_shortcut_menu_item(
                    ui,
                    "Copy",
                    KeyboardShortcut::new(cmd, Key::C),
                    "Copy the current selection to the clipboard",
                    true,
                )
                .clicked()
                {
                    action = MenuAction::Copy;
                    ui.close_menu();
                }
                if native_shortcut_menu_item(
                    ui,
                    "Paste",
                    KeyboardShortcut::new(cmd, Key::V),
                    "Paste the clipboard into the focused text field",
                    true,
                )
                .clicked()
                {
                    action = MenuAction::Paste;
                    ui.close_menu();
                }
                ui.separator();
                if native_shortcut_menu_item(
                    ui,
                    "Select All",
                    KeyboardShortcut::new(cmd, Key::A),
                    "Select all text in the focused field",
                    true,
                )
                .clicked()
                {
                    action = MenuAction::SelectAll;
                    ui.close_menu();
                }
                ui.separator();
                if shortcut_menu_item(
                    ui,
                    "Preferences…",
                    ShortcutAction::OpenOptions,
                    "LLM provider, API key, thinking budget, theme",
                )
                .clicked()
                {
                    action = MenuAction::OpenOptions;
                    ui.close_menu();
                }
            });

            let mut chosen_theme: Option<Theme> = None;
            let mut chosen_shader: Option<PreviewShader> = None;
            ui.menu_button("View", |ui| {
                if shortcut_menu_item(
                    ui,
                    "Frame",
                    ShortcutAction::Frame,
                    "Re-fit the camera to the scene",
                )
                .clicked()
                {
                    action = MenuAction::Frame;
                    ui.close_menu();
                }
                let mut show_grid = self.settings.show_grid();
                if ui
                    .checkbox(&mut show_grid, "Show Grid")
                    .on_hover_text("Toggle the ground-plane reference grid in the 3D viewport")
                    .changed()
                {
                    self.settings.set_show_grid(show_grid);
                    self.viewer.set_show_grid(show_grid);
                    let _ = self.settings.save();
                }
                ui.separator();
                ui.menu_button("Shader", |ui| {
                    let current = self.settings.preview_shader();
                    for s in PREVIEW_SHADERS {
                        let selected = s == current;
                        if ui
                            .selectable_label(selected, preview_shader_label(s))
                            .clicked()
                            && !selected
                        {
                            chosen_shader = Some(s);
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button("Theme", |ui| {
                    let current = self.settings.theme();
                    for t in THEMES {
                        let selected = t == current;
                        if ui
                            .selectable_label(selected, theme_label(t))
                            .clicked()
                            && !selected
                        {
                            chosen_theme = Some(t);
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button("Background", |ui| {
                    // Inline picker (not `color_edit_button_srgb`): the button
                    // form opens a child popup, and clicks inside that popup
                    // count as "outside the menu", so the menu — and the
                    // picker with it — closes on the first press. Drawing
                    // the picker inline keeps every drag inside this menu's
                    // area.
                    let current = self.settings.viewer_bg_rgb();
                    let mut srgba =
                        egui::Color32::from_rgb(current[0], current[1], current[2]);
                    if egui::widgets::color_picker::color_picker_color32(
                        ui,
                        &mut srgba,
                        egui::widgets::color_picker::Alpha::Opaque,
                    ) {
                        let rgb = [srgba.r(), srgba.g(), srgba.b()];
                        if rgb != current {
                            self.settings.set_viewer_bg_rgb(rgb);
                            let _ = self.settings.save();
                        }
                    }
                    if ui.button("Reset to default").clicked() {
                        self.settings.set_viewer_bg_rgb(DEFAULT_VIEWER_BG_RGB);
                        let _ = self.settings.save();
                        ui.close_menu();
                    }
                });
            });
            if let Some(t) = chosen_theme {
                self.settings.set_theme(t);
                apply_theme(ui.ctx(), t);
                let _ = self.settings.save();
            }
            if let Some(s) = chosen_shader {
                self.settings.set_preview_shader(s);
                self.viewer.set_preview_shader(s);
                let _ = self.settings.save();
            }

            ui.menu_button("Help", |ui| {
                ui.hyperlink_to("GitHub repository", GITHUB_REPO_URL);
                ui.hyperlink_to("Documentation", DOCS_URL);
                ui.hyperlink_to("License (MIT)", LICENSE_URL);
                ui.separator();
                if ui
                    .button("About MoGen Studio…")
                    .on_hover_text("Version and credits")
                    .clicked()
                {
                    action = MenuAction::OpenAbout;
                    ui.close_menu();
                }
            });
        });

        self.dispatch_menu_action(ui.ctx(), action);
    }

    /// Run a `MenuAction`. Extracted so both menu clicks and keyboard
    /// shortcuts (via `dispatch_shortcuts`) hit the same code path.
    pub(super) fn dispatch_menu_action(&mut self, ctx: &egui::Context, action: MenuAction) {
        match action {
            MenuAction::None => {}
            MenuAction::NewUntitled => self.new_untitled(),
            MenuAction::OpenNewPromptModal => {
                self.new_prompt_draft.clear();
                self.show_new_prompt = true;
                self.new_prompt_focus_pending = true;
            }
            MenuAction::OpenDialog => self.open_dialog(),
            MenuAction::OpenPath(p) => {
                if p.is_file() {
                    self.open_path(&p);
                } else {
                    self.forget_recent(&p);
                    self.active_mut().status =
                        format!("recent: {} no longer exists — removed from list", p.display());
                }
            }
            MenuAction::ClearRecent => self.clear_recent(),
            MenuAction::ImportDsl => self.import_dialog(),
            MenuAction::Save => self.save(),
            MenuAction::SaveAs => self.save_as(),
            MenuAction::Build => self.open_build_dialog(),
            MenuAction::Recheck => self.compile_active(),
            MenuAction::CloseActive => self.request_close_file(self.active),
            MenuAction::ReopenClosed => self.reopen_last_closed(),
            MenuAction::Quit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            // Undo / Redo are bimodal: when a TextEdit owns focus, route the
            // event natively so the editor's per-buffer history runs; when
            // no widget is focused (the viewport / inspector are the user's
            // current surface), drive the app-level undo stack instead.
            MenuAction::Undo => {
                if ctx.memory(|m| m.focused().is_none()) {
                    self.undo_active();
                } else {
                    inject_key(ctx, egui::Key::Z, egui::Modifiers::COMMAND);
                }
            }
            MenuAction::Redo => {
                if ctx.memory(|m| m.focused().is_none()) {
                    self.redo_active();
                } else {
                    inject_key(
                        ctx,
                        egui::Key::Z,
                        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                    );
                }
            }
            MenuAction::Cut => {
                ctx.input_mut(|i| i.events.push(egui::Event::Cut));
            }
            MenuAction::Copy => {
                ctx.input_mut(|i| i.events.push(egui::Event::Copy));
            }
            MenuAction::Paste => {
                // Read synchronously so the injected event carries the
                // clipboard payload in the same frame.
                let clip = arboard::Clipboard::new()
                    .and_then(|mut c| c.get_text())
                    .unwrap_or_default();
                if !clip.is_empty() {
                    ctx.input_mut(|i| i.events.push(egui::Event::Paste(clip)));
                }
            }
            MenuAction::SelectAll => self.select_all_active_editor(ctx),
            MenuAction::OpenOptions => {
                self.options_api_key_draft = self.settings.gemini_api_key.clone();
                self.show_options = true;
            }
            MenuAction::Frame => self.viewer.frame_view(),
            MenuAction::OpenAbout => {
                self.show_about = true;
            }
            MenuAction::GenerateThumbnail => self.generate_thumbnail(ctx),
            MenuAction::GenerateVideo => self.generate_video(ctx),
        }
    }

    /// Select the entire source buffer in the active tab's code editor by
    /// writing its `TextEdit` state directly. Injecting a synthetic Cmd+A is
    /// unreliable from a menu click — the menu interaction can drop focus, and
    /// pushed events don't always reach the widget in the same frame.
    fn select_all_active_editor(&self, ctx: &egui::Context) {
        use egui::text::{CCursor, CCursorRange};

        let editor_id = self.active_editor_id();
        let total_chars = self.files[self.active].source.chars().count();
        if let Some(mut st) = egui::TextEdit::load_state(ctx, editor_id) {
            st.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(0),
                CCursor::new(total_chars),
            )));
            st.store(ctx, editor_id);
        }
        ctx.memory_mut(|m| m.request_focus(editor_id));
    }

    /// Poll every global keyboard shortcut and run its bound action. Called
    /// once per frame *before* the menu is built so that consuming the key
    /// event hides it from text widgets like the editor — pressing Ctrl+S
    /// shouldn't insert anything into the source buffer.
    pub(super) fn dispatch_shortcuts(&mut self, ctx: &egui::Context) {
        // Find (Ctrl+F / F3) — consumed first so the editor never sees the
        // keypress and the find bar opens regardless of which widget owns
        // focus.
        self.dispatch_find_shortcuts(ctx);

        // Esc cancels an in-flight LLM call when the active tab is busy. Gated
        // so the key isn't swallowed when there's nothing to cancel.
        if self.active().llm_in_flight.is_some() {
            let esc = egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Escape);
            if ctx.input_mut(|i| i.consume_shortcut(&esc)) {
                self.cancel_active_llm();
            }
        }

        // App-level undo / redo: only fire when nothing is focused, so typing
        // in the code editor or any prompt field still gets native TextEdit
        // history. Cmd+Shift+Z is tested first because its modifier set is a
        // strict superset of Cmd+Z's — otherwise the redo press would be
        // consumed as an undo.
        if ctx.memory(|m| m.focused().is_none()) {
            let undo_sc = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);
            let redo_sc = egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            );
            if ctx.input_mut(|i| i.consume_shortcut(&redo_sc)) {
                self.redo_active();
            } else if ctx.input_mut(|i| i.consume_shortcut(&undo_sc)) {
                self.undo_active();
            }
        }

        // Bare-letter gizmo mode shortcuts (Godot / Unity convention:
        // W=move, E=rotate, R=scale). Gated on cursor-over-viewport, NOT
        // on focus — clicking a node in the viewport auto-focuses the
        // editor (so the user can type after a pick), and we still want
        // W/E/R to switch gizmo modes after that. A modal text input
        // takes focus *and* sits over the viewport, so we additionally
        // refuse when the focused widget is anything other than the
        // editor — that's the "is the user actively typing in a modal"
        // check.
        let pointer_in_viewport = self
            .last_viewport_rect
            .zip(ctx.input(|i| i.pointer.hover_pos()))
            .map(|(r, p)| r.contains(p))
            .unwrap_or(false);
        let editor_id = self.active_editor_id();
        let modal_typing = ctx.memory(|m| {
            m.focused()
                .map(|id| id != editor_id)
                .unwrap_or(false)
        });
        if pointer_in_viewport && !modal_typing {
            use crate::gizmo::GizmoMode;
            for (key, letter, mode) in [
                (egui::Key::W, "w", GizmoMode::Translate),
                (egui::Key::E, "e", GizmoMode::Rotate),
                (egui::Key::R, "r", GizmoMode::Scale),
            ] {
                if consume_bare_letter(ctx, key, letter) {
                    self.viewer.set_gizmo_mode(mode);
                }
            }
        }

        let mut hit: Option<ShortcutAction> = None;
        ctx.input_mut(|i| {
            for action in ShortcutAction::ALL {
                if i.consume_shortcut(&action.shortcut()) {
                    hit = Some(*action);
                    break;
                }
            }
        });
        if let Some(action) = hit {
            self.dispatch_menu_action(ctx, action.into_menu());
        }
    }

    /// Horizontal browser-style tab strip with one entry per open MOG file.
    /// Replaces the old "Open" list that lived in the left sidebar.
    pub(super) fn ui_tabs(&mut self, ui: &mut egui::Ui) {
        let mut activate: Option<usize> = None;
        let mut close: Option<usize> = None;
        let mut close_others_of: Option<usize> = None;
        let mut close_to_right_of: Option<usize> = None;
        let mut close_all = false;
        let mut duplicate: Option<usize> = None;
        let mut copy_path: Option<String> = None;
        let mut new_from_empty_area = false;
        // Total rect the tab strip gets to draw in. We compare the last item's
        // right edge against this to detect clicks on the unused tail — that's
        // where a double-click should mint a fresh MOG file.
        let strip_rect = ui.available_rect_before_wrap();
        let mut last_item_right = strip_rect.min.x;
        let total_tabs = self.files.len();
        egui::ScrollArea::horizontal()
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, f) in self.files.iter().enumerate() {
                        let selected = i == self.active;
                        let mut label = f.display_name();
                        if f.dirty {
                            label.push_str(" •");
                        }
                        if f.llm_in_flight.is_some() {
                            label.push_str(" ⟳");
                        }
                        let resp = ui.selectable_label(selected, label);
                        if resp.clicked() {
                            activate = Some(i);
                        }
                        let has_path = f.path.is_some();
                        let has_right = i + 1 < total_tabs;
                        let has_others = total_tabs > 1;
                        resp.context_menu(|ui| {
                            if ui.button("Duplicate").clicked() {
                                duplicate = Some(i);
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(has_path, egui::Button::new("Copy path"))
                                .on_hover_text(if has_path {
                                    "Copy the absolute path of this MOG file to the clipboard"
                                } else {
                                    "Save the MOG file first to give it a path"
                                })
                                .clicked()
                            {
                                if let Some(p) = &f.path {
                                    copy_path = Some(p.display().to_string());
                                }
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Close tab").clicked() {
                                close = Some(i);
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(has_others, egui::Button::new("Close others"))
                                .on_hover_text(
                                    "Close every other tab. Tabs with \
                                     unsaved changes are skipped.",
                                )
                                .clicked()
                            {
                                close_others_of = Some(i);
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(has_right, egui::Button::new("Close to the right"))
                                .on_hover_text(
                                    "Close every tab to the right of this one. \
                                     Tabs with unsaved changes are skipped.",
                                )
                                .clicked()
                            {
                                close_to_right_of = Some(i);
                                ui.close_menu();
                            }
                            if ui
                                .button("Close all")
                                .on_hover_text(
                                    "Close every open tab. Tabs with unsaved \
                                     changes are skipped.",
                                )
                                .clicked()
                            {
                                close_all = true;
                                ui.close_menu();
                            }
                        });
                        let x_resp = ui
                            .small_button("×")
                            .on_hover_text("Close tab");
                        if x_resp.clicked() {
                            close = Some(i);
                        }
                        let sep_resp = ui.separator();
                        last_item_right = last_item_right
                            .max(x_resp.rect.right())
                            .max(sep_resp.rect.right());
                    }
                });
            });
        // Transparent click-catcher over the empty strip to the right of the
        // last tab. Double-click opens a new MOG file, mirroring the behaviour
        // of every major browser / editor tab bar.
        let empty_left = last_item_right;
        if empty_left < strip_rect.max.x {
            let empty_rect = egui::Rect::from_min_max(
                egui::pos2(empty_left, strip_rect.min.y),
                egui::pos2(strip_rect.max.x, strip_rect.max.y),
            );
            let empty_resp = ui.interact(
                empty_rect,
                egui::Id::new("tabs_empty_space"),
                egui::Sense::click(),
            );
            if empty_resp.double_clicked() {
                new_from_empty_area = true;
            }
            empty_resp.on_hover_text("Double-click to open a new MOG file");
        }
        if new_from_empty_area {
            self.new_untitled();
        }
        if let Some(i) = activate {
            self.activate(i);
        }
        if let Some(i) = duplicate {
            self.duplicate_file(i);
        }
        if let Some(path) = copy_path {
            ui.output_mut(|o| o.copied_text = path.clone());
            self.active_mut().status = format!("copied path: {path}");
        }
        if let Some(i) = close {
            self.request_close_file(i);
        }
        if let Some(i) = close_others_of {
            self.close_other_tabs(i);
        }
        if let Some(i) = close_to_right_of {
            self.close_tabs_to_right(i);
        }
        if close_all {
            self.close_all_tabs();
        }
    }
}
