use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use eframe::egui;
use mogen_export::ExportOptions;

use crate::settings::Settings;
use crate::splash;
use crate::theme::apply_theme;
use crate::viewer::Viewer;

mod ask;
mod autocomplete;
mod build;
mod compile;
mod crash_consent;
mod editor_link;
mod error_class;
mod file_picker;
mod files;
mod find;
mod generate;
mod indent;
mod line_ops;
mod llm;
mod moghub;
mod moghub_open;
mod multi_caret;
mod oauth_ui;
mod onboarding;
mod pricing;
mod publish_textures;
mod spotlight;
mod text_menu;
mod thumbnail;
mod types;
mod ui_dialogs;
mod ui_docs;
mod ui_llm;
mod ui_menu;
mod ui_panels;
mod undo;
mod update;
mod util;
mod viewport_menu;
mod watcher;

use self::types::{
    viewer_bg_color, AskInFlight, AutocompleteState, BuildOutcome, DocsState, EnhanceInFlight,
    EnhanceTarget, ExternalConflict, FileState, FindState, GenImageInput, SessionUsage,
    SpotlightState, ThumbCache,
};
use self::util::locate_project_root;

/// One queued unit of deferred startup work. Each step runs on a separate
/// frame so the splash screen has a chance to advance the progress bar before
/// the next item; `OpenTab` is the slow one (file read + parse + viewer
/// rebuild via `compile_active`), `RestoreRecent` and `ActivateRestored` are
/// trivial and exist mainly to round out the progress count and bookend the
/// init sequence.
enum InitStep {
    OpenTab(PathBuf),
    /// Reapply the saved recent-files list. `open_path` clobbers it on each
    /// call (every open pushes the path to the front), so the original list
    /// has to be written back once the tab strip is rebuilt.
    RestoreRecent(Vec<String>),
    /// Activate the tab whose path matches the saved `last_opened`. Resolves
    /// the index lazily because `open_path` may have collapsed pristine tabs.
    ActivateRestored(PathBuf),
    /// Legacy single-file restore path for users whose settings predate the
    /// `open_tabs` field. Skipped when the saved tab list is non-empty.
    LegacyRestoreLast,
}

/// Loader state machine. Held on the app while startup work is in flight so
/// the splash screen can show real progress (and a stage caption) instead of
/// a fixed-duration animation. `splash_tex` is uploaded on the first frame
/// from inside `update`, since `eframe::CreationContext` doesn't expose a
/// texture loader before the GL renderer is fully online.
struct InitProgress {
    queue: VecDeque<InitStep>,
    /// Total step count when we kicked off, used as the progress denominator.
    /// Doesn't shrink when items are pulled — `done` does the bookkeeping.
    total: usize,
    done: usize,
    /// Caption shown above the progress bar. Updated *before* the matching
    /// step runs so the user sees "opening foo.mog" while the file is being
    /// parsed, not after.
    label: String,
    splash_tex: Option<egui::TextureHandle>,
    /// Wall-clock time the splash first painted. Used to enforce a minimum
    /// dwell so a near-instant launch (no tabs to restore) doesn't flash the
    /// splash for a single frame.
    started_at: Option<Instant>,
}

/// Floor on splash visibility (ms). Shorter and the splash flashes; longer
/// and it feels like the app is dragging. Tuned to feel deliberate without
/// holding up users on cold launch with no tabs to restore.
const SPLASH_MIN_DWELL_MS: u128 = 1800;

/// Maximum recently-closed paths held for Ctrl+Shift+T reopen. Matches the
/// settings `recent_files` cap so the two lists feel comparable; older
/// entries fall off the back of the deque when this is exceeded.
const RECENTLY_CLOSED_CAP: usize = 12;

pub struct MogenStudioApp {
    files: Vec<FileState>,
    active: usize,

    /// Monotonic counter handed out as `FileState::tab_id`. Each tab needs a
    /// stable identity for the editor's `egui::Id`; reusing indices across
    /// close/open would mean a fresh tab inheriting the closed tab's
    /// TextEditState (cursor + undo history) from egui memory.
    next_tab_id: u64,

    project_root: PathBuf,

    settings: Settings,
    show_options: bool,
    options_api_key_draft: String,
    /// Draft string for the optional seed field in the New Prompt dialog.
    /// Stored as a `String` so the user can type freely without partial
    /// parses; we re-parse on submit. Preferences → LLM has its own seed
    /// editor that reads/writes `settings.seed_override` directly.
    options_seed_draft: String,
    /// Active tab inside the Preferences window. Persists across opens within
    /// a session so re-opening the dialog returns to whichever pane the user
    /// was last on.
    prefs_active_tab: ui_dialogs::PrefsTab,

    /// First-launch onboarding visibility. Raised once after the splash drains
    /// when `settings.onboarded` is false; the user dismisses it by pasting a
    /// key + Get Started, or by Skip. Either path latches `onboarded = true`
    /// so the modal never reopens on this install.
    show_onboarding: bool,
    /// First-launch privacy prompt for crash reporting. Raised when
    /// `settings.crash_reports_enabled` is `None` (and env vars don't already
    /// hard-disable telemetry). Suppressed once the user picks Allow / Decline;
    /// the saved choice gates `crash::init` on subsequent launches.
    show_crash_consent: bool,
    /// Draft API key edited inside the onboarding modal. Kept separate from
    /// `options_api_key_draft` so closing one dialog never leaks state into
    /// the other.
    onboarding_api_key_draft: String,

