use eframe::egui;

use crate::docs::{self, DOC_PAGES};

use super::types::{DocsState, GITHUB_REPO_URL};
use super::MogenStudioApp;

impl MogenStudioApp {
    /// Open the Documentation window on a specific page + section. Used by
    /// Ctrl+click in the editor to land on the relevant heading. The window
    /// is non-modal — opening it doesn't steal focus from the editor.
    pub(super) fn open_docs_at(&mut self, page: &str, slug: Option<String>) {
        // Push the previous spot onto the back-stack so the Back button can
        // step out of a Ctrl+click jump. Skip the push when the page hasn't
        // been initialised yet (first open) or when the destination matches
        // the current view (avoids no-op history entries).
        let dest_page = page.to_string();
        if !self.docs.page_id.is_empty()
            && (self.docs.page_id != dest_page || self.docs.pending_scroll != slug)
        {
            self.docs
                .history
                .push((self.docs.page_id.clone(), self.docs.pending_scroll.clone()));
            // Cap so a long browse session doesn't grow unbounded.
            const MAX_HIST: usize = 32;
            let len = self.docs.history.len();
            if len > MAX_HIST {
                self.docs.history.drain(0..len - MAX_HIST);
            }
        }
        self.docs.page_id = dest_page;
        self.docs.pending_scroll = slug;
        self.show_docs = true;
    }

    /// Open the Documentation window without changing the current page.
    /// First-time opens default to the DSL reference (the page users hit
    /// most often).
    pub(super) fn open_docs_home(&mut self) {
        if self.docs.page_id.is_empty() {
            self.docs.page_id = "dsl".to_string();
        }
        self.show_docs = true;
    }

