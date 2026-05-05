//! Community window — browse + open published `.mog` files from MoGHub.
//!
//! Read-only browse: Discover feed with kind filters + search + paged
//! results, model detail with thumbnails and tag chips, "Open in editor"
//! that pulls every file body inline (no second round-trip — the detail
//! response carries them) into untitled tabs.
//!
//! All HTTP I/O runs on `std::thread` workers in `app/moghub.rs`; this
//! module only renders the UI and dispatches calls. Image bytes also
//! arrive via the same worker channel and are uploaded to the GPU on
//! the UI thread.
//!
//! State preservation across close/reopen: `me` (the cached signed-in
//! user) survives so the chip doesn't flicker; the rest of the dialog
//! resets so the next open re-fetches the front page.

use std::collections::HashMap;
use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use mogen_moghub_client::{
    CommentList, DependencyList, DiscoverQuery, DiscoverResponse, ModelDetail, ModelSummary,
    ModuleSuggestion, MoghubError, NotificationList, PublishFileInput, PublishRequest,
    PublishResponse, UpdatesAvailable, UserSummary,
};

use crate::app::moghub::{
    fetch_comments, fetch_deps, fetch_discover, fetch_image, fetch_model_detail,
    fetch_module_suggestions, fetch_notifications, fetch_updates, fetch_whoami,
    mark_notifications_read, post_comment, publish_model, start_signin, toggle_like, InFlight,
    MoghubMessage,
};
use crate::app::types::FileState;
use crate::app::MogenStudioApp;
use crate::viewer::{CaptureFrame, CaptureKind, CaptureRequest};

/// Items per page; matches the moghub server default and stays well under
/// `DISCOVER_MAX_LIMIT = 96`.
const PAGE_SIZE: u32 = 24;

/// Edge length (px) of the publish-dialog preview render. Matches the
/// `Generate Thumbnail` menu action so a publish-time preview and a
/// user-driven thumbnail produce visually consistent output.
const PUBLISH_THUMB_SIZE: u32 = 512;
/// On-screen size of the preview image inside the publish dialog. Smaller
/// than the rendered PNG so the dialog stays compact; egui scales the
/// 512px source down on the GPU.
const PUBLISH_THUMB_PREVIEW_SIZE: f32 = 128.0;

/// Pick a unique temp path for one publish-dialog thumbnail render. Suffixed
/// with the pid + nanos so back-to-back open/cancel cycles don't trip over a
/// previous run's leftover file.
fn publish_thumb_temp_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "mogen-publish-thumb-{}-{nanos}.png",
        std::process::id()
    ))
}

/// Kind filter pills shown above the discover grid. Mirrors the web
/// client's `?kind=` chip set; `All` is the default and sends no `kind`
/// query string at all.
#[derive(Default, Copy, Clone, PartialEq, Eq)]
enum KindFilter {
    #[default]
    All,
    Scene,
    Model,
    Module,
}

impl KindFilter {
    fn label(self) -> &'static str {
        match self {
            KindFilter::All => "All",
            KindFilter::Scene => "Scenes",
            KindFilter::Model => "Models",
            KindFilter::Module => "Modules",
        }
    }

    fn query_value(self) -> Option<&'static str> {
        match self {
            KindFilter::All => None,
            KindFilter::Scene => Some("scene"),
            KindFilter::Model => Some("model"),
            KindFilter::Module => Some("module"),
        }
    }
}

const KIND_FILTERS: [KindFilter; 4] = [
    KindFilter::All,
    KindFilter::Scene,
    KindFilter::Model,
    KindFilter::Module,
];