    /// "New from Prompt…" modal visibility and its prompt draft. The LLM
    /// generator only surfaces here — it is no longer part of the inspector.
    show_new_prompt: bool,
    new_prompt_draft: String,
    /// Optional image staged inside the New-from-Prompt modal (image-to-3D).
    /// Moved into the new tab's `FileState.gen_image` on submit; cleared when
    /// the dialog closes. None until the user picks a file.
    new_prompt_image: Option<GenImageInput>,
    /// Latched when the modal is opened so the dialog can grab focus on its
    /// first frame; cleared after the focus request fires.
    new_prompt_focus_pending: bool,

    /// "Unsaved changes" modal shown when a window-close is requested while
    /// any buffer is dirty. `confirmed_quit` latches once the user picks
    /// Discard (or saves everything) so the re-issued close request passes
    /// through without re-prompting.
    show_quit_confirm: bool,
    confirmed_quit: bool,

    /// Per-tab close confirmation. Set when the user tries to close a dirty
    /// tab (menu, Ctrl+W, or the X in the tab strip); the index is held here
    /// until the user picks Save / Discard / Cancel.
    pending_close_index: Option<usize>,

    /// Stack of paths for tabs the user has just closed, newest at the back.
    /// Drives Ctrl+Shift+T (reopen the last closed tab). Bounded by
    /// `RECENTLY_CLOSED_CAP` so an editing session that closes hundreds of
    /// tabs doesn't grow this unbounded; oldest entries fall off when the cap
    /// is hit. Untitled tabs are not pushed — there's nothing on disk to
    /// re-open.
    recently_closed: VecDeque<PathBuf>,

    /// Last frame's 3D viewport rect. Used by the gizmo hotkey gate so W/E/R
    /// only fire when the mouse is hovering the viewport — matches the
    /// "shortcuts follow cursor" convention every 3D DCC uses (Blender,
    /// Maya, Unity, Unreal). `None` until the first paint completes.
    last_viewport_rect: Option<egui::Rect>,

    /// Help → About modal visibility.
    show_about: bool,

    /// View → Community window state. Holds the discover feed, active
    /// detail, search box, and any in-flight HTTP workers. Reset to
    /// `default()` when the window closes so the next open re-fetches.
    community: ui_dialogs::community::CommunityState,

    /// Help → Check for Updates modal visibility. Independent of
    /// `update_state` so the user can close the modal mid-install and the
    /// worker keeps running in the background.
    show_update: bool,
    /// Self-updater state machine. `None` means nothing has been kicked off
    /// (or the user closed the dialog while idle); a `Some(_)` survives modal
    /// closes when an install is in flight so the worker isn't orphaned.
    update_state: Option<self::update::UpdateState>,

    /// Help → Documentation window visibility. Toggled by F1 / the Help
    /// menu / a Ctrl+click on a keyword in the editor. The window is
    /// non-modal so users can keep editing while reading docs side-by-side.
    show_docs: bool,
    /// Persistent docs viewer state — current page, pending scroll target,
    /// navigation history, sidebar filter. Survives close/reopen so the
    /// user lands back where they left off.
    docs: DocsState,

    /// "Build GLB" modal visibility. Also acts as the ui gate on the export
    /// toggles while a build is in flight — the worker writes status through
    /// `build_stage` and posts its result on `build_rx`.
    show_export: bool,
    /// Draft toggles edited in the modal. Copied from the active file on
    /// open, written back on Build. Keeps the per-file state clean if the
    /// user cancels out.
    export_opts_draft: ExportOptions,
    /// Channel carrying the finished build back to the UI thread. Present
    /// means a build is in flight (or at least, the UI hasn't dropped it).
    build_rx: Option<Receiver<BuildOutcome>>,
    /// Channel carrying the OAuth login outcome back to the UI thread.
    /// `Some` while the loopback server is open and waiting for the user to
    /// finish the browser flow. Polled every frame; the result is folded into
    /// `oauth_status_message` and the saved bundle.
    oauth_login_rx: Option<Receiver<Result<mogen_llm::OAuthBundle, mogen_llm::OAuthError>>>,
    /// Which OAuth provider config the in-flight login is for. Pinned when
    /// `start_oauth_login` spawns the worker so `poll_oauth_login` knows
    /// which token file (`google_auth.json` vs `antigravity_auth.json`) to
    /// write the resulting bundle to. `None` when no login is in flight.
    oauth_login_provider: Option<&'static mogen_llm::google_oauth::ProviderConfig>,
    /// Last-known short status line for the Gemini OAuth section in
    /// Preferences. Replaces the inline lookup so the UI can flash messages
    /// like "logged in as …" or "login failed: …" between flow attempts
    /// without re-reading the on-disk bundle every frame.
    oauth_status_message: Option<String>,
    /// Current stage label written by the worker ("merging sibling meshes",
    /// "writing glb", …). Read every frame to drive the modal status line.
    /// Shared because the worker thread updates it mid-build.
    build_stage: Arc<Mutex<String>>,

