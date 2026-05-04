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
    DiscoverQuery, DiscoverResponse, ModelDetail, MoghubClient, MoghubError,
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
    /// Returned source of a single file. `(user, slug, filename, body)`.
    /// Used by "Open in editor" — the body lands in a new untitled tab.
    FileSource {
        user: String,
        slug: String,
        filename: String,
        result: Result<String, MoghubError>,
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
/// worker. `base_url` comes from `Settings::moghub_url`.
fn build_client(base_url: &str) -> Result<MoghubClient, MoghubError> {
    MoghubClient::new(base_url)
}

/// Spawn a `GET /api/discover` worker. Cheap-to-construct so the UI can
/// fire a fresh one every time the user changes search/filter.
pub(super) fn fetch_discover(
    base_url: String,
    ctx: egui::Context,
    query: DiscoverQuery,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_client(&base_url).and_then(|c| c.discover(query));
        let _ = tx.send(MoghubMessage::Discover(result));
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/m/:user/:slug` worker.
pub(super) fn fetch_model_detail(
    base_url: String,
    ctx: egui::Context,
    user: String,
    slug: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let user_for_msg = user.clone();
    let slug_for_msg = slug.clone();
    thread::spawn(move || {
        let result = build_client(&base_url).and_then(|c| c.model_detail(&user, &slug));
        let _ = tx.send(MoghubMessage::ModelDetail {
            user: user_for_msg,
            slug: slug_for_msg,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}

/// Spawn a `GET /api/m/:user/:slug/files/:filename` worker. Used by the
/// "Open in editor" action to load a published source into a fresh tab.
pub(super) fn fetch_file_source(
    base_url: String,
    ctx: egui::Context,
    user: String,
    slug: String,
    filename: String,
) -> InFlight {
    let (tx, rx) = mpsc::channel();
    let user_for_msg = user.clone();
    let slug_for_msg = slug.clone();
    let filename_for_msg = filename.clone();
    thread::spawn(move || {
        let result =
            build_client(&base_url).and_then(|c| c.file_raw(&user, &slug, &filename));
        let _ = tx.send(MoghubMessage::FileSource {
            user: user_for_msg,
            slug: slug_for_msg,
            filename: filename_for_msg,
            result,
        });
        ctx.request_repaint();
    });
    InFlight { rx }
}
