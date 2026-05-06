//! Discover feed: search box, kind pills, summary rows, thumbnails,
//! and the URL-keyed image cache shared with the rest of the dialog.

use mogen_moghub_client::{DiscoverQuery, DiscoverResponse, ModelSummary};

use crate::app::moghub::{fetch_discover, fetch_image};
use crate::app::types::FileState;
use crate::app::MogenStudioApp;

use super::state::{ImageState, KindFilter, KIND_FILTERS, PAGE_SIZE};
use super::util::kind_badge;

impl MogenStudioApp {
    pub(super) fn draw_discover(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label("Search:");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.community.search)
                    .hint_text("title or description"),
            );
            if (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button("Go").clicked()
            {
                self.kick_discover(ctx, false);
            }
            if ui.button("Reset").clicked() {
                self.community.search.clear();
                self.community.kind = KindFilter::All;
                self.kick_discover(ctx, false);
            }
        });
        // Kind filter pills.
        ui.horizontal(|ui| {
            for k in KIND_FILTERS {
                let selected = self.community.kind == k;
                if ui.selectable_label(selected, k.label()).clicked() && !selected {
                    self.community.kind = k;
                    self.kick_discover(ctx, false);
                }
            }
        });
        ui.separator();
        if let Some(err) = self.community.error.clone() {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::LIGHT_RED, err);
                if ui.button("Retry").clicked() {
                    self.kick_discover(ctx, false);
                }
            });
            ui.separator();
        }
        let pending_initial =
            self.community.pending_discover.is_some() && !self.community.appending;
        if pending_initial && self.community.items.is_empty() {
            ui.label("loading…");
            return;
        }

        let featured = self.community.featured.clone();
        let items = self.community.items.clone();
        let count = items.len();
        let has_more = self.community.has_more;
        let appending = self.community.appending;

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if let Some(f) = featured.as_ref() {
                    ui.heading("Featured");
                    self.draw_summary_row(ui, f, ctx, true);
                    ui.add_space(8.0);
                }
                ui.heading(format!("Discover ({count} item{})", if count == 1 { "" } else { "s" }));
                if count == 0 && !pending_initial {
                    ui.weak("No models match — try a different search or clear the filter.");
                    return;
                }
                for item in &items {
                    self.draw_summary_row(ui, item, ctx, false);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if has_more {
                        let label = if appending { "Loading…" } else { "Load more" };
                        if ui
                            .add_enabled(!appending, egui::Button::new(label))
                            .clicked()
                        {
                            self.kick_discover(ctx, true);
                        }
                    } else if count > 0 {
                        ui.weak("End of feed.");
                    }
                });
            });
    }

    fn draw_summary_row(
        &mut self,
        ui: &mut egui::Ui,
        item: &ModelSummary,
        ctx: &egui::Context,
        featured: bool,
    ) {
        let thumb_size = if featured { 96.0 } else { 64.0 };
        ui.horizontal(|ui| {
            // Thumbnail (left).
            self.draw_thumbnail(ui, ctx, item.thumbnail_url.as_deref(), thumb_size);
            ui.vertical(|ui| {
                let title = format!("{} — @{}/{}", item.title, item.user.handle, item.slug);
                if ui.link(title).clicked() {
                    self.kick_detail(ctx, item.user.handle.clone(), item.slug.clone());
                }
                ui.horizontal(|ui| {
                    if let Some(url) = &item.user.avatar_url {
                        if let Some(tex) = self.community_image(ctx, url) {
                            ui.add(
                                egui::Image::new(&tex).fit_to_exact_size(egui::vec2(16.0, 16.0)),
                            );
                        }
                    }
                    ui.weak(format!("@{}", item.user.handle));
                    ui.weak("·");
                    ui.weak(kind_badge(&item.kind));
                    ui.weak("·");
                    ui.weak(format!("{} ♥", item.like_count));
                    ui.weak(format!("· {} ⑂", item.fork_count));
                });
                if !item.description.is_empty() {
                    ui.weak(item.description.lines().next().unwrap_or(""));
                }
                if !item.tags.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for t in &item.tags {
                            ui.weak(format!("#{t}"));
                        }
                    });
                }
            });
        });
        ui.separator();
    }

    /// Render a thumbnail at `size` pixels square, falling back to a
    /// neutral placeholder while the bytes load (or stay missing).
    pub(super) fn draw_thumbnail(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        url: Option<&str>,
        size: f32,
    ) {
        let dim = egui::vec2(size, size);
        if let Some(url) = url {
            if let Some(tex) = self.community_image(ctx, url) {
                ui.add(egui::Image::new(&tex).fit_to_exact_size(dim));
                return;
            }
        }
        // Placeholder — fixed-size rect so layout doesn't jump when the
        // texture eventually arrives.
        let (rect, _) = ui.allocate_exact_size(dim, egui::Sense::hover());
        ui.painter().rect_filled(
            rect,
            4.0,
            ui.style().visuals.widgets.inactive.bg_fill,
        );
    }

    /// Pull a texture for `url` from the cache, kicking a worker fetch
    /// the first time we see it. Returns `None` while loading or after
    /// a permanent failure (avatar host down, 404, etc.).
    pub(in crate::app) fn community_image(
        &mut self,
        ctx: &egui::Context,
        url: &str,
    ) -> Option<egui::TextureHandle> {
        match self.community.image_cache.get(url) {
            Some(ImageState::Ready(t)) => Some(t.clone()),
            Some(ImageState::Loading) | Some(ImageState::Failed) => None,
            None => {
                let base = self.settings.moghub_url.clone();
                let token = self.settings.moghub_session.clone();
                let inflight = fetch_image(base, token, ctx.clone(), url.to_string());
                self.community
                    .pending_images
                    .insert(url.to_string(), inflight);
                self.community
                    .image_cache
                    .insert(url.to_string(), ImageState::Loading);
                None
            }
        }
    }

    pub(super) fn kick_discover(&mut self, ctx: &egui::Context, append: bool) {
        let kind = self.community.kind.query_value().map(String::from);
        let q = if self.community.search.is_empty() {
            DiscoverQuery {
                kind,
                limit: Some(PAGE_SIZE),
                offset: Some(if append { self.community.next_offset } else { 0 }),
                ..Default::default()
            }
        } else {
            DiscoverQuery {
                q: Some(self.community.search.clone()),
                kind,
                limit: Some(PAGE_SIZE),
                offset: Some(if append { self.community.next_offset } else { 0 }),
                ..Default::default()
            }
        };
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_discover = Some(fetch_discover(url, token, ctx.clone(), q));
        self.community.error = None;
        self.community.appending = append;
        if !append {
            self.community.items.clear();
            self.community.next_offset = 0;
            self.community.has_more = false;
            self.community.featured = None;
        }
    }

    /// Merge a fresh discover response into the dialog state. First
    /// pages reset; appended pages tack onto the existing list.
    pub(super) fn apply_discover_page(&mut self, page: DiscoverResponse, appending: bool) {
        if !appending {
            self.community.featured = page.featured;
            self.community.items = page.items;
            self.community.next_offset = self.community.items.len() as u32;
        } else {
            // Featured is suppressed by the server when offset > 0, so
            // we keep whatever we already had.
            let added = page.items.len() as u32;
            self.community.items.extend(page.items);
            self.community.next_offset += added;
        }
        // Heuristic: a full page implies more is available. A short or
        // empty page closes the feed.
        let last_page_len = if appending {
            // The diff between current items and the offset we requested
            // — but easier to just re-derive from items length minus the
            // pre-call offset, which we conveniently already advanced.
            self.community.items.len() as u32 - (self.community.next_offset - PAGE_SIZE).max(0)
        } else {
            self.community.items.len() as u32
        };
        self.community.has_more = last_page_len >= PAGE_SIZE;
    }

    /// Open a fetched MoGHub source as a fresh untitled tab. Returns
    /// the index of the new tab so callers handling multi-file payloads
    /// can pick which one to focus.
    pub(super) fn open_fetched_in_new_tab(&mut self, label: String, source: String) -> usize {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut tab = FileState::untitled(id);
        tab.source = source.clone();
        tab.last_saved_source = source;
        tab.dirty = true;
        tab.needs_compile = true;
        tab.status = format!("from MoGHub: {label}");
        self.files.push(tab);
        let idx = self.files.len() - 1;
        self.active = idx;
        idx
    }
}