    viewer: Viewer,

    /// Computed once: the system instruction grows with stdlib + grammar
    /// and is shared by every text-LLM call.
    system_instruction_cache: Option<Arc<String>>,

    /// `(path, mtime)` -> exists, with last-checked timestamp. Avoids stat'ing
    /// every texture path on every frame.
    tex_exists_cache: HashMap<PathBuf, (Option<SystemTime>, bool, Instant)>,

    /// Running total of Gemini tokens / calls and estimated USD cost this
    /// session. Displayed in the status footer; cleared by the user from the
    /// LLM inspector section.
    session_usage: SessionUsage,

    /// egui-uploaded thumbnails for generated texture PNGs, keyed by absolute
    /// path. Populated lazily when a path first appears in the inspector and
    /// invalidated when the file's mtime changes (so re-running textures
    /// refreshes the preview).
    thumb_cache: ThumbCache,

    /// Single in-flight prompt-enhance call. `None` when idle. Held at the
    /// app level (not per-file) so the Generate modal draft and the
    /// per-file Modify / Animate / Style fields can share the same worker
    /// slot — only one enhance ever runs at a time, which keeps the UX
    /// and the token bill predictable.
    enhance_in_flight: Option<EnhanceInFlight>,
    /// Most recent enhance failure message tagged by target, surfaced inline
    /// next to the relevant Enhance button until the user clicks away or
    /// kicks off another enhance. `None` when the last call succeeded or
    /// nothing has been tried yet.
    enhance_error: Option<(EnhanceTarget, String)>,

    /// "Ask MoGen" modal state. The user opens it from the editor context
    /// menu; the snippet they had selected (or the whole file) is captured
    /// once at open time so later edits to the editor don't change what the
    /// model is asked about.
    show_ask: bool,
    ask_question_draft: String,
    ask_code_context: String,
    /// Short human description of what's being asked about ("selected (12
    /// lines)" / "entire file (45 lines)") shown above the question field.
    ask_context_label: String,
    /// Latched when the modal is opened so the dialog can grab focus on its
    /// first frame; cleared after the focus request fires.
    ask_focus_pending: bool,
    /// Single in-flight Ask call. `None` when idle. Held at the app level
    /// because the modal is global.
    ask_in_flight: Option<AskInFlight>,
    /// Last Ask result. Kept after the call returns so the user can read /
    /// re-read it until they close the modal or submit a fresh question.
    ask_answer: Option<Result<String, String>>,

    /// Editor autocomplete popup state. One instance since only one editor is
    /// on-screen at a time; it's reset on tab switch / focus loss.
    autocomplete: AutocompleteState,

    /// Find / search bar state for the code editor. App-level since only one
    /// editor is visible at a time; the bar persists across tab switches and
    /// recomputes matches against whichever file is active.
    find: FindState,

    /// Ctrl+P spotlight palette state — query, selected index, focus latch.
    /// Always-app-level because it's a global modal; closes itself on Esc /
    /// Enter / outside-click.
    spotlight: SpotlightState,

    /// Pending on-disk conflict awaiting user resolution. Set by the file
    /// watcher when an open file changed on disk and the buffer is dirty
    /// (clean buffers reload silently — see `watcher.rs`). Cleared when the
    /// modal is dismissed.
    pending_external: Option<ExternalConflict>,

    /// Deferred startup state. `Some(_)` while the splash screen is still
    /// running and queued init steps remain; cleared the frame the queue
    /// drains so the regular UI takes over without flicker. The whole
    /// loading dance lives behind this option so steady-state code paths
    /// don't pay any branch cost once startup is done.
    init: Option<InitProgress>,

    /// Set when a video render is mid-flight: the GL worker is producing
    /// frame PNGs and we're waiting for it to report back so we can hand
    /// them to ffmpeg. None when no video is in progress.
    pending_video: Option<self::generate::PendingVideo>,

    /// In-flight ffmpeg encode after the GL frames landed on disk. Carries
    /// the receiver and the cleanup paths we need on completion.
    video_encode: Option<self::generate::VideoEncode>,

    /// Set while an LLM auto-refine iteration's screenshot capture is
    /// pumping through the GL worker. Carries the file index the
    /// capture was submitted for so the outcome can be routed back to
    /// that file even if the user has switched tabs in the meantime.
    /// `None` when no refine capture is in flight.
    pending_refine_capture: Option<self::llm::refine::PendingRefineCapture>,

    /// "Render MP4" options modal visibility. Opened from the File menu /
    /// spotlight before kicking the actual render off so the user can pick
    /// resolution and camera mode.
    show_video_options: bool,
    /// Draft options edited in the modal. Persisted across opens so the user's
    /// last choice is the next default; resets to `Default` only on app start.
    video_opts_draft: self::generate::VideoOptions,

    /// Inspector "link scale axes" toggle. When on, dragging one axis of the
    /// Scale row scales the other two by the same ratio so the node keeps its
    /// proportions. Defaults to on — uniform scale is the common case and the
    /// only safe default for nodes whose mesh was authored at 1× across the
    /// board.
    inspector_scale_linked: bool,