/// One slot in the URL → texture cache. Loading is the in-flight state;
/// Failed is sticky so we don't retry forever on a broken avatar URL.
enum ImageState {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

/// Top-level state for the Community window. Lives on `MogenStudioApp`
/// so it survives across frames; reset to `default()` on close — except
/// for the auth fields, which the app refreshes from `Settings` on
/// reopen so the chip survives close/reopen.
#[derive(Default)]
pub(crate) struct CommunityState {
    pub(crate) open: bool,
    /// What's showing in the right pane.
    view: View,
    /// Search box buffer. Empty = "front page" (no `?q=`).
    search: String,
    /// Currently selected kind pill.
    kind: KindFilter,
    /// Featured hero from the most recent first-page fetch. Stays put
    /// across "Load more" calls so the hero doesn't flicker.
    featured: Option<ModelSummary>,
    /// Items accumulated across paginated calls. The first page replaces
    /// this; subsequent "Load more" calls append.
    items: Vec<ModelSummary>,
    /// Offset for the next "Load more" call.
    next_offset: u32,
    /// Server returned a full page last time — assume more to come.
    has_more: bool,
    /// True while a "Load more" append is in flight (vs. a fresh search
    /// that replaces). The button label flips accordingly.
    appending: bool,
    /// True once the first discover call returns. Used to distinguish
    /// "still loading" from "loaded, zero results".
    loaded_once: bool,
    /// Last error string, surfaced as a banner above the grid.
    error: Option<String>,
    /// Active in-flight calls. We hold onto them so the channel stays
    /// open; on completion they transition into one of the other fields
    /// and the entry is dropped.
    pending_discover: Option<InFlight>,
    pending_detail: Option<InFlight>,
    pending_deps: Option<InFlight>,
    pending_updates: Option<InFlight>,
    pending_comments: Option<InFlight>,
    pending_post_comment: Option<InFlight>,
    pending_like: Option<InFlight>,
    pending_whoami: Option<InFlight>,
    pending_signin: Option<InFlight>,
    pending_notifications: Option<InFlight>,
    pending_notifications_read: Option<InFlight>,
    /// One in-flight `InFlight` per image URL we've kicked. The map's
    /// keys mirror `image_cache` so we don't double-kick.
    pending_images: HashMap<String, InFlight>,
    /// URL → texture cache. `Failed` is sticky.
    image_cache: HashMap<String, ImageState>,
    /// Detail pane state. `selected` is the (user, slug) the user
    /// clicked; `detail` is the loaded ModelDetail when ready.
    selected: Option<(String, String)>,
    detail: Option<ModelDetail>,
    /// Outbound + inbound graph for the currently-open model.
    deps: Option<DependencyList>,
    /// Outdated pins for the currently-open model.
    updates: Option<UpdatesAvailable>,
    /// Loaded comments for the currently-open model.
    comments: Option<CommentList>,
    /// Bbcode the user is composing in the comment box.
    comment_draft: String,
    /// Notifications inbox + unread count. Populated lazily once `me`
    /// is set; clicking the bell pops a dropdown.
    notifications: Option<NotificationList>,
    /// State for the publish dialog. `None` = closed.
    publish: Option<PublishDialogState>,
    /// Pending POST /api/models.
    pending_publish: Option<InFlight>,
    /// State for the module palette (Cmd+Shift+M). `None` = closed.
    module_palette: Option<ModulePaletteState>,
    /// Pending GET /api/registry/suggest. The worker echoes the query
    /// so stale results (typed-past) can be dropped.
    pending_module_suggest: Option<InFlight>,
    /// Currently signed-in user. Populated by `whoami` on app start
    /// (when the persisted session is still valid) and after the
    /// loopback OAuth flow completes. `None` = signed-out.
    pub(crate) me: Option<UserSummary>,
    /// Last sign-in flow error surfaced as a banner above the chip.
    auth_error: Option<String>,
}

#[derive(Default)]
enum View {
    #[default]
    Discover,
    Detail,
}

/// Module palette state. Searches `/api/registry/suggest` as the user
/// types and inserts the picked module's `use "@user/slug@v"` line at
/// the end of the active file's source. End-of-file insertion (rather
/// than at-cursor) avoids poking the TextEdit's cursor state from
/// outside the editor panel — predictable, and a `use` statement
/// sitting after the rest of the file is what `mogen-validate` accepts
/// regardless of order.
pub(crate) struct ModulePaletteState {
    /// Current search input.
    query: String,
    /// Most recent successful suggestion list, keyed by the query that
    /// produced it (so the UI can drop stale results).
    results: Vec<ModuleSuggestion>,
    results_for: String,
    /// Surface a fetch error inline; doesn't clear `results` so the
    /// user keeps seeing the last good list while diagnosing.
    error: Option<String>,
    /// Set on first paint so the search box autofocuses.
    needs_focus: bool,
}

/// Publish dialog state. Seeded when the user opens the dialog from
/// the Community menu; `source` is a snapshot of the active tab so
/// edits made in the editor while the dialog is open don't sneak into
/// the published version.
pub(crate) struct PublishDialogState {
    /// Editable. Defaults to the source's `meta(name=…)` on dialog
    /// open; the user can edit before submitting (the meta block isn't
    /// rewritten — this is a publish-time override).
    title: String,
    /// Editable. Defaults to `meta(description=…)`.
    description: String,
    /// Editable comma-separated list. Defaults to `meta(tags=[…])`
    /// joined with `, `.
    tags_input: String,
    license: String,
    visibility: String,
    publish_message: String,
    publish_as_module: bool,
    /// Filename to send for the entry file. Defaults to the active
    /// tab's `display_name`, falling back to `scene.mog` for untitled
    /// buffers.
    filename: String,
    /// Snapshot of the active tab's source at the moment the dialog
    /// opened.
    source: String,
    /// Disk location of the entry file at the moment the dialog opened.
    /// Used to resolve `import "..."` siblings for multi-file publish.
    /// `None` for untitled buffers — their imports (if any) can't be
    /// bundled, so the publish proceeds with the entry file alone.
    entry_path: Option<std::path::PathBuf>,
    /// Last error from the server, surfaced inline.
    error: Option<String>,
    /// Successful publish response — the dialog flips to a "published!"
    /// state with a "Open in browser" button when this is set.
    success: Option<PublishResponse>,
    /// Temp file the publish-thumbnail capture writes to. Set when the
    /// capture is queued and cleared after the bytes have been read back
    /// (success) or the capture failed. Removed from disk in either case.
    thumbnail_temp: Option<std::path::PathBuf>,
    /// PNG bytes of the captured preview, ready to base64-encode into the
    /// publish request. `None` while the capture is still pending.
    thumbnail_bytes: Option<Vec<u8>>,
    /// Decoded preview texture for in-dialog display. Uploaded once after
    /// the capture lands so we don't re-decode every frame.
    thumbnail_texture: Option<egui::TextureHandle>,
    /// Capture-pipeline error, if the GL worker reported one. Shown next
    /// to the preview so the user knows the upload will go without one.
    thumbnail_error: Option<String>,
}

impl MogenStudioApp {
    /// Paint the Community window. No-op when `community.open` is false.
    pub(in crate::app) fn community_window(&mut self, ctx: &egui::Context) {
        if !self.community.open {
            return;
        }
        // Lazy initial fetch — first frame after the window opens with no
        // discover state cached. Also gate on `error` so a failed initial
        // load doesn't re-fire on every repaint (the worker clears
        // `pending_discover` on completion, so without this gate the next
        // frame would see the empty state and kick again, hammering
        // MoGHub as fast as connections fail).
        if !self.community.loaded_once
            && self.community.pending_discover.is_none()
            && self.community.error.is_none()
        {
            self.kick_discover(ctx, false);
        }
        // Lazy whoami — only when a persisted token exists and we
        // haven't yet validated it this session. `me` survives close /
        // reopen so a re-open doesn't refire the call.
        if self.community.me.is_none()
            && !self.settings.moghub_session.is_empty()
            && self.community.pending_whoami.is_none()
            && self.community.pending_signin.is_none()
            && self.community.auth_error.is_none()
        {
            self.kick_whoami(ctx);
        }
        // Drain worker channels before painting so the UI reflects
        // any results that arrived during this frame.
        self.poll_community_workers(ctx);

        let mut keep_open = true;
        egui::Window::new("Community")
            .open(&mut keep_open)
            .default_width(640.0)
            .default_height(720.0)
            .resizable(true)
            .show(ctx, |ui| {
                self.draw_auth_strip(ui, ctx);
                ui.separator();
                match self.community.view {
                    View::Discover => self.draw_discover(ui, ctx),
                    View::Detail => self.draw_detail(ui, ctx),
                }
            });
        if !keep_open {
            // Preserve auth state across close/reopen — the persisted
            // token is the source of truth, and `me` is just the cached
            // whoami response. Leave the dialog state otherwise empty so
            // the next open re-fetches discover.
            let me = self.community.me.take();
            self.community = CommunityState {
                me,
                ..CommunityState::default()
            };
        }
    }

