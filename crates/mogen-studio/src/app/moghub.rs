//! HTTP worker that runs MoGHub calls off the egui main loop.
//!
//! Mirrors `app/llm.rs`: the UI snapshots the request inputs, spawns a
//! `std::thread`, and the worker posts results through an `mpsc` channel.
//! Each frame the app polls every active receiver via [`poll`] and
//! transitions any completed call's state. No tokio, no async fn —
//! `reqwest::blocking` is fine for the volumes Studio sends.

use std::sync::mpsc::{self, Receiver};
use std::thread;

use mogen_moghub_client::{
    Comment, CommentList, DependencyList, DiscoverQuery, DiscoverResponse, LikeResponse,
    ModelDetail, ModuleSuggestion, MoghubClient, MoghubError, NotificationList, PublishRequest,
    PublishResponse, UpdatesAvailable, WhoAmI,
};

/// Outcome posted back to the UI when a call completes. One variant per
/// supported call kind. Add cases here as the Community window grows
/// (publish, comments, notifications).
pub(super) enum MoghubMessage {
    Discover(Result<DiscoverResponse, MoghubError>),
    /// `(user, slug)` echoed back so the UI can route the result to the
    /// detail panel even if the user has clicked elsewhere by the time
    /// it arrives.
    ModelDetail {
        user: String,
        slug: String,
        result: Result<ModelDetail, MoghubError>,
    },
    /// `GET /api/whoami` result. Used after a fresh sign-in to populate
    /// the session chip and on app start to validate a stored token.
    WhoAmI(Result<WhoAmI, MoghubError>),
    /// Loopback OAuth completed. `Ok(uuid)` is the session token to
    /// store; `Err` is the textual reason the flow failed (browser
    /// closed, listener died, server returned an error).
    SignedIn(Result<String, String>),
    /// Fetched bytes for an image URL (thumbnail or avatar). The
    /// receiver is keyed by URL on the polling side, so the message
    /// itself only needs to carry the result.
    Image(Result<Vec<u8>, MoghubError>),
    /// `(user, slug)` echoed back so a stale result (the user clicked
    /// a different model since) can be dropped on the floor.
    Deps {
        user: String,
        slug: String,
        result: Result<DependencyList, MoghubError>,
    },
    /// `GET /api/m/:user/:slug/updates` — outdated-pin data for the
    /// currently-open detail.
    Updates {
        user: String,
        slug: String,
        result: Result<UpdatesAvailable, MoghubError>,
    },
    Comments {
        user: String,
        slug: String,
        result: Result<CommentList, MoghubError>,
    },
    /// Result of a like-toggle. The boolean records whether the call
    /// was a POST (true) or a DELETE (false) so the UI can revert
    /// optimistic state on error.
    Like {
        user: String,
        slug: String,
        was_liking: bool,
        result: Result<LikeResponse, MoghubError>,
    },
    PostedComment {
        user: String,
        slug: String,
        result: Result<Comment, MoghubError>,
    },
    Notifications(Result<NotificationList, MoghubError>),
    /// `POST /api/notifications` — mark-all-read result. Carries a
    /// fresh `NotificationList` so the dropdown reflects the
    /// server-canonical state immediately.
    NotificationsRead(Result<NotificationList, MoghubError>),
    Published(Result<PublishResponse, MoghubError>),
    /// `(query echoed back, suggestions)` — the query is echoed so the
    /// palette can drop stale results after the user has typed past
    /// them.
    ModuleSuggestions {
        query: String,
        result: Result<Vec<ModuleSuggestion>, MoghubError>,
    },
}

/// Async handle for one in-flight call. Drop it to abandon the receiver
/// (the worker thread keeps running but nothing reads its result — fine,
/// reqwest::blocking has bounded resource use).
pub(super) struct InFlight {
    pub(super) rx: Receiver<MoghubMessage>,
}

