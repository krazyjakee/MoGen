//! Model detail pane: header, like button, dependency graph,
//! comments list + post form, "Open in editor" hand-off.

use mogen_moghub_client::ModelDetail;

use crate::app::moghub::{
    fetch_comments, fetch_deps, fetch_model_detail, fetch_updates, post_comment, toggle_like,
};
use crate::app::MogenStudioApp;

use super::state::{CommunityState, View};
use super::util::{kind_badge, strip_bbcode};

impl MogenStudioApp {
    pub(super) fn draw_detail(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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

    pub(super) fn kick_detail(&mut self, ctx: &egui::Context, user: String, slug: String) {
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
}
