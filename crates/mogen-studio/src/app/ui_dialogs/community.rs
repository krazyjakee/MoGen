//! Community window — browse + open published `.mog` files from MoGHub.
//!
//! The v1 surface is read-only: Discover feed → click a model → preview
//! its title/description/tags → "Open in editor" loads the published
//! source into a fresh untitled tab. Auth, publish, and social actions
//! land in subsequent slices.
//!
//! All HTTP I/O runs on `std::thread` workers in `app/moghub.rs`; this
//! module only renders the UI and dispatches calls.

use mogen_moghub_client::{
    DiscoverQuery, DiscoverResponse, ModelDetail, ModelSummary, MoghubError, UserSummary,
};

use crate::app::moghub::{
    fetch_discover, fetch_file_source, fetch_model_detail, fetch_whoami, start_signin, InFlight,
    MoghubMessage,
};
use crate::app::types::FileState;
use crate::app::MogenStudioApp;

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
    /// Most recent successful Discover fetch.
    discover: Option<DiscoverResponse>,
    /// Last error string, surfaced as a banner above the grid.
    error: Option<String>,
    /// Active in-flight calls. We hold onto them so the channel stays
    /// open; on completion they transition into one of the other fields
    /// and the entry is dropped.
    pending_discover: Option<InFlight>,
    pending_detail: Option<InFlight>,
    pending_source: Option<InFlight>,
    pending_whoami: Option<InFlight>,
    pending_signin: Option<InFlight>,
    /// Detail pane state. `selected` is the (user, slug) the user
    /// clicked; `detail` is the loaded ModelDetail when ready.
    selected: Option<(String, String)>,
    detail: Option<ModelDetail>,
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
        // frame would see `discover=None, pending=None` and kick again,
        // hammering MoGHub as fast as connections fail).
        if self.community.discover.is_none()
            && self.community.pending_discover.is_none()
            && self.community.error.is_none()
        {
            self.kick_discover(ctx);
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
            .default_width(560.0)
            .default_height(640.0)
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

    /// Render the auth chip at the top of the Community window.
    /// Signed-out: a "Sign in with GitHub" button kicks the loopback
    /// OAuth flow. Signed-in: shows `@handle` with a Sign out menu.
    fn draw_auth_strip(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if let Some(user) = self.community.me.clone() {
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
                self.kick_discover(ctx);
            }
            if ui.button("Reset").clicked() {
                self.community.search.clear();
                self.kick_discover(ctx);
            }
        });
        ui.separator();
        if let Some(err) = &self.community.error {
            ui.colored_label(egui::Color32::LIGHT_RED, err);
            ui.separator();
        }
        if self.community.pending_discover.is_some() {
            ui.label("loading…");
            return;
        }
        let Some(discover) = self.community.discover.clone() else {
            return;
        };
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if let Some(featured) = discover.featured.as_ref() {
                    ui.heading("Featured");
                    self.draw_summary_row(ui, featured, ctx);
                    ui.add_space(8.0);
                }
                ui.heading(format!("Discover ({} items)", discover.items.len()));
                for item in &discover.items {
                    self.draw_summary_row(ui, item, ctx);
                }
            });
    }

    fn draw_summary_row(
        &mut self,
        ui: &mut egui::Ui,
        item: &ModelSummary,
        ctx: &egui::Context,
    ) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                let title = format!("@{}/{} — {}", item.user.handle, item.slug, item.title);
                if ui.link(title).clicked() {
                    self.kick_detail(ctx, item.user.handle.clone(), item.slug.clone());
                }
                if !item.description.is_empty() {
                    ui.weak(item.description.lines().next().unwrap_or(""));
                }
                ui.horizontal(|ui| {
                    ui.weak(format!("{} ♥ · {} forks", item.like_count, item.fork_count));
                    if !item.tags.is_empty() {
                        ui.weak(format!("tags: {}", item.tags.join(", ")));
                    }
                });
            });
        });
        ui.separator();
    }

    fn draw_detail(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if ui.button("← back").clicked() {
            self.community.view = View::Discover;
            self.community.detail = None;
            self.community.selected = None;
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
        ui.heading(format!(
            "@{}/{} — {}",
            detail.user.handle, detail.slug, detail.title
        ));
        ui.weak(format!(
            "v{} · {} ♥ · {} forks · license {}",
            detail.version.version, detail.like_count, detail.fork_count, detail.license
        ));
        if !detail.description.is_empty() {
            ui.label(detail.description);
        }
        if !detail.tags.is_empty() {
            ui.weak(format!("tags: {}", detail.tags.join(", ")));
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
        let entry = detail
            .version
            .files
            .iter()
            .find(|f| f.is_entry)
            .or_else(|| detail.version.files.first());
        let entry_filename = entry.map(|f| f.filename.clone());
        let user_handle = detail.user.handle.clone();
        let slug = detail.slug.clone();
        if let Some(filename) = entry_filename {
            if ui
                .add_enabled(
                    self.community.pending_source.is_none(),
                    egui::Button::new("Open in editor"),
                )
                .clicked()
            {
                self.kick_open_in_editor(ctx, user_handle, slug, filename);
            }
        }
        if self.community.pending_source.is_some() {
            ui.weak("loading source…");
        }
    }

    // --- worker dispatch + polling -----------------------------------

    fn kick_discover(&mut self, ctx: &egui::Context) {
        let q = if self.community.search.is_empty() {
            DiscoverQuery::default()
        } else {
            DiscoverQuery {
                q: Some(self.community.search.clone()),
                ..Default::default()
            }
        };
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_discover = Some(fetch_discover(url, token, ctx.clone(), q));
        self.community.error = None;
    }

    fn kick_detail(&mut self, ctx: &egui::Context, user: String, slug: String) {
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_detail = Some(fetch_model_detail(
            url,
            token,
            ctx.clone(),
            user.clone(),
            slug.clone(),
        ));
        self.community.selected = Some((user, slug));
        self.community.detail = None;
        self.community.view = View::Detail;
    }

    fn kick_open_in_editor(
        &mut self,
        ctx: &egui::Context,
        user: String,
        slug: String,
        filename: String,
    ) {
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_source = Some(fetch_file_source(
            url, token, ctx.clone(), user, slug, filename,
        ));
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

    /// Clear the persisted token + cached `me`. Best-effort save —
    /// failure to persist leaves the in-memory state signed-out, which
    /// is the conservative choice (next launch will reload from disk
    /// and re-sign-in if the file write actually failed).
    fn sign_out_moghub(&mut self) {
        self.settings.moghub_session.clear();
        let _ = self.settings.save();
        self.community.me = None;
        self.community.auth_error = None;
    }

    fn poll_community_workers(&mut self, ctx: &egui::Context) {
        // Discover.
        if let Some(inflight) = &self.community.pending_discover {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_discover = None;
                if let MoghubMessage::Discover(result) = msg {
                    match result {
                        Ok(d) => self.community.discover = Some(d),
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
        // FileSource → spawn a fresh untitled tab.
        if let Some(inflight) = &self.community.pending_source {
            if let Some(msg) = inflight.try_recv() {
                self.community.pending_source = None;
                if let MoghubMessage::FileSource {
                    user,
                    slug,
                    filename,
                    result,
                } = msg
                {
                    match result {
                        Ok(body) => {
                            let label = format!("{}@{}-{}", user, slug, filename);
                            self.open_fetched_in_new_tab(label, body);
                            // Close the Community window once we've
                            // handed off to a tab — keeps the focus on
                            // the editor. Preserve `me` so the chip
                            // doesn't flip to signed-out on close.
                            let me = self.community.me.take();
                            self.community = CommunityState {
                                me,
                                ..CommunityState::default()
                            };
                        }
                        Err(e) => self.community.error = Some(format_err(&e)),
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
                            self.settings.moghub_session.clear();
                            let _ = self.settings.save();
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
                            self.settings.moghub_session = token;
                            if let Err(e) = self.settings.save() {
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
    }

    /// Open a fetched MoGHub source as a fresh untitled tab. The user
    /// can save with Save As to land it on disk.
    fn open_fetched_in_new_tab(&mut self, label: String, source: String) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut tab = FileState::untitled(id);
        tab.source = source.clone();
        tab.last_saved_source = source;
        tab.dirty = true;
        tab.needs_compile = true;
        tab.status = format!("from MoGHub: {label}");
        self.files.push(tab);
        self.active = self.files.len() - 1;
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
        MoghubError::Unauthorized => "sign-in required (P2)".to_string(),
        MoghubError::Decode(s) => format!("decode error: {s}"),
    }
}
