//! Centralised drain of every Community worker channel. Called once
//! per frame from `community_window` and again from
//! `draw_moghub_status_chip` so the bell + chip stay live even when the
//! Community window is closed.

use mogen_moghub_client::{CommentList, MoghubError};

use crate::app::moghub::MoghubMessage;
use crate::app::MogenStudioApp;

use super::state::ImageState;
use super::util::{decode_image, format_err};

impl MogenStudioApp {
    pub(super) fn poll_community_workers(&mut self, ctx: &egui::Context) {
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
                        Ok(w) => {
                            self.community.me = w.user;
                            // moghub returns 200 + `user: None` for an
                            // unknown session (not 401). Without this,
                            // a stale token would loop the kick-gate
                            // forever — me stays None, token stays set.
                            if self.community.me.is_none()
                                && !self.settings.moghub_session.is_empty()
                            {
                                let _ = self.settings.clear_moghub_session();
                            }
                        }
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
}