    pub(super) fn ui_docs(&mut self, ctx: &egui::Context) {
        if !self.show_docs {
            return;
        }
        // First open: default to the DSL page so the window has something
        // sensible to render.
        if self.docs.page_id.is_empty() {
            self.docs.page_id = "dsl".to_string();
        }

        let mut open = true;
        // Draft fields written by the inner closures so we can apply them
        // after the borrow on `self.docs` ends — the navigation buttons need
        // mutable access to the same struct.
        let mut nav_to: Option<(String, Option<String>)> = None;
        let mut go_back = false;
        let mut filter_draft = self.docs.outline_filter.clone();

        let page = docs::page_by_id(&self.docs.page_id)
            .copied()
            .unwrap_or(DOC_PAGES[0]);
        let outline = docs::page_outline(&page);
        let pending_scroll = self.docs.pending_scroll.clone();
        let history_depth = self.docs.history.len();
        let current_page_id = self.docs.page_id.clone();

        // Cap the window to the viewport so a wide code block in the
        // markdown can't grow it past the screen on first open. egui's
        // Resize widget will otherwise expand to fit content larger than
        // `default_size`, which is what shipped before.
        let screen = ctx.screen_rect();
        let max_w = (screen.width() - 40.0).max(520.0);
        let max_h = (screen.height() - 40.0).max(360.0);

        egui::Window::new("Documentation")
            .id(egui::Id::new("docs_window"))
            .open(&mut open)
            .resizable(true)
            .collapsible(true)
            .default_width(820.0_f32.min(max_w))
            .default_height(620.0_f32.min(max_h))
            .min_width(520.0_f32.min(max_w))
            .min_height(360.0_f32.min(max_h))
            .max_width(max_w)
            .max_height(max_h)
            .show(ctx, |ui| {
                // Top toolbar — page tabs + Back + outline filter.
                ui.horizontal(|ui| {
                    let back_enabled = history_depth > 0;
                    if ui
                        .add_enabled(back_enabled, egui::Button::new("◀ Back"))
                        .on_hover_text(if back_enabled {
                            "Return to the previous page / section"
                        } else {
                            "(no history yet)"
                        })
                        .clicked()
                    {
                        go_back = true;
                    }
                    ui.separator();
                    for p in DOC_PAGES {
                        let selected = p.id == current_page_id;
                        if ui
                            .selectable_label(selected, p.title)
                            .on_hover_text(p.subtitle)
                            .clicked()
                            && !selected
                        {
                            nav_to = Some((p.id.to_string(), None));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.hyperlink_to("View on GitHub", GITHUB_REPO_URL)
                            .on_hover_text(
                                "Open the canonical rendered docs on GitHub. Useful when \
                                 you want to share a link or follow off-tree references.",
                            );
                    });
                });
                ui.add_space(4.0);
                ui.separator();

                // Body: outline sidebar + scrollable content.
                let avail = ui.available_size();
                ui.horizontal_top(|ui| {
                    let sidebar_width = (avail.x * 0.28).clamp(180.0, 280.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(sidebar_width, avail.y),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.label(
                                egui::RichText::new(format!("{} — outline", page.title))
                                    .weak(),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut filter_draft)
                                    .hint_text("filter sections")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.add_space(4.0);
                            egui::ScrollArea::vertical()
                                .id_salt("docs_outline_scroll")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    let needle = filter_draft.trim().to_ascii_lowercase();
                                    for entry in &outline {
                                        if !needle.is_empty()
                                            && !entry
                                                .title
                                                .to_ascii_lowercase()
                                                .contains(&needle)
                                        {
                                            continue;
                                        }
                                        let indent = match entry.level {
                                            2 => 0.0,
                                            3 => 12.0,
                                            _ => 24.0,
                                        };
                                        ui.horizontal(|ui| {
                                            ui.add_space(indent);
                                            let selected = pending_scroll
                                                .as_deref()
                                                == Some(entry.slug.as_str());
                                            let label = egui::RichText::new(&entry.title);
                                            let label = if entry.level == 2 {
                                                label.strong()
                                            } else {
                                                label
                                            };
                                            if ui
                                                .selectable_label(selected, label)
                                                .clicked()
                                            {
                                                nav_to = Some((
                                                    page.id.to_string(),
                                                    Some(entry.slug.clone()),
                                                ));
                                            }
                                        });
                                    }
                                });
                        },
                    );

                    ui.separator();

                    // Right pane: rendered markdown.
                    let body_width = (avail.x - sidebar_width - 12.0).max(0.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(body_width, avail.y),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_max_width(body_width);
                            ui.label(
                                egui::RichText::new(page.subtitle).weak(),
                            );
                            ui.add_space(4.0);
                            // `both()` rather than `vertical()` so a long
                            // no-wrap line in a code fence scrolls
                            // horizontally inside the pane instead of
                            // expanding the Resize widget around the window.
                            egui::ScrollArea::both()
                                .id_salt(("docs_body_scroll", page.id))
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    docs::render_markdown(
                                        ui,
                                        page.source,
                                        pending_scroll.as_deref(),
                                    );
                                });
                        },
                    );
                });
            });

        // Apply deferred state mutations now that the immutable borrow on
        // `self.docs` has ended.
        self.docs.outline_filter = filter_draft;
        if go_back {
            if let Some((page_id, slug)) = self.docs.history.pop() {
                self.docs.page_id = page_id;
                self.docs.pending_scroll = slug;
            }
        } else if let Some((page_id, slug)) = nav_to {
            // Push the current spot onto the history before jumping so Back
            // returns the user to where they clicked from.
            self.docs
                .history
                .push((self.docs.page_id.clone(), self.docs.pending_scroll.clone()));
            self.docs.page_id = page_id;
            self.docs.pending_scroll = slug;
        } else {
            // Renderer scrolls on the first paint after a pending_scroll is
            // set. Clear it here so the next paint doesn't keep yanking the
            // viewport back to the top of the section as the user scrolls.
            self.docs.pending_scroll = None;
        }

        if !open {
            self.show_docs = false;
        }
    }
}

/// Compile-time check: `DocsState` must remain Default-constructible so the
/// app initialiser can spawn one without parameters. Kept as an associated
/// const so the compiler emits the error against this file rather than the
/// app struct's initialiser line.
const _: fn() -> DocsState = DocsState::default;