    /// Custom file picker modal state. `Some(_)` while the picker is open;
    /// cleared by confirm or cancel. Replaces the native `rfd` dialog for
    /// Open / Import / Save As so the body can render thumbnail previews.
    pub(in crate::app) picker: Option<file_picker::FilePickerState>,

    /// Background thumbnail engine that produces the rendered previews
    /// shown in the picker grid. Independent of the file-picker lifetime
    /// because the worker threads + on-disk PNG cache outlive any single
    /// picker session.
    pub(in crate::app) thumbnail_mgr: thumbnail::ThumbnailManager,

    /// Camera snapshot captured the moment the picker opens. The thumbnail
    /// engine swaps the live viewer's scene under us for each render —
    /// this lets us put the user's camera framing back when the picker
    /// closes. Scene restore goes through `refresh_viewer_from_active`
    /// since the active file already owns the source-of-truth scene Arc.
    pub(in crate::app) picker_prev_camera:
        Option<crate::viewer::CameraSnapshot>,

    /// Pending / in-flight `mogen://` action. Set by
    /// [`Self::queue_protocol_url`] when launched with a URL on argv,
    /// or by future in-app deep-link surfaces. `None` at steady state.
    moghub_protocol: Option<moghub_open::Flow>,
}

impl MogenStudioApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let gl = cc
            .gl
            .as_ref()
            .expect("eframe was built with the glow backend, so cc.gl is Some");
        let viewer = Viewer::new(gl).expect("failed to init 3D viewer");

        let project_root = locate_project_root();

        let mut settings = Settings::load();
        let options_api_key_draft = settings.gemini_api_key.clone();
        let options_seed_draft = settings
            .seed_override
            .map(|s| s.to_string())
            .unwrap_or_default();
        install_fonts(&cc.egui_ctx);
        apply_theme(&cc.egui_ctx, settings.theme());
        viewer.set_preview_shader(settings.preview_shader());
        viewer.set_show_grid(settings.show_grid());
        viewer.set_show_light_gizmos(settings.show_light_gizmos());
        viewer.set_show_transform_gizmo(settings.show_transform_gizmo());
        viewer.set_show_colliders(settings.show_colliders());
        viewer.set_environment(settings.environment());
        viewer.set_shadows(settings.shadow_quality());
        viewer.set_max_fps(settings.max_fps());

        // Hand the seed tab id 0 directly; the counter that hands out
        // subsequent ids starts at 1 so it never collides.
        let mut initial = FileState::untitled(0);
        initial.status = "welcome — open a MOG file to get started".into();

        // Build the deferred startup queue. open_path is the slow part of
        // restoring a session (file I/O + parse + viewer recompile per tab),
        // so each tab opens on its own frame and the splash bar advances in
        // between. open_path also clobbers `recent_files`, so snapshot it
        // here and queue a `RestoreRecent` step to put it back at the end.
        let saved_open_tabs = settings.open_tabs.clone();
        let saved_active = settings.last_opened.clone();
        let saved_recent = settings.recent_files.clone();

        let mut queue: VecDeque<InitStep> = VecDeque::new();
        if !saved_open_tabs.is_empty() {
            for path_str in &saved_open_tabs {
                let p = PathBuf::from(path_str);
                if !p.is_file() {
                    continue;
                }
                queue.push_back(InitStep::OpenTab(p));
            }
            queue.push_back(InitStep::RestoreRecent(saved_recent));
            if let Some(active) = saved_active {
                let p = PathBuf::from(active);
                if p.is_file() {
                    queue.push_back(InitStep::ActivateRestored(p));
                }
            }
        } else {
            queue.push_back(InitStep::LegacyRestoreLast);
        }

        let total = queue.len().max(1);
        let init = InitProgress {
            queue,
            total,
            done: 0,
            label: "starting MoGen Studio…".into(),
            splash_tex: None,
            started_at: None,
        };

        // Show the welcome flow exactly once per install. Skip it for users
        // upgrading from a settings file that predates the `onboarded` field
        // but already has a saved key — they've clearly walked through
        // Preferences themselves and don't need the orientation pass.
        if !settings.onboarded && !settings.gemini_api_key.trim().is_empty() {
            settings.onboarded = true;
            let _ = settings.save();
        }
        let show_onboarding = !settings.onboarded;

        // First-launch privacy prompt: only when the user has never been
        // asked AND the env doesn't already opt them out (in which case the
        // prompt would be cosmetic — `crash::init` is already a no-op).
        let show_crash_consent = settings.crash_reports_enabled.is_none()
            && !crate::crash::telemetry_blocked_by_env();

        Self {
            files: vec![initial],
            active: 0,
            next_tab_id: 1,
            project_root,
            settings,
            show_options: false,
            options_api_key_draft,
            options_seed_draft,
            prefs_active_tab: ui_dialogs::PrefsTab::default(),
            show_onboarding,
            show_crash_consent,
            onboarding_api_key_draft: String::new(),
            show_new_prompt: false,
            new_prompt_draft: String::new(),
            new_prompt_image: None,
            new_prompt_focus_pending: false,
            show_quit_confirm: false,
            confirmed_quit: false,
            pending_close_index: None,
            recently_closed: VecDeque::new(),
            last_viewport_rect: None,
            show_about: false,
            community: ui_dialogs::community::CommunityState::default(),
            show_update: false,
            update_state: None,
            show_docs: false,
            docs: DocsState::default(),
            show_export: false,
            export_opts_draft: ExportOptions::default(),
            build_rx: None,
            build_stage: Arc::new(Mutex::new(String::new())),
            oauth_login_rx: None,
            oauth_login_provider: None,
            oauth_status_message: None,
            viewer,
            system_instruction_cache: None,
            tex_exists_cache: HashMap::new(),
            session_usage: SessionUsage::default(),
            thumb_cache: ThumbCache::new(),
            enhance_in_flight: None,
            enhance_error: None,
            show_ask: false,
            ask_question_draft: String::new(),
            ask_code_context: String::new(),
            ask_context_label: String::new(),
            ask_focus_pending: false,
            ask_in_flight: None,
            ask_answer: None,
            autocomplete: AutocompleteState::default(),
            find: FindState::default(),
            spotlight: SpotlightState::default(),
            pending_external: None,
            init: Some(init),
            pending_video: None,
            pending_refine_capture: None,
            video_encode: None,
            show_video_options: false,
            video_opts_draft: self::generate::VideoOptions::default(),
            inspector_scale_linked: true,
            picker: None,
            thumbnail_mgr: thumbnail::ThumbnailManager::new(cc.egui_ctx.clone()),
            picker_prev_camera: None,
            moghub_protocol: None,
        }
    }

    /// Queue a `mogen://` URL captured from argv. Acted on after the
    /// splash drains so the user isn't ambushed by a folder picker
    /// during the welcome animation.
    pub fn queue_protocol_url(&mut self, url: crate::protocol::MogenUrl) {
        self.moghub_protocol = Some(moghub_open::Flow::Pending(url));
    }

    /// Advance the splash one step. Caller has already confirmed `init` is
    /// `Some` and the queue is non-empty. The label is set *before* running
    /// so the user sees what's happening during the call, not after.
    fn run_one_init_step(&mut self) {
        let Some(init) = self.init.as_mut() else {
            return;
        };
        let Some(step) = init.queue.pop_front() else {
            return;
        };
        match step {
            InitStep::OpenTab(path) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                init.label = format!("opening {name}");
                self.open_path(&path);
            }
            InitStep::RestoreRecent(recent) => {
                if let Some(init) = self.init.as_mut() {
                    init.label = "restoring recent files…".into();
                }
                self.settings.recent_files = recent;
                let _ = self.settings.save();
            }
            InitStep::ActivateRestored(path) => {
                if let Some(init) = self.init.as_mut() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    init.label = format!("activating {name}");
                }
                if let Some(i) = self.file_index_by_path(&path) {
                    self.activate(i);
                }
            }
            InitStep::LegacyRestoreLast => {
                if let Some(init) = self.init.as_mut() {
                    init.label = "loading last session…".into();
                }
                let last = self
                    .settings
                    .recent_files
                    .iter()
                    .map(PathBuf::from)
                    .find(|p| p.is_file())
                    .or_else(|| {
                        self.settings
                            .last_opened
                            .as_ref()
                            .map(PathBuf::from)
                            .filter(|p| p.is_file())
                    });
                if let Some(p) = last {
                    self.open_path(&p);
                }
            }
        }
        if let Some(init) = self.init.as_mut() {
            init.done = init.done.saturating_add(1);
        }
    }

    /// Drive the splash screen for one frame: lazy-upload the splash texture
    /// on first paint, advance one init step (if any), render the splash UI,
    /// and clear `self.init` once the queue is drained and the minimum dwell
    /// has elapsed.
    fn tick_splash(&mut self, ctx: &egui::Context) {
        if let Some(init) = self.init.as_mut() {
            if init.splash_tex.is_none() {
                init.splash_tex = splash::upload(ctx);
            }
            if init.started_at.is_none() {
                init.started_at = Some(Instant::now());
            }
        }

        // One step per frame. This means N tabs takes N frames of splash —
        // at typical egui pacing that's still well under a second per tab,
        // and the bar advances visibly between them.
        let has_step = self
            .init
            .as_ref()
            .map(|i| !i.queue.is_empty())
            .unwrap_or(false);
        if has_step {
            self.run_one_init_step();
        }

        // Snapshot what we need to paint and decide whether to stay loading.
        let (progress, label, tex, dwell_ok, queue_empty) = {
            let Some(init) = self.init.as_ref() else {
                return;
            };
            let progress = if init.total == 0 {
                1.0
            } else {
                (init.done as f32 / init.total as f32).clamp(0.0, 1.0)
            };
            let dwell_ok = init
                .started_at
                .map(|t| t.elapsed().as_millis() >= SPLASH_MIN_DWELL_MS)
                .unwrap_or(false);
            (
                progress,
                init.label.clone(),
                init.splash_tex.clone(),
                dwell_ok,
                init.queue.is_empty(),
            )
        };

        splash::draw(ctx, tex.as_ref(), progress, &label);

        if queue_empty && dwell_ok {
            self.init = None;
        }
    }

    /// Drive the `mogen://` protocol flow one frame: while the splash
    /// is still up we leave the URL pending; once init drains we raise
    /// the folder picker, spawn a download/unzip worker, and on
    /// completion route the entry `.mog` through `open_path`.
    fn drive_moghub_protocol(&mut self, ctx: &egui::Context) {
        if self.init.is_some() {
            return;
        }
        // Take the current state out so we can mutate `self` in match
        // arms without fighting the borrow checker; whatever the arm
        // wants to keep is written back at the end.
        let Some(flow) = self.moghub_protocol.take() else {
            return;
        };
        match flow {
            moghub_open::Flow::Pending(url) => {
                let label = match &url {
                    crate::protocol::MogenUrl::MoghubOpen { user, slug, .. } => {
                        format!("@{user}/{slug}")
                    }
                };
                // Native folder picker. Blocks the UI thread but matches
                // the rest of Studio's Save-As flow; users expect a
                // dialog to take focus.
                let initial = self.project_root.clone();
                let dest_root = rfd::FileDialog::new()
                    .set_title(&format!("Choose folder for {label}"))
                    .set_directory(initial)
                    .pick_folder();
                let Some(dest_root) = dest_root else {
                    // User cancelled — drop the action; status reflects it.
                    self.active_mut().status = format!("cancelled opening {label}");
                    return;
                };
                let base_url = self.settings.moghub_url.clone();
                let token = self.settings.moghub_session.clone();
                let rx = moghub_open::spawn(base_url, token, ctx.clone(), url, dest_root);
                self.active_mut().status = format!("downloading {label}…");
                self.moghub_protocol = Some(moghub_open::Flow::Working {
                    label,
                    rx,
                });
            }
            moghub_open::Flow::Working { label, rx } => {
                match rx.try_recv() {
                    Ok(outcome) => match outcome.result {
                        Ok(entry_path) => {
                            self.open_path(&entry_path);
                            self.active_mut().status = format!("opened {label}");
                            // Flow done — leave moghub_protocol = None.
                        }
                        Err(msg) => {
                            self.active_mut().status =
                                format!("opening {label} failed: {msg}");
                            self.moghub_protocol =
                                Some(moghub_open::Flow::Failed(msg));
                        }
                    },
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        // Still working — put it back.
                        self.moghub_protocol = Some(moghub_open::Flow::Working {
                            label,
                            rx,
                        });
                        ctx.request_repaint_after(std::time::Duration::from_millis(120));
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        let msg = "download worker exited unexpectedly".to_string();
                        self.active_mut().status =
                            format!("opening {label} failed: {msg}");
                        self.moghub_protocol =
                            Some(moghub_open::Flow::Failed(msg));
                    }
                }
            }
            moghub_open::Flow::Failed(msg) => {
                // Sticky until cleared. Keep the state so a future
                // surface can show it (no toast yet); status bar
                // already carries the message.
                self.moghub_protocol = Some(moghub_open::Flow::Failed(msg));
            }
        }
    }

    /// Mint the next stable per-tab id. Every fresh `FileState` should pull
    /// from here so the editor's `egui::Id` never collides with one belonging
    /// to a now-closed tab.
    pub(super) fn next_tab_id(&mut self) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.wrapping_add(1);
        id
    }

    /// `egui::Id` for the active tab's code-editor TextEdit. Salting on the
    /// tab id (not the index) keeps cursor + undo history per-tab and stable
    /// across tab reorders / closes.
    pub(super) fn active_editor_id(&self) -> egui::Id {
        egui::Id::new(("mog_editor_textedit", self.files[self.active].tab_id))
    }
}

