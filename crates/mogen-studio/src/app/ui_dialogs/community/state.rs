//! Top-level state types for the Community window.
//!
//! All state lives on `MogenStudioApp::community` so it survives across
//! frames; the field visibilities below are `pub(super)` so every
//! sibling submodule of `community/` can mutate state directly. `me`
//! and `open` are wider (`pub(crate)`) because `app::ui_menu` reads
//! them when painting the menu.

use std::collections::HashMap;

use mogen_moghub_client::{
    CommentList, DependencyList, ModelDetail, ModelSummary, ModuleSuggestion, NotificationList,
    PublishResponse, UpdatesAvailable, UserSummary,
};

use crate::app::moghub::InFlight;

/// Items per page; matches the moghub server default and stays well under
/// `DISCOVER_MAX_LIMIT = 96`.
pub(super) const PAGE_SIZE: u32 = 24;

/// Edge length (px) of the publish-dialog preview render. Matches the
/// `Generate Thumbnail` menu action so a publish-time preview and a
/// user-driven thumbnail produce visually consistent output.
pub(super) const PUBLISH_THUMB_SIZE: u32 = 512;
/// On-screen size of the preview image inside the publish dialog. Smaller
/// than the rendered PNG so the dialog stays compact; egui scales the
/// 512px source down on the GPU.
pub(super) const PUBLISH_THUMB_PREVIEW_SIZE: f32 = 128.0;

/// Kind filter pills shown above the discover grid. Mirrors the web
/// client's `?kind=` chip set; `All` is the default and sends no `kind`
/// query string at all.
#[derive(Default, Copy, Clone, PartialEq, Eq)]
pub(super) enum KindFilter {
    #[default]
    All,
    Scene,
    Model,
    Module,
}

impl KindFilter {
    pub(super) fn label(self) -> &'static str {
        match self {
            KindFilter::All => "All",
            KindFilter::Scene => "Scenes",
            KindFilter::Model => "Models",
            KindFilter::Module => "Modules",
        }
    }

    pub(super) fn query_value(self) -> Option<&'static str> {
        match self {
            KindFilter::All => None,
            KindFilter::Scene => Some("scene"),
            KindFilter::Model => Some("model"),
            KindFilter::Module => Some("module"),
        }
    }
}

pub(super) const KIND_FILTERS: [KindFilter; 4] = [
    KindFilter::All,
    KindFilter::Scene,
    KindFilter::Model,
    KindFilter::Module,
];

