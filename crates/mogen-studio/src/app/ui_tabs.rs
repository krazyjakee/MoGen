use eframe::egui;

use super::MogenStudioApp;

impl MogenStudioApp {
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
        let mut reveal_path: Option<std::path::PathBuf> = None;
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
                        // Prefix with a leading bullet for unsaved buffers so
                        // dirty state is visible at a glance, not just on the
                        // trailing edge where a long filename can push it
                        // out of view.
                        let mut label = String::new();
                        if f.dirty {
                            label.push_str("• ");
                        }
                        label.push_str(&f.display_name());
                        if f.llm_in_flight.is_some() {
                            label.push_str(" ⟳");
                        }
                        let resp = ui.selectable_label(selected, label);
                        let resp = if f.dirty || f.llm_in_flight.is_some() {
                            resp.on_hover_text(if f.dirty && f.llm_in_flight.is_some() {
                                "• unsaved changes · ⟳ AI request in progress"
                            } else if f.dirty {
                                "• unsaved changes — Cmd/Ctrl+S to save"
                            } else {
                                "⟳ AI request in progress"
                            })
                        } else {
                            resp
                        };
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
                            if ui
                                .add_enabled(has_path, egui::Button::new("Reveal in file system"))
                                .on_hover_text(if has_path {
                                    "Open the OS file manager at this MOG file's location"
                                } else {
                                    "Save the MOG file first to give it a path"
                                })
                                .clicked()
                            {
                                if let Some(p) = &f.path {
                                    reveal_path = Some(p.clone());
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
                        // Larger hit-area than `small_button("×")`. Frame is
                        // off so the X looks the same when idle but registers
                        // a wider clickable region for trackpad / touch users.
                        let x_resp = ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("×").strong(),
                                )
                                .frame(false)
                                .min_size(egui::vec2(18.0, 18.0)),
                            )
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
        if let Some(path) = reveal_path {
            let status = match crate::app::editor_link::reveal_in_os(&path) {
                Ok(()) => format!("revealed {}", path.display()),
                Err(e) => format!("reveal failed: {} ({e})", path.display()),
            };
            self.active_mut().status = status;
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