impl InFlight {
    /// Try to drain a single completed message. `None` while the worker
    /// is still in flight; `Some(Err(_))` only if the channel closed
    /// unexpectedly (worker panicked).
    pub(super) fn try_recv(&self) -> Option<MoghubMessage> {
        match self.rx.try_recv() {
            Ok(m) => Some(m),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }
}

/// Build a fresh client for a single call. The HTTP `Client` itself is
/// cheap (it shares a connection pool internally), but constructing a
/// new one per call avoids any state we'd need to thread through the
/// worker. `base_url` comes from `Settings::moghub_url`. `token` is the
/// persisted session UUID (empty when signed-out); when present it
/// rides on every request as `Authorization: Bearer <uuid>`.
fn build_client(base_url: &str, token: &str) -> Result<MoghubClient, MoghubError> {
    let client = MoghubClient::new(base_url)?;
    if token.is_empty() {
        Ok(client)
    } else {
        Ok(client.with_token(Some(token.to_string())))
    }
}

/// Spawn a `GET /api/discover` worker. Cheap-to-construct so the UI can
/// fire a fresh one every time the user changes search/filter.
pub(super) fn fetch_discover(
    base_url: String,
    token: String,
    ctx: egui::Context,
    query: DiscoverQuery,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.discover(query));
        let _ = tx.send(MoghubMessage::Discover(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/m/:user/:slug` worker.
pub(super) fn fetch_model_detail(
    base_url: String,
    token: String,
    ctx: egui::Context,
    user: String,
    slug: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let user_for_msg = user.clone();
    let slug_for_msg = slug.clone();
    thread::spawn(move || {
        let result =
            build_client(&base_url, &token).and_then(|c| c.model_detail(&user, &slug));
        let _ = tx.send(MoghubMessage::ModelDetail {
            user: user_for_msg,
            slug: slug_for_msg,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/m/:user/:slug/deps` worker.
pub(super) fn fetch_deps(
    base_url: String,
    token: String,
    ctx: egui::Context,
    user: String,
    slug: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let user_for_msg = user.clone();
    let slug_for_msg = slug.clone();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.deps(&user, &slug));
        let _ = tx.send(MoghubMessage::Deps {
            user: user_for_msg,
            slug: slug_for_msg,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/m/:user/:slug/updates` worker.
pub(super) fn fetch_updates(
    base_url: String,
    token: String,
    ctx: egui::Context,
    user: String,
    slug: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let user_for_msg = user.clone();
    let slug_for_msg = slug.clone();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.updates(&user, &slug));
        let _ = tx.send(MoghubMessage::Updates {
            user: user_for_msg,
            slug: slug_for_msg,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/m/:user/:slug/comments` worker.
pub(super) fn fetch_comments(
    base_url: String,
    token: String,
    ctx: egui::Context,
    user: String,
    slug: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let user_for_msg = user.clone();
    let slug_for_msg = slug.clone();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.comments(&user, &slug));
        let _ = tx.send(MoghubMessage::Comments {
            user: user_for_msg,
            slug: slug_for_msg,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a like-toggle worker. `liking = true` issues a POST; `false`
/// issues a DELETE. The variant flag is echoed back on the message so
/// the UI can revert optimistic state on error.
pub(super) fn toggle_like(
    base_url: String,
    token: String,
    ctx: egui::Context,
    user: String,
    slug: String,
    liking: bool,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let user_for_msg = user.clone();
    let slug_for_msg = slug.clone();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| {
            if liking {
                c.like(&user, &slug)
            } else {
                c.unlike(&user, &slug)
            }
        });
        let _ = tx.send(MoghubMessage::Like {
            user: user_for_msg,
            slug: slug_for_msg,
            was_liking: liking,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `POST /api/m/:user/:slug/comments` worker. Body is bbcode;
/// the server renders to safe HTML and echoes both back.
pub(super) fn post_comment(
    base_url: String,
    token: String,
    ctx: egui::Context,
    user: String,
    slug: String,
    body: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let user_for_msg = user.clone();
    let slug_for_msg = slug.clone();
    thread::spawn(move || {
        let result =
            build_client(&base_url, &token).and_then(|c| c.post_comment(&user, &slug, &body));
        let _ = tx.send(MoghubMessage::PostedComment {
            user: user_for_msg,
            slug: slug_for_msg,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/notifications` worker.
pub(super) fn fetch_notifications(
    base_url: String,
    token: String,
    ctx: egui::Context,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.notifications());
        let _ = tx.send(MoghubMessage::Notifications(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `POST /api/notifications` mark-all-read worker.
pub(super) fn mark_notifications_read(
    base_url: String,
    token: String,
    ctx: egui::Context,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.mark_notifications_read());
        let _ = tx.send(MoghubMessage::NotificationsRead(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/registry/suggest?q=…` worker.
pub(super) fn fetch_module_suggestions(
    base_url: String,
    token: String,
    ctx: egui::Context,
    query: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let echo = query.clone();
    thread::spawn(move || {
        let result =
            build_client(&base_url, &token).and_then(|c| c.registry_suggest(&query, Some(20)));
        let _ = tx.send(MoghubMessage::ModuleSuggestions {
            query: echo,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `POST /api/models` worker.
pub(super) fn publish_model(
    base_url: String,
    token: String,
    ctx: egui::Context,
    request: PublishRequest,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.publish(&request));
        let _ = tx.send(MoghubMessage::Published(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a worker that pulls image bytes from `url` (either absolute,
/// e.g. a GitHub avatar CDN, or a moghub-relative path like
/// `/api/m/.../thumbnail.png`). The result is keyed by the original URL
/// so the UI can route the bytes back to the right cache slot.
pub(super) fn fetch_image(
    base_url: String,
    token: String,
    ctx: egui::Context,
    url: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.fetch_image_bytes(&url));
        let _ = tx.send(MoghubMessage::Image(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/whoami` worker. Used on startup to validate a
/// stored session and after a fresh sign-in to populate the chip.
pub(super) fn fetch_whoami(base_url: String, token: String, ctx: egui::Context) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_client(&base_url, &token).and_then(|c| c.whoami());
        let _ = tx.send(MoghubMessage::WhoAmI(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn the loopback OAuth flow. Binds a one-shot HTTP listener on
/// `127.0.0.1:0` (OS-assigned port), opens the user's default browser
/// at `<base_url>/api/auth/desktop/start?return=…&nonce=…`, then waits
/// up to [`OAUTH_TIMEOUT`] for GitHub's callback to redirect back to
/// `http://127.0.0.1:<port>/callback?session=<uuid>&nonce=<echoed>`.
/// On success [`MoghubMessage::SignedIn`] carries the session UUID; on
/// failure it carries a human-readable reason.
pub(super) fn start_signin(base_url: String, ctx: egui::Context) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = mogen_moghub_client::oauth::run_loopback_flow(&base_url);
        let _ = tx.send(MoghubMessage::SignedIn(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}