/// One slot in the URL → texture cache. Loading is the in-flight state;
/// Failed is sticky so we don't retry forever on a broken avatar URL.
pub(super) enum ImageState {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

#[derive(Default)]
pub(super) enum View {
    #[default]
    Discover,
    Detail,
}

/// Top-level state for the Community window. Lives on `MogenStudioApp`
/// so it survives across frames; reset to `default()` on close — except
/// for the auth fields, which the app refreshes from `Settings` on
/// reopen so the chip survives close/reopen.
#[derive(Default)]
pub(crate) struct CommunityState {
    pub(crate) open: bool,
    /// What's showing in the right pane.
    pub(super) view: View,
    /// Search box buffer. Empty = "front page" (no `?q=`).
    pub(super) search: String,
    /// Currently selected kind pill.
    pub(super) kind: KindFilter,
    /// Featured hero from the most recent first-page fetch. Stays put
    /// across "Load more" calls so the hero doesn't flicker.
    pub(super) featured: Option<ModelSummary>,
    /// Items accumulated across paginated calls. The first page replaces
    /// this; subsequent "Load more" calls append.
    pub(super) items: Vec<ModelSummary>,
    /// Offset for the next "Load more" call.
    pub(super) next_offset: u32,
    /// Server returned a full page last time — assume more to come.
    pub(super) has_more: bool,
    /// True while a "Load more" append is in flight (vs. a fresh search
    /// that replaces). The button label flips accordingly.
    pub(super) appending: bool,
    /// True once the first discover call returns. Used to distinguish
    /// "still loading" from "loaded, zero results".
    pub(super) loaded_once: bool,
    /// Last error string, surfaced as a banner above the grid.
    pub(super) error: Option<String>,
    /// Active in-flight calls. We hold onto them so the channel stays
    /// open; on completion they transition into one of the other fields
    /// and the entry is dropped.
    pub(super) pending_discover: Option<InFlight>,
    pub(super) pending_detail: Option<InFlight>,
    pub(super) pending_deps: Option<InFlight>,
    pub(super) pending_updates: Option<InFlight>,
    pub(super) pending_comments: Option<InFlight>,
    pub(super) pending_post_comment: Option<InFlight>,
    pub(super) pending_like: Option<InFlight>,
    pub(super) pending_whoami: Option<InFlight>,
    pub(super) pending_signin: Option<InFlight>,
    pub(super) pending_notifications: Option<InFlight>,
    pub(super) pending_notifications_read: Option<InFlight>,
    /// One in-flight `InFlight` per image URL we've kicked. The map's
    /// keys mirror `image_cache` so we don't double-kick.
    pub(super) pending_images: HashMap<String, InFlight>,
    /// URL → texture cache. `Failed` is sticky.
    pub(super) image_cache: HashMap<String, ImageState>,
    /// Detail pane state. `selected` is the (user, slug) the user
    /// clicked; `detail` is the loaded ModelDetail when ready.
    pub(super) selected: Option<(String, String)>,
    pub(super) detail: Option<ModelDetail>,
    /// Outbound + inbound graph for the currently-open model.
    pub(super) deps: Option<DependencyList>,
    /// Outdated pins for the currently-open model.
    pub(super) updates: Option<UpdatesAvailable>,
    /// Loaded comments for the currently-open model.
    pub(super) comments: Option<CommentList>,
    /// Bbcode the user is composing in the comment box.
    pub(super) comment_draft: String,
    /// Notifications inbox + unread count. Populated lazily once `me`
    /// is set; clicking the bell pops a dropdown.
    pub(super) notifications: Option<NotificationList>,
    /// State for the publish dialog. `None` = closed.
    pub(super) publish: Option<PublishDialogState>,
    /// Pending POST /api/models.
    pub(super) pending_publish: Option<InFlight>,
    /// State for the module palette (Cmd+Shift+M). `None` = closed.
    pub(super) module_palette: Option<ModulePaletteState>,
    /// Pending GET /api/registry/suggest. The worker echoes the query
    /// so stale results (typed-past) can be dropped.
    pub(super) pending_module_suggest: Option<InFlight>,
    /// Currently signed-in user. Populated by `whoami` on app start
    /// (when the persisted session is still valid) and after the
    /// loopback OAuth flow completes. `None` = signed-out.
    pub(crate) me: Option<UserSummary>,
    /// Last sign-in flow error surfaced as a banner above the chip.
    pub(super) auth_error: Option<String>,
}

/// Module palette state. Searches `/api/registry/suggest` as the user
/// types and inserts the picked module's `use "@user/slug@v"` line at
/// the end of the active file's source. End-of-file insertion (rather
/// than at-cursor) avoids poking the TextEdit's cursor state from
/// outside the editor panel — predictable, and a `use` statement
/// sitting after the rest of the file is what `mogen-validate` accepts
/// regardless of order.
pub(super) struct ModulePaletteState {
    /// Current search input.
    pub(super) query: String,
    /// Most recent successful suggestion list, keyed by the query that
    /// produced it (so the UI can drop stale results).
    pub(super) results: Vec<ModuleSuggestion>,
    pub(super) results_for: String,
    /// Surface a fetch error inline; doesn't clear `results` so the
    /// user keeps seeing the last good list while diagnosing.
    pub(super) error: Option<String>,
    /// Set on first paint so the search box autofocuses.
    pub(super) needs_focus: bool,
}

/// Publish dialog state. Seeded when the user opens the dialog from
/// the Community menu; `source` is a snapshot of the active tab so
/// edits made in the editor while the dialog is open don't sneak into
/// the published version.
pub(super) struct PublishDialogState {
    /// Editable. Defaults to the source's `meta(name=…)` on dialog
    /// open; the user can edit before submitting (the meta block isn't
    /// rewritten — this is a publish-time override).
    pub(super) title: String,
    /// Editable. Defaults to `meta(description=…)`.
    pub(super) description: String,
    /// Editable comma-separated list. Defaults to `meta(tags=[…])`
    /// joined with `, `.
    pub(super) tags_input: String,
    pub(super) license: String,
    pub(super) visibility: String,
    pub(super) publish_message: String,
    pub(super) publish_as_module: bool,
    /// Filename to send for the entry file. Defaults to the active
    /// tab's `display_name`, falling back to `scene.mog` for untitled
    /// buffers.
    pub(super) filename: String,
    /// Snapshot of the active tab's source at the moment the dialog
    /// opened.
    pub(super) source: String,
    /// Disk location of the entry file at the moment the dialog opened.
    /// Used to resolve `import "..."` siblings for multi-file publish.
    /// `None` for untitled buffers — their imports (if any) can't be
    /// bundled, so the publish proceeds with the entry file alone.
    pub(super) entry_path: Option<std::path::PathBuf>,
    /// Last error from the server, surfaced inline.
    pub(super) error: Option<String>,
    /// Successful publish response — the dialog flips to a "published!"
    /// state with a "Open in browser" button when this is set.
    pub(super) success: Option<PublishResponse>,
    /// Temp file the publish-thumbnail capture writes to. Set when the
    /// capture is queued and cleared after the bytes have been read back
    /// (success) or the capture failed. Removed from disk in either case.
    pub(super) thumbnail_temp: Option<std::path::PathBuf>,
    /// PNG bytes of the captured preview, ready to base64-encode into the
    /// publish request. `None` while the capture is still pending.
    pub(super) thumbnail_bytes: Option<Vec<u8>>,
    /// Decoded preview texture for in-dialog display. Uploaded once after
    /// the capture lands so we don't re-decode every frame.
    pub(super) thumbnail_texture: Option<egui::TextureHandle>,
    /// Capture-pipeline error, if the GL worker reported one. Shown next
    /// to the preview so the user knows the upload will go without one.
    pub(super) thumbnail_error: Option<String>,
    /// MoGHub model that this file was last published to, lifted from the
    /// `meta(moghub_model_id=…, moghub_slug=…, moghub_version=…)` stamp
    /// applied on a previous successful publish. When `Some` and the user
    /// hasn't toggled `publish_as_new`, the request carries
    /// `target_model_id` so the server appends a new version instead of
    /// allocating a fresh slug.
    pub(super) update_target: Option<UpdateTarget>,
    /// User wants to abandon the stamped identity and create a brand-new
    /// MoGHub model from this source. Surfaced as a checkbox in the
    /// dialog when `update_target` is set.
    pub(super) publish_as_new: bool,
}

/// What we know about the prior publish of the file currently open in
/// the Publish dialog. Read from the source's `meta(...)` block; the
/// fields are kept together so the UI can render a single
/// "Updating @user/slug → v(N+1)" caption without juggling three
/// optionals.
#[derive(Clone)]
pub(super) struct UpdateTarget {
    /// UUID identifying the MoGHub model. Sent verbatim as
    /// `target_model_id` in the publish request.
    pub(super) model_id: String,
    /// Slug last seen in the URL path. Used for the dialog caption and
    /// re-stamped on success in case the server ever permits slug
    /// changes (today it doesn't).
    pub(super) slug: String,
    /// Last published version number. The next publish will create
    /// `last_version + 1`.
    pub(super) last_version: i32,
}
