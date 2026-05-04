use std::time::Instant;

use eframe::egui;

use crate::app::MogenStudioApp;

impl MogenStudioApp {
    pub(in crate::app) fn ui_editor(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let i = self.active;
        let editor_id = self.active_editor_id();

        // Lock the editor while an LLM call is in flight — the worker will
        // overwrite `source` on completion, so any keystrokes typed during
        // generation would just be discarded.
        let generating = self.files[i].llm_in_flight.is_some();
        if generating {
            self.autocomplete.close();
            // External rewrite incoming — extras would land on stale text.
            self.clear_multi_caret();
        }

        // External source mutations (gizmo, inspector, undo, LLM apply) can
        // shrink the buffer out from under any multi-cursor extras the user
        // has built up. Drop any range that no longer fits before reading.
        self.prune_invalid_extras();

        // Find bar (Ctrl+F). Painted above the editor's ScrollArea so the
        // controls stay anchored while the source scrolls beneath. The bar
        // owns its own focus via `find_input_id` — the user can keep typing
        // in the query without losing the editor's selection state.
        if self.find.open {
            // Source can change every frame (typing, undo, gizmo edits) —
            // re-search so match positions stay valid before we draw overlays.
            self.recompute_find_matches();
            self.ui_find_bar(ui);
        }

        // Consume popup navigation keys BEFORE the TextEdit is rendered — Up /
        // Down / Tab / Enter / Esc are only intercepted when the popup is
        // open, so normal editing isn't affected.
        let popup_key = self.autocomplete_key(ui);

        // Block-indent / dedent on Tab / Shift+Tab when the selection covers
        // multiple lines (or for any Shift+Tab). Runs after autocomplete so an
        // open popup keeps its claim on Tab.
        if self.handle_indent_keys(ui, editor_id) {
            changed = true;
        }

        // VS Code–style line ops (toggle comment, select/delete/move line,
        // select next occurrence). Pure-text edits + cursor restore, so they
        // run alongside indent before the TextEdit paints.
        if self.handle_line_op_keys(ui, editor_id) {
            changed = true;
        }

        // Multi-cursor fan-out: if the user has built up extra carets via
        // Cmd+D, intercept text-affecting events here so they apply to every
        // selection. Runs after line ops so Cmd+L / Cmd+/ etc. still claim
        // their shortcuts before we look at the input queue. Skipped while
        // the LLM owns the buffer.
        if !generating && self.handle_multi_caret_events(ui, editor_id) {
            changed = true;
        }

        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let palette = crate::highlight::Palette::for_visuals(&ui.style().visuals);

        // Layouter closure — runs on every repaint for the visible text. Kept
        // cheap by the single-pass tokeniser in `highlight`; caching on hash
        // would be nice but isn't needed yet at typical .mog sizes.
        let hl_font = font_id.clone();
        let mut layouter = move |ui: &egui::Ui, text: &str, _wrap_width: f32| {
            // Wrap is disabled in `highlight` — long lines scroll horizontally
            // so the gutter's one-number-per-source-line stays aligned.
            let job = crate::highlight::highlight(text, hl_font.clone(), palette);
            ui.fonts(|f| f.layout_job(job))
        };

        // Compute how many text rows fit in the visible panel so the editor
        // and the gutter column always fill the full available height —
        // regardless of whether the .mog is 3 lines or 300. With fewer
        // content rows than fit, `desired_rows` keeps the TextEdit tall and
        // clickable; with more, the outer ScrollArea takes over.
        let row_height = ui.fonts(|f| f.row_height(&font_id));
        // TextEdit reserves ~2px top + 2px bottom as inner margin; account
        // for that so the last visible row doesn't clip.
        let available_height = (ui.available_height() - 4.0).max(row_height);
        let visible_rows = ((available_height / row_height).floor() as usize).max(1);

        let mut textedit_output: Option<egui::widgets::text_edit::TextEditOutput> = None;

        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    // Gutter: one right-aligned line number per source row,
                    // padded with blank cells so the column visually extends
                    // to match the editor even when content is short.
                    // Wrapped in a Frame with the same vertical padding
                    // egui's TextEdit uses so the first row of the gutter
                    // sits on the first row of the editor.
                    let (gutter, _digits) = crate::highlight::gutter_job_padded(
                        &self.files[i].source,
                        visible_rows,
                        font_id.clone(),
                        palette,
                    );
                    egui::Frame::none()
                        .inner_margin(egui::Margin {
                            left: 4.0,
                            right: 4.0,
                            top: 2.0,
                            bottom: 2.0,
                        })
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(gutter).selectable(false).wrap_mode(egui::TextWrapMode::Extend));
                        });

                    let pending_caret = self.files[i].pending_caret.take();

                    // Snapshot the cursor range before the widget runs so the
                    // right-click menu has something to restore — egui
                    // collapses the selection on any secondary press.
                    let prior = egui::TextEdit::load_state(ui.ctx(), editor_id)
                        .and_then(|s| s.cursor.char_range());

                    let mut editor = egui::TextEdit::multiline(&mut self.files[i].source)
                        // code_editor() implies lock_focus(true), so Tab inserts
                        // a tab character instead of moving focus out of the
                        // editor — the right behavior for a code surface.
                        .code_editor()
                        .desired_rows(visible_rows)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace)
                        .layouter(&mut layouter)
                        .id(editor_id);
                    if generating {
                        editor = editor.interactive(false);
                    }
                    let output = editor.show(ui);

                    let resp = output.response.clone();
                    if resp.changed() {
                        changed = true;
                    }

                    // Re-assert the pre-press selection on secondary press
                    // so the right-click menu can see what the user had
                    // highlighted.
                    if resp.hovered() && ui.input(|i| i.pointer.secondary_pressed()) {
                        if let Some(range) = prior {
                            if range.primary.index != range.secondary.index {
                                if let Some(mut st) =
                                    egui::TextEdit::load_state(ui.ctx(), editor_id)
                                {
                                    st.cursor.set_char_range(Some(range));
                                    st.store(ui.ctx(), editor_id);
                                }
                            }
                        }
                    }
                    let mut menu_changed = false;
                    // (selected_text, label) captured at click-time so opening
                    // the modal later doesn't have to re-read editor state.
                    let mut ask_request: Option<(String, String)> = None;
                    let source_ref = &mut self.files[i].source;
                    resp.context_menu(|ui| {
                        if crate::app::text_menu::show_context_menu(ui, editor_id, source_ref) {
                            menu_changed = true;
                        }
                        ui.separator();
                        if ui
                            .add(egui::Button::new("Ask…"))
                            .on_hover_text(
                                "Ask the active provider's fast model a question about the \
                                 selected code (or the whole file if nothing is selected)",
                            )
                            .clicked()
                        {
                            ask_request = Some(crate::app::ask::capture_snippet(
                                ui,
                                editor_id,
                                source_ref,
                            ));
                            ui.close_menu();
                        }
                    });
                    if menu_changed {
                        changed = true;
                    }
                    if let Some((snippet, label)) = ask_request {
                        self.open_ask_modal(snippet, label);
                    }

                    // Move the editor caret onto the selected node's
                    // declaration when the viewport reported a new pick.
                    if let Some(offset) = pending_caret {
                        let src = &self.files[i].source;
                        let clamped = offset.min(src.len());
                        let char_idx = src[..clamped].chars().count();
                        use egui::text::{CCursor, CCursorRange};
                        if let Some(mut state) =
                            egui::TextEdit::load_state(ui.ctx(), editor_id)
                        {
                            state.cursor.set_char_range(Some(CCursorRange::one(
                                CCursor::new(char_idx),
                            )));
                            state.store(ui.ctx(), editor_id);
                            ui.ctx().memory_mut(|m| m.request_focus(editor_id));
                        }
                    }

                    // Ctrl+click navigation: jumps to imported module
                    // declarations, opens URLs / referenced files, or
                    // scrolls the in-app docs window to the matching
                    // section. Dispatched here so we run while the click is
                    // still in-flight and can paint the link cursor on
                    // hover.
                    if !generating {
                        let _ = self.handle_editor_link_click(ui, &output);
                    }

                    // Paint find-match overlays + drive scroll-to-match. Both
                    // need to happen inside this ScrollArea closure so the
                    // overlay scrolls with the text and `scroll_to_rect`
                    // targets the correct ScrollArea.
                    if self.find.open {
                        self.paint_find_overlays(ui, &output);
                        self.drive_find_scroll(ui, editor_id, &output);
                    }

                    // Multi-cursor extras paint over the same galley as the
                    // primary selection so the user sees one unified set of
                    // highlights — matching VS Code's visual treatment.
                    self.paint_multi_caret_overlays(ui, &output);

                    textedit_output = Some(output);
                });
            });

        if let Some(ref output) = textedit_output {
            // Refresh candidate list + popup anchor after the TextEdit has
            // rendered. Keyboard navigation decoded before the widget is
            // applied here so the selection/accept lands on the current
            // candidates. Skipped while the LLM owns the buffer so Tab/Enter
            // can't splice a completion into source the worker is about to
            // overwrite.
            if !generating {
                self.update_autocomplete_after_textedit(ui, output, editor_id, popup_key);
            }
        }

        if changed {
            self.files[i].dirty = self.files[i].source != self.files[i].last_saved_source;
            self.files[i].needs_compile = true;
            self.files[i].last_edit_at = Some(Instant::now());
            // The TextEdit owns its own native undo for typing — those edits
            // never enter the app stack. Reset the coalesce window so a
            // subsequent gizmo / inspector edit doesn't merge into a stack
            // entry whose `before` predates the user's typing.
            self.break_undo_chain(i);
            // Compilation itself is gated by `drive_compile_debounce` so a
            // burst of keystrokes only re-parses once the user pauses.
        }
    }
}