    /// Render a compact MoGHub auth chip — meant for places like the
    /// main-window status bar where vertical space is precious. Signed
    /// in: avatar + `@handle` + notification bell with unread badge,
    /// chip menu has "Open Community" + "Sign out". Signed out: a
    /// "Sign in" button that kicks the loopback OAuth flow.
    ///
    /// Cooperates with the same workers as the in-window auth strip:
    /// `kick_whoami` runs lazily on first paint when a persisted token
    /// exists.
    pub(in crate::app) fn draw_moghub_status_chip(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
    ) {
        // Lazy whoami the first time the chip is shown with a stored
        // token but no cached `me`. Mirrors the gate in
        // `community_window` so the status bar bootstraps even if the
        // Community window is never opened.
        if self.community.me.is_none()
            && !self.settings.moghub_session.is_empty()
            && self.community.pending_whoami.is_none()
            && self.community.pending_signin.is_none()
            && self.community.auth_error.is_none()
        {
            self.kick_whoami(ctx);
        }
        // Lazy first poll of the notifications inbox once we know who
        // we are. Subsequent polls fire on bell click + on app focus.
        if self.community.me.is_some()
            && self.community.notifications.is_none()
            && self.community.pending_notifications.is_none()
        {
            self.kick_notifications(ctx);
        }

        if let Some(user) = self.community.me.clone() {
            let avatar = user
                .avatar_url
                .as_ref()
                .and_then(|u| self.community_image(ctx, u));
            self.draw_notif_bell(ui, ctx);
            ui.menu_button(format!("@{}", user.handle), |ui| {
                if ui.button("Open Community").clicked() {
                    self.community.open = true;
                    ui.close_menu();
                }
                if ui.button("Refresh notifications").clicked() {
                    self.kick_notifications(ctx);
                    ui.close_menu();
                }
                if ui.button("Sign out").clicked() {
                    self.sign_out_moghub();
                    ui.close_menu();
                }
            });
            if let Some(tex) = avatar {
                ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(18.0, 18.0)));
            }
        } else if self.community.pending_signin.is_some() {
            ui.weak("MoGHub: complete sign-in in browser…");
        } else if self.community.pending_whoami.is_some() {
            ui.weak("MoGHub: checking…");
        } else if ui.small_button("Sign in to MoGHub").clicked() {
            self.kick_signin(ctx);
        }
    }

    /// Notifications bell — shows an unread badge when the inbox has
    /// any unread items. Click pops a dropdown with the most recent
    /// `module_updated` events; "Mark all read" calls the server.
    fn draw_notif_bell(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let unread = self
            .community
            .notifications
            .as_ref()
            .map(|n| n.unread)
            .unwrap_or(0);
        let label = if unread > 0 {
            format!("🔔 {unread}")
        } else {
            "🔔".to_string()
        };
        ui.menu_button(label, |ui| {
            self.draw_notif_dropdown(ui, ctx);
        });
    }

    fn draw_notif_dropdown(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let pending = self.community.pending_notifications.is_some()
            || self.community.pending_notifications_read.is_some();
        let unread_count = self
            .community
            .notifications
            .as_ref()
            .map(|n| n.unread)
            .unwrap_or(0);
        ui.horizontal(|ui| {
            ui.heading("Notifications");
            ui.weak(format!("· {unread_count} unread"));
        });
        ui.separator();
        if pending {
            ui.weak("loading…");
            return;
        }
        let items = self
            .community
            .notifications
            .as_ref()
            .map(|n| n.items.clone())
            .unwrap_or_default();
        if items.is_empty() {
            ui.weak("Nothing new.");
        } else {
            // Cap how many we show in the dropdown so the menu stays
            // bounded; everything stays available via the upcoming
            // /notifications page on the web side.
            for n in items.iter().take(20) {
                ui.horizontal(|ui| {
                    let dot = if !n.read { "● " } else { "  " };
                    let summary = match (&n.source_model, n.source_version) {
                        (Some(s), Some(v)) => format!(
                            "{dot}{} pushed @{}/{} v{}",
                            n.kind, s.user.handle, s.slug, v
                        ),
                        (Some(s), None) => {
                            format!("{dot}{} updated @{}/{}", n.kind, s.user.handle, s.slug)
                        }
                        _ => format!("{dot}{}", n.kind),
                    };
                    if let Some(s) = &n.source_model {
                        if ui.link(summary).clicked() {
                            self.community.open = true;
                            self.kick_detail(ctx, s.user.handle.clone(), s.slug.clone());
                            ui.close_menu();
                        }
                    } else {
                        ui.label(summary);
                    }
                });
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.kick_notifications(ctx);
            }
            if unread_count > 0
                && ui
                    .add_enabled(!pending, egui::Button::new("Mark all read"))
                    .clicked()
            {
                self.kick_mark_notifications_read(ctx);
            }
        });
    }

    fn kick_notifications(&mut self, ctx: &egui::Context) {
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        if token.is_empty() {
            return;
        }
        self.community.pending_notifications =
            Some(fetch_notifications(url, token, ctx.clone()));
    }

    fn kick_mark_notifications_read(&mut self, ctx: &egui::Context) {
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        if token.is_empty() {
            return;
        }
        self.community.pending_notifications_read =
            Some(mark_notifications_read(url, token, ctx.clone()));
    }

    /// Render the auth chip at the top of the Community window.
    /// Signed-out: a "Sign in with GitHub" button kicks the loopback
    /// OAuth flow. Signed-in: shows `@handle` with a Sign out menu.
    fn draw_auth_strip(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if let Some(user) = self.community.me.clone() {
                if let Some(url) = &user.avatar_url {
                    if let Some(tex) = self.community_image(ctx, url) {
                        ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(20.0, 20.0)));
                    }
                }
                ui.label(format!("Signed in as @{}", user.handle));
                if ui.small_button("Sign out").clicked() {
                    self.sign_out_moghub();
                }
            } else if self.community.pending_signin.is_some() {
                ui.label("Sign-in: complete the flow in your browser…");
            } else if self.community.pending_whoami.is_some() {
                ui.label("Checking session…");
            } else if ui.button("Sign in with GitHub").clicked() {
                self.kick_signin(ctx);
            }
        });
        if let Some(err) = &self.community.auth_error {
            ui.colored_label(egui::Color32::LIGHT_RED, err);
        }
    }

    fn draw_discover(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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
    fn draw_thumbnail(
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

    fn draw_detail(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if ui.button("← back").clicked() {
            self.community.view = View::Discover;
            self.community.detail = None;
            self.community.selected = None;
            self.community.deps = None;
            self.community.updates = None;
            self.community.comments = None;
            self.community.comment_draft.clear();
        }
        ui.separator();
        if self.community.pending_detail.is_some() {
            ui.label("loading model…");
            return;
        }
        let Some(detail) = self.community.detail.clone() else {
            ui.label("no model selected");
            return;
        };
        ui.horizontal(|ui| {
            self.draw_thumbnail(ui, ctx, detail.version.thumbnail_url.as_deref(), 128.0);
            ui.vertical(|ui| {
                ui.heading(&detail.title);
                ui.horizontal(|ui| {
                    if let Some(url) = &detail.user.avatar_url {
                        if let Some(tex) = self.community_image(ctx, url) {
                            ui.add(
                                egui::Image::new(&tex).fit_to_exact_size(egui::vec2(20.0, 20.0)),
                            );
                        }
                    }
                    ui.weak(format!("@{}/{}", detail.user.handle, detail.slug));
                });
                ui.horizontal(|ui| {
                    self.draw_like_button(ui, ctx, &detail);
                    ui.weak("·");
                    ui.weak(format!("v{}", detail.version.version));
                    ui.weak("·");
                    ui.weak(format!("{} ⑂", detail.fork_count));
                    ui.weak("·");
                    ui.weak(kind_badge(&detail.kind));
                    ui.weak("·");
                    ui.weak(format!("license {}", detail.license));
                });
                if detail.is_module {
                    ui.weak(format!(
                        "Module · {} dependent{}",
                        detail.dependent_count,
                        if detail.dependent_count == 1 { "" } else { "s" },
                    ));
                }
            });
        });
        if !detail.description.is_empty() {
            ui.label(&detail.description);
        }
        if !detail.tags.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for t in &detail.tags {
                    ui.weak(format!("#{t}"));
                }
            });
        }
        ui.separator();
        ui.label(format!("Files ({})", detail.version.files.len()));
        for f in &detail.version.files {
            ui.horizontal(|ui| {
                ui.label(&f.filename);
                if f.is_entry {
                    ui.weak("(entry)");
                }
            });
        }
        ui.separator();
        self.draw_deps_section(ui, ctx);
        // The detail response already carries every file body inline,
        // so "Open in editor" is purely local — pull the entry (and any
        // siblings) into untitled tabs without a second round-trip.
        let has_files = !detail.version.files.is_empty();
        if has_files && ui.button("Open in editor").clicked() {
            self.open_detail_in_editor(&detail);
        }
        if detail.version.files.len() > 1 {
            ui.weak(format!(
                "Opens {} files as untitled tabs — Save As to a folder so `import` resolves.",
                detail.version.files.len()
            ));
        }
        ui.separator();
        self.draw_comments_section(ui, ctx);
    }

    /// Heart button that toggles a like with optimistic UI. The
    /// optimistic flip lives directly on `community.detail` so the
    /// surrounding header redraws immediately; on error the worker
    /// reply reverts it.
    fn draw_like_button(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        detail: &ModelDetail,
    ) {
        let signed_in = self.community.me.is_some();
        let busy = self.community.pending_like.is_some();
        let label = format!(
            "{} {}",
            if detail.liked_by_me { "♥" } else { "♡" },
            detail.like_count,
        );
        let resp = ui.add_enabled(signed_in && !busy, egui::Button::new(label));
        if !signed_in {
            resp.clone().on_hover_text("Sign in to like");
        }
        if resp.clicked() {
            self.toggle_detail_like(ctx, detail);
        }
    }

    fn toggle_detail_like(&mut self, ctx: &egui::Context, detail: &ModelDetail) {
        let Some(active) = self.community.detail.as_mut() else {
            return;
        };
        // Optimistic flip — keep `community.detail` in sync so the
        // header reflects the new state immediately.
        let new_state = !active.liked_by_me;
        active.liked_by_me = new_state;
        active.like_count = if new_state {
            active.like_count + 1
        } else {
            (active.like_count - 1).max(0)
        };
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_like = Some(toggle_like(
            url,
            token,
            ctx.clone(),
            detail.user.handle.clone(),
            detail.slug.clone(),
            new_state,
        ));
    }

    /// Comments list + (signed-in only) post form. The heading carries
    /// the count; an in-flight fetch shows a placeholder. Posting
    /// echoes the new comment directly into the local list rather than
    /// re-fetching, so the user sees their comment instantly.
    fn draw_comments_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.community.pending_comments.is_some() {
            ui.weak("loading comments…");
            return;
        }
        let comments_count = self
            .community
            .comments
            .as_ref()
            .map(|c| c.comments.len())
            .unwrap_or(0);
        ui.label(format!("Comments ({})", comments_count));
        if let Some(list) = self.community.comments.clone() {
            for c in &list.comments {
                if c.deleted {
                    ui.weak("(comment deleted)");
                    ui.separator();
                    continue;
                }
                ui.horizontal_top(|ui| {
                    if let Some(url) = &c.user.avatar_url {
                        if let Some(tex) = self.community_image(ctx, url) {
                            ui.add(
                                egui::Image::new(&tex).fit_to_exact_size(egui::vec2(20.0, 20.0)),
                            );
                        }
                    }
                    ui.vertical(|ui| {
                        ui.weak(format!("@{}", c.user.handle));
                        // egui has no rich text — show the original
                        // bbcode body. Links won't be clickable; that's
                        // acceptable for the in-app preview.
                        ui.label(strip_bbcode(&c.body));
                    });
                });
                ui.separator();
            }
        }

        if self.community.me.is_some() {
            let posting = self.community.pending_post_comment.is_some();
            ui.add(
                egui::TextEdit::multiline(&mut self.community.comment_draft)
                    .desired_rows(2)
                    .hint_text("Add a comment — bbcode supported (e.g. [b]bold[/b])"),
            );
            ui.horizontal(|ui| {
                let can_post = !posting && !self.community.comment_draft.trim().is_empty();
                if ui
                    .add_enabled(can_post, egui::Button::new("Post"))
                    .clicked()
                {
                    self.kick_post_comment(ctx);
                }
                if posting {
                    ui.weak("posting…");
                }
            });
        } else {
            ui.weak("Sign in to post a comment.");
        }
    }

    fn kick_post_comment(&mut self, ctx: &egui::Context) {
        let Some((user, slug)) = self.community.selected.clone() else {
            return;
        };
        let body = self.community.comment_draft.trim().to_string();
        if body.is_empty() {
            return;
        }
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_post_comment =
            Some(post_comment(url, token, ctx.clone(), user, slug, body));
    }

    /// Render the Dependencies / Used by / Updates available sections
    /// for the currently-open model. Shows a placeholder while the
    /// graph fetches are in flight; nothing for models that have no
    /// outbound or inbound edges.
    fn draw_deps_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.community.pending_deps.is_some()
            || self.community.pending_updates.is_some()
        {
            ui.weak("loading dependency graph…");
            return;
        }
        let updates = self
            .community
            .updates
            .as_ref()
            .map(|u| u.updates.clone())
            .unwrap_or_default();
        if !updates.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(180, 130, 40),
                format!(
                    "⚠ Updates available for {} pinned dependenc{}: {}",
                    updates.len(),
                    if updates.len() == 1 { "y" } else { "ies" },
                    updates
                        .iter()
                        .map(|u| format!(
                            "@{}/{} v{}→v{}",
                            u.user, u.slug, u.pinned_version, u.latest_version
                        ))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            );
            ui.separator();
        }
        let deps = self.community.deps.clone();
        if let Some(deps) = deps {
            if !deps.dependencies.is_empty() {
                ui.label(format!("Dependencies ({})", deps.dependencies.len()));
                for edge in &deps.dependencies {
                    let m = &edge.model;
                    ui.horizontal(|ui| {
                        self.draw_thumbnail(ui, ctx, m.thumbnail_url.as_deref(), 28.0);
                        let label = format!(
                            "@{}/{} {} — {}",
                            m.user.handle,
                            m.slug,
                            edge.version_constraint,
                            m.title,
                        );
                        if ui.link(label).clicked() {
                            self.kick_detail(ctx, m.user.handle.clone(), m.slug.clone());
                        }
                    });
                }
                ui.separator();
            }
            if !deps.dependents.is_empty() {
                ui.label(format!("Used by ({})", deps.dependents.len()));
                for m in &deps.dependents {
                    ui.horizontal(|ui| {
                        self.draw_thumbnail(ui, ctx, m.thumbnail_url.as_deref(), 28.0);
                        let label =
                            format!("@{}/{} — {}", m.user.handle, m.slug, m.title);
                        if ui.link(label).clicked() {
                            self.kick_detail(ctx, m.user.handle.clone(), m.slug.clone());
                        }
                    });
                }
                ui.separator();
            }
        }
    }

    // --- worker dispatch + polling -----------------------------------

    fn kick_discover(&mut self, ctx: &egui::Context, append: bool) {
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

    fn kick_detail(&mut self, ctx: &egui::Context, user: String, slug: String) {
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_detail = Some(fetch_model_detail(
            url.clone(),
            token.clone(),
            ctx.clone(),
            user.clone(),
            slug.clone(),
        ));
        self.community.pending_deps = Some(fetch_deps(
            url.clone(),
            token.clone(),
            ctx.clone(),
            user.clone(),
            slug.clone(),
        ));
        self.community.pending_updates = Some(fetch_updates(
            url.clone(),
            token.clone(),
            ctx.clone(),
            user.clone(),
            slug.clone(),
        ));
        self.community.pending_comments = Some(fetch_comments(
            url,
            token,
            ctx.clone(),
            user.clone(),
            slug.clone(),
        ));
        self.community.selected = Some((user, slug));
        self.community.detail = None;
        self.community.deps = None;
        self.community.updates = None;
        self.community.comments = None;
        self.community.comment_draft.clear();
        self.community.view = View::Detail;
    }

    /// Open every file from a [`ModelDetail`] as a fresh untitled tab.
    /// The entry tab becomes active so the editor lands on the file the
    /// user clicked. After hand-off the Community window closes — keeps
    /// focus on the editor.
    fn open_detail_in_editor(&mut self, detail: &ModelDetail) {
        let user_handle = detail.user.handle.clone();
        let slug = detail.slug.clone();
        let version = detail.version.version;
        let mut entry_idx: Option<usize> = None;
        for f in &detail.version.files {
            let label = format!("{user_handle}@{slug}@v{version}-{}", f.filename);
            let idx = self.open_fetched_in_new_tab(label, f.source.clone());
            if f.is_entry {
                entry_idx = Some(idx);
            }
        }
        if let Some(idx) = entry_idx {
            self.active = idx;
        }
        // Close the window (preserve `me` for the chip).
        let me = self.community.me.take();
        self.community = CommunityState {
            me,
            ..CommunityState::default()
        };
    }

    /// Spawn the loopback OAuth flow. Browser opens to the desktop
    /// start endpoint; the worker holds the listener until GitHub
    /// redirects back or [`OAUTH_TIMEOUT`] elapses.
    pub(in crate::app) fn kick_signin(&mut self, ctx: &egui::Context) {
        let url = self.settings.moghub_url.clone();
        self.community.pending_signin = Some(start_signin(url, ctx.clone()));
        self.community.auth_error = None;
    }

    /// Refresh `whoami`. Called after a successful sign-in (to load the
    /// chip's `@handle`) and on app start when a persisted token is
    /// present (to validate it before showing a stale chip).
    pub(in crate::app) fn kick_whoami(&mut self, ctx: &egui::Context) {
        if self.settings.moghub_session.is_empty() {
            self.community.me = None;
            return;
        }
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_whoami = Some(fetch_whoami(url, token, ctx.clone()));
    }

    /// Open the module palette and kick an initial empty-query fetch
    /// so the user sees popular modules immediately. Called from the
    /// Community menu and from the Cmd+Shift+M shortcut.
    pub(in crate::app) fn open_module_palette(&mut self, ctx: &egui::Context) {
        self.community.module_palette = Some(ModulePaletteState {
            query: String::new(),
            results: Vec::new(),
            results_for: String::new(),
            error: None,
            needs_focus: true,
        });
        self.kick_module_suggest(ctx, String::new());
    }

    /// Render the module palette. Search field autofocuses on first
    /// paint; clicking a row appends the `use` line and closes the
    /// palette.
    pub(in crate::app) fn module_palette_dialog(&mut self, ctx: &egui::Context) {
        if self.community.module_palette.is_none() {
            return;
        }
        // Drain any in-flight registry-suggest reply.
        self.poll_module_suggest_worker();

        let mut keep_open = true;
        let mut to_insert: Option<String> = None;
        let mut new_query: Option<String> = None;
        egui::Window::new("Insert module reference")
            .open(&mut keep_open)
            .default_width(440.0)
            .resizable(true)
            .show(ctx, |ui| {
                let Some(state) = self.community.module_palette.as_mut() else {
                    return;
                };
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut state.query)
                        .hint_text("Search modules — try @alice/chair"),
                );
                if state.needs_focus {
                    resp.request_focus();
                    state.needs_focus = false;
                }
                if resp.changed() {
                    new_query = Some(state.query.clone());
                }
                if let Some(err) = &state.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }
                ui.separator();
                if state.results.is_empty() {
                    if state.query.trim().is_empty() {
                        ui.weak("Loading…");
                    } else {
                        ui.weak(
                            "No matching modules. \
                             Publish your own with Community → Publish current file…",
                        );
                    }
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            for s in &state.results {
                                let row_label = format!(
                                    "@{}/{} @{} — {}",
                                    s.user, s.slug, s.latest_version, s.title
                                );
                                if ui.button(row_label).clicked() {
                                    to_insert = Some(format!(
                                        "use \"@{}/{}@{}\"",
                                        s.user, s.slug, s.latest_version,
                                    ));
                                }
                            }
                        });
                }
            });
        if let Some(q) = new_query {
            self.kick_module_suggest(ctx, q);
        }
        if let Some(line) = to_insert {
            self.insert_module_line(line);
            self.community.module_palette = None;
        }
        if !keep_open {
            self.community.module_palette = None;
        }
    }

    fn kick_module_suggest(&mut self, ctx: &egui::Context, query: String) {
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_module_suggest =
            Some(fetch_module_suggestions(url, token, ctx.clone(), query));
    }

    fn poll_module_suggest_worker(&mut self) {
        let Some(inflight) = &self.community.pending_module_suggest else {
            return;
        };
        let Some(msg) = inflight.try_recv() else {
            return;
        };
        self.community.pending_module_suggest = None;
        if let MoghubMessage::ModuleSuggestions { query, result } = msg {
            let Some(state) = self.community.module_palette.as_mut() else {
                return;
            };
            // Drop stale results: the user has typed past the query
            // this fetch was for.
            if state.query != query {
                return;
            }
            match result {
                Ok(items) => {
                    state.results = items;
                    state.results_for = query;
                    state.error = None;
                }
                Err(e) => {
                    state.error = Some(format_err(&e));
                }
            }
        }
    }

    /// Append `line` to the active tab's source on its own line, with
    /// a trailing newline. Mirrors the web client's RegistryPalette
    /// insertion strategy — predictable for the user, doesn't poke the
    /// editor's TextEdit cursor state from outside.
    fn insert_module_line(&mut self, line: String) {
        let i = self.active;
        let Some(file) = self.files.get_mut(i) else {
            return;
        };
        if !file.source.ends_with('\n') && !file.source.is_empty() {
            file.source.push('\n');
        }
        file.source.push_str(&line);
        file.source.push('\n');
        file.dirty = file.source != file.last_saved_source;
        file.needs_compile = true;
        file.last_edit_at = Some(std::time::Instant::now());
    }

    /// Seed and open the publish dialog from the active tab. Caller
    /// (the Community menu handler) has already gated on `me.is_some()`,
    /// so reaching this with no signed-in user means a bug — we still
    /// guard defensively by returning early.
    pub(in crate::app) fn open_publish_dialog(&mut self) {
        if self.community.me.is_none() {
            return;
        }
        let active = self.active();
        let active_source = active.source.clone();
        let active_display = active.display_name();
        let active_path = active.path.clone();
        let suggested_filename = if active_display.ends_with(".mog") {
            active_display.clone()
        } else if active_display == "untitled" {
            "scene.mog".to_string()
        } else {
            format!("{active_display}.mog")
        };

        // Pull title/description/tags off the source's `meta(...)` block.
        // Auto-default `publish_as_module` to on when there are no
        // top-level `import` declarations — a self-contained file is the
        // common shape of a registry-importable module. Parse failures
        // fall back to empty meta + scene; the dialog will block
        // publishing on the missing title.
        let (meta, has_imports) = match mogen_dsl::parse(&active_source) {
            Ok(ast) => {
                let meta = mogen_dsl::extract_meta(&ast).unwrap_or_default();
                let has_imports = ast.iter().any(|n| n.kind == "import");
                (meta, has_imports)
            }
            Err(_) => (Default::default(), false),
        };
        let tags_input = meta
            .tags
            .iter()
            .map(|t| t.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");

        // Kick off a thumbnail capture of the live viewport. The GL paint
        // callback consumes the request on the next frame and writes a PNG
        // to `thumbnail_temp`; `publish_dialog` drains the outcome and
        // uploads a preview texture. Submitting the request here (rather
        // than on first paint of the dialog) keeps the typical fast path
        // — open dialog, glance at preview, click Publish — under one frame
        // of dead air.
        let thumbnail_temp = publish_thumb_temp_path();
        self.viewer.submit_capture(CaptureRequest {
            kind: CaptureKind::Publish,
            size: PUBLISH_THUMB_SIZE,
            bg: self.settings.viewer_bg_rgb(),
            frames: vec![CaptureFrame {
                yaw: std::f32::consts::FRAC_PI_4,
                pitch: 0.5,
                time: 0.0,
                path: thumbnail_temp.clone(),
            }],
            total: 0,
            written: Vec::new(),
            error: None,
        });

        self.community.publish = Some(PublishDialogState {
            title: meta.name.unwrap_or_default(),
            description: meta.description.unwrap_or_default(),
            tags_input,
            license: "CC0-1.0".to_string(),
            visibility: "public".to_string(),
            publish_message: String::new(),
            publish_as_module: !has_imports,
            filename: suggested_filename,
            source: active_source,
            entry_path: active_path,
            error: None,
            success: None,
            thumbnail_temp: Some(thumbnail_temp),
            thumbnail_bytes: None,
            thumbnail_texture: None,
            thumbnail_error: None,
        });
    }

    /// Render the publish dialog. No-op when closed. Designed to be
    /// called from the central app paint pass after the menu and
    /// status bar are drawn, so it floats above them.
    pub(in crate::app) fn publish_dialog(&mut self, ctx: &egui::Context) {
        if self.community.publish.is_none() {
            return;
        }
        // Drain any in-flight publish result so the dialog's success /
        // error state reflects the latest worker reply.
        self.poll_publish_worker();
        // Drain a Publish-kind capture outcome (if any) before the dialog
        // body draws so the preview shows up in the same frame the GL
        // worker reports completion.
        self.poll_publish_thumbnail(ctx);
        let mut keep_open = true;
        let me_handle = self
            .community
            .me
            .as_ref()
            .map(|u| u.handle.clone())
            .unwrap_or_default();
        let posting = self.community.pending_publish.is_some();
        let mut submit = false;
        let mut open_in_browser: Option<String> = None;
        egui::Window::new("Publish to MoGHub")
            .open(&mut keep_open)
            .default_width(440.0)
            .resizable(true)
            .show(ctx, |ui| {
                let Some(state) = self.community.publish.as_mut() else {
                    return;
                };
                if let Some(success) = state.success.clone() {
                    ui.heading("Published ✓");
                    ui.label(format!(
                        "Your model is live at {}{}",
                        self.settings.moghub_url.trim_end_matches('/'),
                        success.url_path,
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("Open in browser").clicked() {
                            open_in_browser = Some(format!(
                                "{}{}",
                                self.settings.moghub_url.trim_end_matches('/'),
                                success.url_path,
                            ));
                        }
                        if ui.button("Close").clicked() {
                            // Close handled by setting publish to None
                            // outside the closure.
                            state.success = None; // sentinel so the
                                                  // outer code closes
                                                  // the window
                        }
                    });
                    if state.success.is_none() {
                        // User clicked Close inside the success branch.
                    }
                    return;
                }

                // Title / description / tags default to the source's
                // `meta(...)` block on open — edits here override for
                // this publish without rewriting the file.
                ui.horizontal(|ui| {
                    ui.label("Title");
                    ui.add(
                        egui::TextEdit::singleline(&mut state.title)
                            .hint_text("from meta(name = \"…\")"),
                    );
                });
                ui.weak(format!("Publishing as @{me_handle} (slug auto-assigned)"));
                ui.horizontal(|ui| {
                    ui.label("Filename");
                    ui.add(egui::TextEdit::singleline(&mut state.filename));
                });
                ui.label("Description");
                ui.add(
                    egui::TextEdit::multiline(&mut state.description)
                        .desired_rows(3)
                        .hint_text("from meta(description = \"…\")"),
                );

                ui.horizontal(|ui| {
                    ui.label("License");
                    egui::ComboBox::from_id_salt("publish_license")
                        .selected_text(state.license.as_str())
                        .show_ui(ui, |ui| {
                            for opt in ["CC0-1.0", "CC-BY-4.0", "CC-BY-SA-4.0", "MIT"] {
                                ui.selectable_value(
                                    &mut state.license,
                                    opt.to_string(),
                                    opt,
                                );
                            }
                        });
                    ui.label("Visibility");
                    egui::ComboBox::from_id_salt("publish_visibility")
                        .selected_text(state.visibility.as_str())
                        .show_ui(ui, |ui| {
                            for opt in ["public", "unlisted", "private"] {
                                ui.selectable_value(
                                    &mut state.visibility,
                                    opt.to_string(),
                                    opt,
                                );
                            }
                        });
                });

                ui.horizontal(|ui| {
                    ui.label("Tags");
                    ui.add(
                        egui::TextEdit::singleline(&mut state.tags_input)
                            .hint_text("comma-separated, max 8"),
                    );
                });

                ui.horizontal(|ui| {
                    ui.label("Publish message");
                    ui.add(egui::TextEdit::singleline(&mut state.publish_message));
                });

                ui.checkbox(
                    &mut state.publish_as_module,
                    "Publish as module (other authors can `use \"@…\"` it)",
                );

                ui.label("Preview");
                ui.horizontal(|ui| {
                    let dim = egui::vec2(
                        PUBLISH_THUMB_PREVIEW_SIZE,
                        PUBLISH_THUMB_PREVIEW_SIZE,
                    );
                    if let Some(tex) = state.thumbnail_texture.as_ref() {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(dim));
                    } else {
                        let (rect, _) = ui.allocate_exact_size(dim, egui::Sense::hover());
                        ui.painter().rect_filled(
                            rect,
                            4.0,
                            ui.style().visuals.widgets.inactive.bg_fill,
                        );
                    }
                    ui.vertical(|ui| {
                        if let Some(err) = state.thumbnail_error.as_deref() {
                            ui.colored_label(
                                egui::Color32::LIGHT_RED,
                                format!("preview render failed: {err}"),
                            );
                            ui.weak("Publishing without a thumbnail.");
                        } else if state.thumbnail_bytes.is_none() {
                            ui.weak("Rendering preview…");
                        } else {
                            ui.weak("Captured from the live viewport.");
                        }
                    });
                });

                if let Some(err) = &state.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }

                ui.horizontal(|ui| {
                    let label = if posting { "Publishing…" } else { "Publish" };
                    if ui
                        .add_enabled(!posting, egui::Button::new(label))
                        .clicked()
                    {
                        submit = true;
                    }
                    if !posting && ui.button("Cancel").clicked() {
                        // Sentinel: clear title + flag a cancel so the
                        // outer close-logic shuts the window.
                        state.title.clear();
                        state.success = None;
                        state.error = Some("__cancel__".to_string());
                    }
                });
            });

        if let Some(url) = open_in_browser {
            let _ = webbrowser::open(&url);
        }
        // Close logic: window closed by chrome, by Cancel sentinel, or
        // by the success branch's Close button (which clears success).
        let cancelled = self
            .community
            .publish
            .as_ref()
            .map(|s| s.error.as_deref() == Some("__cancel__"))
            .unwrap_or(false);
        if !keep_open || cancelled {
            // Best-effort: if the capture is still in flight when the user
            // bails out of the dialog, scrub the temp file so it doesn't
            // sit around in /tmp. The capture pipeline will still write
            // it once on the next paint, but `poll_publish_thumbnail`'s
            // "dialog closed" branch will mop that up next time it fires.
            if let Some(state) = self.community.publish.as_ref() {
                if let Some(p) = state.thumbnail_temp.as_deref() {
                    let _ = std::fs::remove_file(p);
                }
            }
            self.community.publish = None;
        }
        if submit {
            self.kick_publish(ctx);
        }
    }

    fn kick_publish(&mut self, ctx: &egui::Context) {
        let Some(state) = self.community.publish.as_mut() else {
            return;
        };
        if state.title.trim().is_empty() {
            state.error = Some("title is required".to_string());
            return;
        }
        state.error = None;

        // Bundle every sibling `.mog` reachable through `import "..."`. Skips
        // registry uses (`use "@user/slug"`) — those resolve through `mog.lock`
        // on the consumer side, so re-uploading them would duplicate bytes.
        // Untitled buffers (no `entry_path`) skip the walk entirely; their
        // imports — if any — can't resolve without an on-disk base dir, so
        // bundling fails gracefully and the publish proceeds with the entry
        // alone.
        let mut files = vec![PublishFileInput {
            filename: state.filename.clone(),
            source: state.source.clone(),
            is_entry: true,
        }];
        if let Some(entry_path) = state.entry_path.as_ref() {
            if let Some(entry_dir) = entry_path.parent() {
                match mogen_dsl::collect_local_import_files(entry_dir, &state.source) {
                    Ok(imports) => {
                        for (name, source) in imports {
                            if name == state.filename {
                                state.error = Some(format!(
                                    "import filename collides with entry filename `{}` — \
                                     rename one before publishing",
                                    state.filename
                                ));
                                return;
                            }
                            files.push(PublishFileInput {
                                filename: name,
                                source,
                                is_entry: false,
                            });
                        }
                    }
                    Err(e) => {
                        state.error = Some(format!("collecting imports: {e}"));
                        return;
                    }
                }
            }
        }

        let thumbnail_png_base64 = state
            .thumbnail_bytes
            .as_ref()
            .map(|bytes| STANDARD.encode(bytes));
        let req = PublishRequest {
            title: state.title.clone(),
            description: state.description.clone(),
            license: state.license.clone(),
            visibility: state.visibility.clone(),
            publish_message: state.publish_message.clone(),
            tags: state
                .tags_input
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .take(8)
                .collect(),
            files,
            thumbnail_png_base64,
            parent_version_id: None,
            publish_as_module: state.publish_as_module,
        };
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_publish = Some(publish_model(url, token, ctx.clone(), req));
    }

    /// Drain a `Publish`-kind capture outcome from the viewer, if one is
    /// ready. On success: read the temp PNG into `thumbnail_bytes`, decode
    /// it, and upload an in-dialog preview texture. The temp file is
    /// deleted either way — its bytes are owned in memory from this point
    /// on.
    fn poll_publish_thumbnail(&mut self, ctx: &egui::Context) {
        let Some(outcome) = self
            .viewer
            .take_capture_outcome_if(|kind| matches!(kind, CaptureKind::Publish))
        else {
            return;
        };
        let Some(state) = self.community.publish.as_mut() else {
            // Dialog closed between capture submission and outcome — clean
            // up the temp file the worker just wrote and drop the bytes.
            for path in &outcome.frame_paths {
                let _ = std::fs::remove_file(path);
            }
            return;
        };
        let temp = state.thumbnail_temp.take();
        if let Some(err) = outcome.error {
            state.thumbnail_error = Some(err);
            if let Some(p) = temp.as_deref() {
                let _ = std::fs::remove_file(p);
            }
            return;
        }
        let path = match outcome.frame_paths.last().cloned().or(temp.clone()) {
            Some(p) => p,
            None => {
                state.thumbnail_error =
                    Some("capture produced no output".to_string());
                return;
            }
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    let rgba = img.to_rgba8();
                    let size =
                        [rgba.width() as usize, rgba.height() as usize];
                    let pixels = rgba.into_raw();
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        size, &pixels,
                    );
                    state.thumbnail_texture = Some(ctx.load_texture(
                        "publish_thumbnail",
                        color,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                state.thumbnail_bytes = Some(bytes);
            }
            Err(e) => {
                state.thumbnail_error =
                    Some(format!("read preview: {e}"));
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    fn poll_publish_worker(&mut self) {
        let Some(inflight) = &self.community.pending_publish else {
            return;
        };
        let Some(msg) = inflight.try_recv() else {
            return;
        };
        self.community.pending_publish = None;
        if let MoghubMessage::Published(result) = msg {
            if let Some(state) = self.community.publish.as_mut() {
                match result {
                    Ok(r) => {
                        state.success = Some(r);
                        state.error = None;
                    }
                    Err(e) => state.error = Some(format_err(&e)),
                }
            }
        }
    }

    /// Clear the persisted token + cached `me`. Best-effort save —
    /// failure to persist leaves the in-memory state signed-out, which
    /// is the conservative choice (next launch will reload from disk
    /// and re-sign-in if the file write actually failed).
    pub(in crate::app) fn sign_out_moghub(&mut self) {
        let _ = self.settings.clear_moghub_session();
        self.community.me = None;
        self.community.auth_error = None;
    }

    fn poll_community_workers(&mut self, ctx: &egui::Context) {
        // Discover.
        if let Some(inflight) = &self.community.pending_discover {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_discover = None;
                if let MoghubMessage::Discover(result) = msg {
                    let appending = self.community.appending;
                    self.community.appending = false;
                    self.community.loaded_once = true;
                    match result {
                        Ok(d) => self.apply_discover_page(d, appending),
                        Err(e) => self.community.error = Some(format_err(&e)),
                    }
                }
            }
        }
        // ModelDetail.
        if let Some(inflight) = &self.community.pending_detail {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_detail = None;
                if let MoghubMessage::ModelDetail { user, slug, result } = msg {
                    // Drop stale results whose (user, slug) no longer
                    // matches what the user clicked.
                    if self.community.selected.as_ref()
                        == Some(&(user.clone(), slug.clone()))
                    {
                        match result {
                            Ok(d) => self.community.detail = Some(d),
                            Err(e) => self.community.error = Some(format_err(&e)),
                        }
                    }
                }
            }
        }
        // Deps — same staleness guard as ModelDetail.
        if let Some(inflight) = &self.community.pending_deps {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_deps = None;
                if let MoghubMessage::Deps { user, slug, result } = msg {
                    if self.community.selected.as_ref() == Some(&(user, slug)) {
                        if let Ok(d) = result {
                            self.community.deps = Some(d);
                        }
                        // Failures are silent — the section just hides;
                        // a missing graph isn't worth a top-of-window
                        // banner over.
                    }
                }
            }
        }
        // Updates — same staleness guard.
        if let Some(inflight) = &self.community.pending_updates {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_updates = None;
                if let MoghubMessage::Updates { user, slug, result } = msg {
                    if self.community.selected.as_ref() == Some(&(user, slug)) {
                        if let Ok(u) = result {
                            self.community.updates = Some(u);
                        }
                    }
                }
            }
        }
        // Comments list.
        if let Some(inflight) = &self.community.pending_comments {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_comments = None;
                if let MoghubMessage::Comments { user, slug, result } = msg {
                    if self.community.selected.as_ref() == Some(&(user, slug)) {
                        if let Ok(list) = result {
                            self.community.comments = Some(list);
                        }
                    }
                }
            }
        }
        // Like toggle.
        if let Some(inflight) = &self.community.pending_like {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_like = None;
                if let MoghubMessage::Like {
                    user,
                    slug,
                    was_liking,
                    result,
                } = msg
                {
                    let still_open = self.community.selected.as_ref()
                        == Some(&(user, slug));
                    if let Some(d) = self.community.detail.as_mut() {
                        if still_open {
                            match result {
                                Ok(r) => {
                                    // Server is authoritative — overwrite
                                    // optimistic state with returned values.
                                    d.liked_by_me = r.liked;
                                    d.like_count = r.like_count;
                                }
                                Err(e) => {
                                    // Revert optimistic flip.
                                    d.liked_by_me = !was_liking;
                                    d.like_count = if was_liking {
                                        (d.like_count - 1).max(0)
                                    } else {
                                        d.like_count + 1
                                    };
                                    self.community.error = Some(format_err(&e));
                                }
                            }
                        }
                    }
                }
            }
        }
        // PostedComment → splice the new entry to the top of the
        // local list (server returns it newest-first; clearing the
        // draft text only on success keeps the user's input safe if
        // posting failed).
        if let Some(inflight) = &self.community.pending_post_comment {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_post_comment = None;
                if let MoghubMessage::PostedComment { user, slug, result } = msg {
                    let still_open = self.community.selected.as_ref()
                        == Some(&(user, slug));
                    match result {
                        Ok(c) => {
                            if still_open {
                                self.community.comment_draft.clear();
                                let list = self
                                    .community
                                    .comments
                                    .get_or_insert_with(|| CommentList { comments: vec![] });
                                list.comments.insert(0, c);
                            }
                        }
                        Err(e) => self.community.error = Some(format_err(&e)),
                    }
                }
            }
        }
        // Notifications fetch.
        if let Some(inflight) = &self.community.pending_notifications {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_notifications = None;
                if let MoghubMessage::Notifications(result) = msg {
                    if let Ok(list) = result {
                        self.community.notifications = Some(list);
                    }
                }
            }
        }
        // Notifications mark-read.
        if let Some(inflight) = &self.community.pending_notifications_read {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_notifications_read = None;
                if let MoghubMessage::NotificationsRead(result) = msg {
                    if let Ok(list) = result {
                        self.community.notifications = Some(list);
                    }
                }
            }
        }
        // WhoAmI → cache the signed-in user (or clear it on 401).
        if let Some(inflight) = &self.community.pending_whoami {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_whoami = None;
                if let MoghubMessage::WhoAmI(result) = msg {
                    match result {
                        Ok(w) => self.community.me = w.user,
                        Err(MoghubError::Unauthorized) => {
                            // Token revoked / expired server-side. Drop
                            // it from settings so we don't keep retrying.
                            let _ = self.settings.clear_moghub_session();
                            self.community.me = None;
                        }
                        Err(e) => self.community.auth_error = Some(format_err(&e)),
                    }
                }
            }
        }
        // SignedIn → persist token + kick whoami to populate the chip.
        if let Some(inflight) = &self.community.pending_signin {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_signin = None;
                if let MoghubMessage::SignedIn(result) = msg {
                    match result {
                        Ok(token) => {
                            if let Err(e) = self.settings.set_moghub_session(&token) {
                                self.community.auth_error =
                                    Some(format!("signed in but couldn't persist token: {e}"));
                            }
                            self.kick_whoami(ctx);
                        }
                        Err(reason) => {
                            self.community.auth_error = Some(reason);
                        }
                    }
                }
            }
        }
        // Image fetches — drain every completed worker into the cache.
        // Collect the (key, decoded-or-failed) pairs first so we don't
        // borrow `self.community` mutably while still iterating it.
        let mut done: Vec<(String, ImageState)> = Vec::new();
        for (key, inflight) in &self.community.pending_images {
            let Some(msg) = inflight.try_recv() else {
                continue;
            };
            if let MoghubMessage::Image(result) = msg {
                let state = match result {
                    Ok(bytes) => match decode_image(&bytes) {
                        Ok((size, rgba)) => {
                            let color = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                            let tex = ctx.load_texture(
                                format!("moghub:{key}"),
                                color,
                                egui::TextureOptions::LINEAR,
                            );
                            ImageState::Ready(tex)
                        }
                        Err(_) => ImageState::Failed,
                    },
                    Err(_) => ImageState::Failed,
                };
                done.push((key.clone(), state));
            }
        }
        for (key, state) in done {
            self.community.pending_images.remove(&key);
            self.community.image_cache.insert(key, state);
        }
    }

    /// Merge a fresh discover response into the dialog state. First
    /// pages reset; appended pages tack onto the existing list.
    fn apply_discover_page(&mut self, page: DiscoverResponse, appending: bool) {
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
    fn open_fetched_in_new_tab(&mut self, label: String, source: String) -> usize {
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

/// Decode arbitrary image bytes (PNG/JPG/WebP — whatever GitHub or our
/// own server returns) into an RGBA8 buffer + size suitable for
/// `ColorImage::from_rgba_unmultiplied`.
fn decode_image(bytes: &[u8]) -> Result<([usize; 2], Vec<u8>), image::ImageError> {
    let img = image::load_from_memory(bytes)?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok((size, rgba.into_raw()))
}

/// Drop bbcode tags so a comment body renders cleanly in plain egui
/// text. Crude but enough for the in-app preview — the web client
/// renders the same bodies via a server-side bbcode parser.
fn strip_bbcode(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' {
            // Skip to the matching `]`. Anything else (no `]` ever, or a
            // newline first) we just emit verbatim so we don't lose
            // user content.
            let mut tag = String::new();
            let mut closed = false;
            for c2 in chars.by_ref() {
                if c2 == ']' {
                    closed = true;
                    break;
                }
                if c2 == '\n' {
                    out.push('[');
                    out.push_str(&tag);
                    out.push('\n');
                    closed = true;
                    break;
                }
                tag.push(c2);
            }
            if !closed {
                out.push('[');
                out.push_str(&tag);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn kind_badge(kind: &str) -> String {
    match kind {
        "scene" => "scene".to_string(),
        "model" => "model".to_string(),
        "module" => "module".to_string(),
        other => other.to_string(),
    }
}

fn format_err(e: &MoghubError) -> String {
    match e {
        MoghubError::Network(s) => format!("couldn't reach MoGHub: {s}"),
        MoghubError::Status { code, body } => {
            if body.is_empty() {
                format!("server error {code}")
            } else {
                format!("server error {code}: {body}")
            }
        }
        MoghubError::Unauthorized => "sign-in required".to_string(),
        MoghubError::Decode(s) => format!("decode error: {s}"),
    }
}