impl eframe::App for MogenStudioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // While the splash is up, do exactly one queued init step per frame
        // and paint the splash. This keeps the bar moving without holding the
        // UI thread for the whole startup duration. We also enforce a small
        // minimum dwell so a near-instant launch (no tabs to restore) doesn't
        // flash the splash for a single frame.
        if self.init.is_some() {
            self.tick_splash(ctx);
            if self.init.is_some() {
                ctx.request_repaint();
                return;
            }
        }

        // Intercept window-close (titlebar ×, Alt-F4, Cmd-Q, menu Quit). If
        // any buffer is dirty and we haven't already confirmed, cancel the
        // close and raise the confirmation modal. `confirmed_quit` latches on
        // Discard / successful Save All so the re-issued Close passes through.
        if ctx.input(|i| i.viewport().close_requested()) && !self.confirmed_quit {
            let any_dirty = self.files.iter().any(|f| f.dirty);
            if any_dirty {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.show_quit_confirm = true;
            }
        }

        // Drain LLM / build completions and run any pending compile from
        // the editor's debounce window before painting.
        self.poll_llm(ctx);
        self.poll_prompt_enhance();
        self.poll_ask();
        self.poll_build();
        self.poll_oauth_login();
        self.poll_update();
        self.poll_generate(ctx);
        self.drive_moghub_protocol(ctx);
        self.drive_compile_debounce(ctx);
        self.check_external_changes(ctx);

        // Consume global shortcuts before the menu / editor see the key event,
        // so e.g. Ctrl+S doesn't reach the TextEdit. Spotlight is dispatched
        // first because Cmd+P should reach the palette even when the editor
        // owns focus.
        self.dispatch_spotlight_shortcuts(ctx);
        self.dispatch_shortcuts(ctx);
        self.handle_dropped_files(ctx);

        egui::TopBottomPanel::top("menubar").show(ctx, |ui| self.ui_menu_bar(ui));
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| self.ui_tabs(ui));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.active().status);
                let n = self.count_in_flight();
                if n > 0 {
                    ui.separator();
                    ui.spinner();
                    ui.label(format!(
                        "{n} llm call{} in flight",
                        if n == 1 { "" } else { "s" }
                    ));
                }
                self.ui_session_meter(ui);
                // MoGHub auth chip lives at the right edge of the status
                // bar so it's always visible — not just when the
                // Community window is open. Right-to-left layout pins it
                // there without a fixed-width spacer.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.draw_moghub_status_chip(ui, ctx);
                });
            });
        });
        egui::SidePanel::right("inspector")
            .default_width(340.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // CollapsingHeader so users can fold what they don't
                        // need; keeps the most-actioned section (LLM) reachable
                        // without scrolling past the other groups. Diagnostics
                        // now lives in the footer, not here.
                        egui::CollapsingHeader::new("Selected")
                            .default_open(true)
                            .show(ui, |ui| self.ui_selected(ui));
                        egui::CollapsingHeader::new("Scene")
                            .default_open(true)
                            .show(ui, |ui| self.ui_summary(ui));
                        egui::CollapsingHeader::new("Materials")
                            .default_open(false)
                            .show(ui, |ui| self.ui_materials(ui));
                        if self.has_visible_clips() {
                            egui::CollapsingHeader::new("Animation")
                                .default_open(true)
                                .show(ui, |ui| self.ui_animation(ui));
                        }
                        egui::CollapsingHeader::new("LLM")
                            .default_open(true)
                            .show(ui, |ui| self.ui_llm(ui));
                    });
            });

        // Editor and viewer sit side-by-side: editor on the left (resizable),
        // viewer fills whatever remains in the central panel. The diagnostics
        // footer is nested inside the editor panel so validator output sits
        // directly under the code it refers to — but only appears when the
        // validator has something actionable to report (errors/warnings).
        egui::SidePanel::left("editor_panel")
            .resizable(true)
            .default_width(520.0)
            .min_width(200.0)
            .show(ctx, |ui| {
                // Force the SidePanel's content ui to claim its full available
                // width. Without this, neither the nested CentralPanel below
                // nor the editor's ScrollArea propagates its allocation back
                // to the panel ui's `min_rect` — so `PanelState.rect.width`
                // stores `min_width` (200) regardless of how wide the user
                // dragged the boundary, and on mouse-up the panel snaps back
                // and the 3D viewport never resizes. Previously the
                // always-shown diagnostics `TopBottomPanel` did this work via
                // `ui.expand_to_include_rect` (egui panel.rs:792); now that
                // the diagnostics panel is conditional we have to ensure it
                // ourselves on the no-diagnostics path.
                ui.set_min_width(ui.max_rect().width());
                if self.has_blocking_diagnostics() {
                    egui::TopBottomPanel::bottom("diagnostics")
                        .resizable(true)
                        .default_height(120.0)
                        .min_height(28.0)
                        .show_inside(ui, |ui| {
                            egui::CollapsingHeader::new(self.diagnostics_header_label())
                                .id_salt("footer_diagnostics")
                                .default_open(true)
                                .show(ui, |ui| {
                                    egui::ScrollArea::vertical()
                                        .auto_shrink([false, true])
                                        .max_height(200.0)
                                        .show(ui, |ui| self.ui_diagnostics(ui));
                                });
                        });
                }
                egui::CentralPanel::default().show_inside(ui, |ui| self.ui_editor(ui));
            });

        let mut viewport_rect: Option<egui::Rect> = None;
        let picker_open = self.picker.is_some();
        egui::CentralPanel::default().show(ctx, |ui| {
            // The 3D viewport is deliberately independent of the UI theme —
            // a calibrated neutral charcoal matches the look of every major
            // DCC app (Blender, Maya, 3ds Max, Modo) and keeps the model's
            // colours reading consistently regardless of the panel scheme.
            // Users can still override the colour via View → Background.
            let fill = viewer_bg_color(&self.settings);
            egui::Frame::canvas(ui.style()).fill(fill).show(ui, |ui| {
                if picker_open {
                    // While the picker is open, the thumbnail engine is
                    // swapping the viewer's scene under us for each render
                    // — paint a solid scrim so the user doesn't see the
                    // cycling. The viewer itself still has to paint (its
                    // paint callback drives the capture pipeline), so we
                    // shove it into a 1×1 hidden corner instead of skipping
                    // it. The corner sits inside the panel so input still
                    // goes to the viewer's allocated rect, not the scrim.
                    let avail = ui.available_size();
                    ui.painter().rect_filled(
                        ui.max_rect(),
                        0.0,
                        ui.style().visuals.extreme_bg_color,
                    );
                    ui.allocate_ui(egui::vec2(1.0, 1.0), |ui| {
                        let _ = self.viewer.show(ui);
                    });
                    // Reserve the rest of the space so the panel doesn't
                    // collapse around the 1×1 viewer (would offset the
                    // scrim). `add_space` is enough since this branch
                    // doesn't need to know the viewport rect.
                    ui.allocate_space(egui::vec2(
                        avail.x.max(1.0),
                        (avail.y - 1.0).max(0.0),
                    ));
                } else {
                    let resp = self.viewer.show(ui);
                    viewport_rect = Some(resp.rect);
                    resp.context_menu(|ui| self.ui_viewport_context_menu(ui));
                }
            });
        });
        if let Some(r) = viewport_rect {
            self.ui_viewport_overlay(ctx, r);
        }
        // Cache for the next frame's gizmo hotkey gate — `dispatch_shortcuts`
        // runs at the start of `update`, so it reads last frame's rect rather
        // than this frame's. The rect is stable across frames at steady state
        // so a one-frame lag is invisible. While the picker is hiding the
        // viewport, leave the cached rect untouched so gizmo hotkeys remain
        // gated against the *previous* viewport rect rather than the 1×1
        // worker rect.
        if !picker_open {
            self.last_viewport_rect = viewport_rect;
        }

        // Pick up any gizmo drags / caret jumps produced by the viewport.
        // Must run after `viewer.show` because that's where new drag
        // commits and selections are queued.
        self.drain_viewport_edits(ctx);

        // After the editor has rendered for the active tab, snapshot its
        // camera back into the FileState so future tab switches restore it.
        // Skip while the picker has the viewer scene swapped out for thumb
        // renders — sampling here would persist the thumbnail's framing.
        if !picker_open {
            let snap = self.viewer.camera_snapshot();
            self.files[self.active].camera = Some(snap);
        }

        // Drive the thumbnail pipeline: drain compile + decode results,
        // submit the next render to the live viewer's capture system.
        // Idempotent + cheap when nothing's queued; safe to run every
        // frame regardless of whether the picker is open (a queued render
        // from a previous picker session might still need draining).
        self.thumbnail_mgr.tick(&self.viewer, ctx);
        if picker_open && self.thumbnail_mgr.is_busy() {
            // Force the next frame to repaint so render outcomes get
            // picked up promptly even when no input is arriving.
            ctx.request_repaint();
        }

        // Privacy prompt comes before the welcome flow so the user makes
        // exactly one decision per modal — they're sequenced, not stacked.
        self.ui_crash_consent(ctx);
        self.ui_onboarding(ctx);
        self.ui_options(ctx);
        self.ui_new_prompt(ctx);
        self.ui_quit_confirm(ctx);
        self.ui_close_confirm(ctx);
        self.ui_export_dialog(ctx);
        self.ui_external_conflict(ctx);
        self.ui_about(ctx);
        self.community_window(ctx);
        self.publish_dialog(ctx);
        self.module_palette_dialog(ctx);
        self.ui_update_dialog(ctx);
        self.ui_docs(ctx);
        self.ui_ask(ctx);
        self.ui_spotlight(ctx);
        self.ui_video_options(ctx);
        self.ui_capture_progress(ctx);
        self.ui_file_picker(ctx);

        // Paint the autocomplete popup last so it floats above every panel.
        // The editor panel updated state earlier in the frame; here we just
        // draw what that state says.
        let editor_id = self.active_editor_id();
        self.render_autocomplete_popup(ctx, editor_id);

        // Keep repainting while ANY file has an LLM call in flight so every
        // spinner ticks and completions land promptly regardless of which tab
        // is active. The Build GLB worker is covered by the same heartbeat
        // so its stage label animates without the user having to mouse the
        // window.
        if self.any_in_flight()
            || self.any_enhance_in_flight()
            || self.any_ask_in_flight()
            || self.build_rx.is_some()
            || self.update_in_flight()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        // Capture loop: each paint callback consumes one frame of an
        // in-flight video render, so we have to keep asking egui to repaint
        // until the request drains. Without this, the app paints once after
        // `submit_capture` and then sits idle — the modal stays up but no
        // frames advance. ffmpeg's encode phase already nudges repaints
        // from `poll_generate`, so we only need to cover the GL phase here.
        if self.viewer.is_capturing() {
            ctx.request_repaint();
        }

        // Idle-tick the on-disk watcher: even when nothing else needs a
        // redraw, schedule a repaint within a watch interval so external
        // edits surface promptly. egui collapses overlapping `request_repaint`
        // calls, so this composes harmlessly with the in-flight heartbeat.
        if self.files.iter().any(|f| f.path.is_some()) {
            ctx.request_repaint_after(types::WATCH_INTERVAL);
        }
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Some(gl) = gl {
            self.viewer.destroy(gl);
        }
    }
}

/// Extend the default proportional font family with `Hack` as a fallback.
/// egui 0.29's bundled `Ubuntu-Light` covers Latin/Cyrillic/Greek but not
/// Geometric Shapes (▲ ▼), Arrows (↑ ↓), or Dingbats (✕) — the icon-shaped
/// glyphs UI buttons reach for. `Hack` is already loaded for the monospace
/// family and covers all of those blocks, so adding it as a tail fallback
/// rescues otherwise-missing glyphs without bundling another font.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        if !family.iter().any(|n| n == "Hack") {
            family.push("Hack".to_owned());
        }
    }
    ctx.set_fonts(fonts);
}
