use std::path::PathBuf;

use eframe::egui;

use crate::preview_shader::{preview_shader_label, PreviewShader, PREVIEW_SHADERS};
use crate::theme::{apply_theme, theme_label, Theme, THEMES};

use super::types::{
    shortcut_menu_item, MenuAction, ShortcutAction, DOCS_URL, GITHUB_REPO_URL, LICENSE_URL,
};
use super::MogenStudioApp;

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
                    "Generate a new MOG file from a natural-language prompt via Gemini",
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
                if shortcut_menu_item(ui, "Quit", ShortcutAction::Quit, "Quit MoGen Studio")
                    .clicked()
                {
                    action = MenuAction::Quit;
                    ui.close_menu();
                }
            });

            ui.menu_button("Edit", |ui| {
                if shortcut_menu_item(
                    ui,
                    "Preferences…",
                    ShortcutAction::OpenOptions,
                    "Gemini API key, thinking budget, theme",
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
            MenuAction::Save => self.save(),
            MenuAction::SaveAs => self.save_as(),
            MenuAction::Build => self.open_build_dialog(),
            MenuAction::Recheck => self.compile_active(),
            MenuAction::CloseActive => self.request_close_file(self.active),
            MenuAction::Quit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            MenuAction::OpenOptions => {
                self.options_api_key_draft = self.settings.gemini_api_key.clone();
                self.show_options = true;
            }
            MenuAction::Frame => self.viewer.frame_view(),
            MenuAction::OpenAbout => {
                self.show_about = true;
            }
        }
    }

    /// Poll every global keyboard shortcut and run its bound action. Called
    /// once per frame *before* the menu is built so that consuming the key
    /// event hides it from text widgets like the editor — pressing Ctrl+S
    /// shouldn't insert anything into the source buffer.
    pub(super) fn dispatch_shortcuts(&mut self, ctx: &egui::Context) {
        // Esc cancels an in-flight LLM call when the active tab is busy. Gated
        // so the key isn't swallowed when there's nothing to cancel.
        if self.active().llm_in_flight.is_some() {
            let esc = egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Escape);
            if ctx.input_mut(|i| i.consume_shortcut(&esc)) {
                self.cancel_active_llm();
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
        let mut duplicate: Option<usize> = None;
        let mut copy_path: Option<String> = None;
        let mut new_from_empty_area = false;
        // Total rect the tab strip gets to draw in. We compare the last item's
        // right edge against this to detect clicks on the unused tail — that's
        // where a double-click should mint a fresh MOG file.
        let strip_rect = ui.available_rect_before_wrap();
        let mut last_item_right = strip_rect.min.x;
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
    }
}
