use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use mogen_core::Diagnostic;
use mogen_export::ExportOptions;
use mogen_llm::gemini::{ThinkingLevel, Usage};
use mogen_llm::textures::{TextureStage, DEFAULT_TEXTURE_SIZE};

use crate::pipeline::CompileResult;
use crate::viewer::CameraSnapshot;

/// Debounce window before a keystroke triggers a recompile. Long enough that
/// holding a key (or pasting) doesn't recompile mid-word; short enough that
/// the diagnostics panel feels live.
pub(super) const COMPILE_DEBOUNCE: Duration = Duration::from_millis(180);

/// TTL on the texture-existence cache. Texture roster runs every paint and
/// would otherwise stat every PNG every frame.
pub(super) const TEX_EXISTS_TTL: Duration = Duration::from_millis(1500);

/// Throttle on the on-disk file watcher. Each open tab's path is stat'd at
/// most once per this interval. Long enough that a quick burst of saves from
/// an external editor doesn't thrash, short enough to feel live.
pub(super) const WATCH_INTERVAL: Duration = Duration::from_millis(1500);

/// GitHub source URL shown in Help → GitHub repository.
pub(super) const GITHUB_REPO_URL: &str = "https://github.com/krazyjakee/model-gen";
/// Documentation URL (rendered DSL docs on GitHub).
pub(super) const DOCS_URL: &str = "https://github.com/krazyjakee/model-gen/blob/master/docs/dsl.md";
/// License file URL, rendered on GitHub for convenience.
pub(super) const LICENSE_URL: &str = "https://github.com/krazyjakee/model-gen/blob/master/LICENSE";

/// Fixed background colour for the 3D viewport. Deliberately independent of
/// the UI theme so the model's colours read consistently no matter which
/// panel scheme the user picked. Neutral-dark, matching the default look of
/// Blender / Maya / Modo.
pub(super) const VIEWER_BG_COLOR: egui::Color32 = egui::Color32::from_rgb(54, 58, 64);

/// Result from a background LLM call. Always includes the DSL we tried to
/// compile so the UI can drop it into the editor even when validation failed.
pub(super) struct LlmOutcome {
    pub(super) dsl: String,
    pub(super) diagnostics: Vec<Diagnostic>,
    /// Raw API usage — `prompt_tokens + response_tokens + cached_tokens` across
    /// every call the worker made for this run (repair iters counted). Zero
    /// when the outcome is a texture run (image API doesn't report usage today)
    /// or when the call failed before the first round-trip.
    pub(super) usage: Usage,
    pub(super) calls: u32,
    /// The model name the worker actually used. Drives pricing lookup and the
    /// session meter — it's the source of truth, not the Settings field, in
    /// case the user changed the setting mid-run.
    pub(super) model: String,
    /// Count of image-gen calls issued during this outcome (0 for text).
    /// Priced flat per-image since the image API doesn't report token usage.
    pub(super) image_calls: u32,
    /// If set, a prompt the user can Retry without re-typing. Populated for
    /// every kind; on retry the caller re-submits the stored prompt.
    pub(super) retry_prompt: Option<String>,
    pub(super) error: Option<LlmErrorInfo>,
    pub(super) kind: LlmKind,
}

/// Either a mid-flight progress update or the final completion. Sent on one
/// channel so the UI can update the status line while a call is in progress
/// and still swap in the outcome atomically when it arrives.
pub(super) enum LlmMessage {
    Progress(LlmProgress),
    Done(LlmOutcome),
}

/// Max number of timeline entries kept per file. Enough to show the last
/// repair round or a handful of texture steps without taking over the panel.
pub(super) const LLM_EVENT_CAP: usize = 6;

/// One entry in the progress card's timeline. `accent` classifies the event
/// for colouring — Info (neutral), Repair (warn), Texture (kind-accent),
/// so readers can scan the history at a glance without reading every line.
#[derive(Clone)]
pub(super) struct LlmEvent {
    pub(super) at: Instant,
    pub(super) text: String,
    pub(super) tone: LlmEventTone,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LlmEventTone {
    Info,
    Repair,
    Texture,
    Done,
}

/// Stage-level progress emitted by a running LLM worker. Each kind (generate /
/// modify / animate / textures) emits the variants that apply to it.
#[derive(Clone)]
pub(super) enum LlmProgress {
    /// Free-form stage label ("calling Gemini…", "validating DSL…", "done").
    Status(String),
    /// A repair iteration completed with `errors` validation errors; the
    /// worker is about to re-call the model.
    Repair { iter: u32, max: u32, errors: usize },
    /// Texture pipeline stage for material `material`.
    Texture {
        current: u32,
        total: u32,
        material: String,
        stage: TextureStage,
    },
}

/// A user-facing error with enough structure that the UI can render an
/// actionable message and offer a class-specific affordance ("Open Settings",
/// "Retry", …) instead of dumping `e.to_string()`.
#[derive(Debug, Clone)]
pub(super) struct LlmErrorInfo {
    pub(super) headline: String,
    pub(super) detail: String,
    pub(super) class: LlmErrorClass,
    /// Whether re-submitting the exact same prompt has any chance of working.
    /// `false` for missing/invalid key, `true` for rate-limit / transient
    /// network. Drives whether the Retry button is enabled.
    pub(super) retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LlmErrorClass {
    MissingKey,
    InvalidKey,
    RateLimited,
    QuotaExceeded,
    ContentBlocked,
    Network,
    ServerError,
    BadRequest,
    Other,
}

/// Running aggregate of every Gemini call made this app session, plus an
/// estimated USD cost. Reset with the "Clear session" button in the footer.
#[derive(Default, Clone)]
pub(super) struct SessionUsage {
    pub(super) prompt_tokens: u64,
    pub(super) response_tokens: u64,
    pub(super) cached_tokens: u64,
    pub(super) text_calls: u32,
    pub(super) image_calls: u32,
    pub(super) estimated_usd: f64,
}

impl SessionUsage {
    pub(super) fn add_text(&mut self, usage: &Usage, calls: u32, cost: f64) {
        self.prompt_tokens += usage.prompt_tokens as u64;
        self.response_tokens += usage.response_tokens as u64;
        self.cached_tokens += usage.cached_tokens as u64;
        self.text_calls += calls;
        self.estimated_usd += cost;
    }

    pub(super) fn add_image(&mut self, calls: u32, cost: f64) {
        self.image_calls += calls;
        self.estimated_usd += cost;
    }
}

/// Cached thumbnail handle keyed by absolute texture path. `mtime` lets us
/// invalidate when the generator rewrites the same file and `handle` is the
/// egui-uploaded texture ready for `Image::new`.
pub(super) struct ThumbEntry {
    pub(super) handle: egui::TextureHandle,
    pub(super) mtime: Option<std::time::SystemTime>,
}

pub(super) type ThumbCache = HashMap<PathBuf, ThumbEntry>;

/// External-change kind detected by the on-disk watcher.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ExternalChangeKind {
    /// File still exists but its mtime no longer matches the value captured
    /// when we last loaded or saved it.
    Modified,
    /// File no longer exists at the recorded path. Could be a delete, a move,
    /// or an editor that wrote with `create` semantics and lost permissions.
    Deleted,
}

/// Pending external-change conflict awaiting user resolution. Set by the
/// watcher when an open file's on-disk content diverges from the buffer and
/// the buffer is dirty (clean buffers reload silently, no modal). Only one
/// conflict is shown at a time; the next is picked up on the next watch tick.
pub(super) struct ExternalConflict {
    pub(super) file_index: usize,
    pub(super) kind: ExternalChangeKind,
    /// Disk contents read at detection time. `None` for `Deleted`. We capture
    /// here rather than re-reading on resolve so the user resolves against
    /// exactly what they were prompted about, even if the file changes again
    /// while the modal is open.
    pub(super) disk_source: Option<String>,
    /// Disk mtime captured alongside `disk_source`. Reapplied to the
    /// `FileState` on resolve so we don't immediately re-prompt.
    pub(super) disk_mtime: Option<SystemTime>,
}

/// Result from a background GLB build. The exported scene is carried back so
/// the viewer can show exactly what hit disk — important when the merge
/// transform produced a different topology to what the editor is showing.
pub(super) struct BuildOutcome {
    pub(super) file_index: usize,
    pub(super) path: PathBuf,
    /// The scene as actually exported, after merge / strip passes. Used to
    /// refresh the 3D viewer so users see the exact output. `None` when the
    /// build failed before reaching a writable scene.
    pub(super) exported_scene: Option<mogen_core::SceneGraph>,
    pub(super) bytes: Option<u64>,
    pub(super) error: Option<String>,
}

/// Menu-bar intent captured while building the menu UI. Deferred so that
/// actions run after the menu closes, avoiding self-borrow tangles with the
/// `&self.recent_files` / `&self.files` iterators inside the menu closures.
pub(super) enum MenuAction {
    None,
    NewUntitled,
    OpenNewPromptModal,
    OpenDialog,
    OpenPath(PathBuf),
    ClearRecent,
    Save,
    SaveAs,
    Build,
    Recheck,
    CloseActive,
    Quit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    OpenOptions,
    Frame,
    OpenAbout,
}

/// The subset of `MenuAction` variants that are bound to a global keyboard
/// shortcut. Kept as its own enum (rather than impling `Copy` on `MenuAction`)
/// because `MenuAction::OpenPath` carries a `PathBuf` and isn't `Copy`.
#[derive(Clone, Copy)]
pub(super) enum ShortcutAction {
    NewUntitled,
    OpenNewPromptModal,
    OpenDialog,
    Save,
    SaveAs,
    Build,
    Recheck,
    CloseActive,
    Quit,
    OpenOptions,
    Frame,
}

impl ShortcutAction {
    pub(super) fn shortcut(self) -> egui::KeyboardShortcut {
        use egui::{Key, KeyboardShortcut, Modifiers};
        // `COMMAND` is Cmd on macOS, Ctrl elsewhere — single binding works on
        // every platform without branching.
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        match self {
            ShortcutAction::NewUntitled => KeyboardShortcut::new(cmd, Key::N),
            ShortcutAction::OpenNewPromptModal => KeyboardShortcut::new(cmd_shift, Key::N),
            ShortcutAction::OpenDialog => KeyboardShortcut::new(cmd, Key::O),
            ShortcutAction::Save => KeyboardShortcut::new(cmd, Key::S),
            ShortcutAction::SaveAs => KeyboardShortcut::new(cmd_shift, Key::S),
            ShortcutAction::Build => KeyboardShortcut::new(cmd, Key::B),
            ShortcutAction::Recheck => KeyboardShortcut::new(Modifiers::NONE, Key::F5),
            ShortcutAction::CloseActive => KeyboardShortcut::new(cmd, Key::W),
            ShortcutAction::Quit => KeyboardShortcut::new(cmd, Key::Q),
            ShortcutAction::OpenOptions => KeyboardShortcut::new(cmd, Key::Comma),
            ShortcutAction::Frame => KeyboardShortcut::new(cmd, Key::Num0),
        }
    }

    pub(super) fn into_menu(self) -> MenuAction {
        match self {
            ShortcutAction::NewUntitled => MenuAction::NewUntitled,
            ShortcutAction::OpenNewPromptModal => MenuAction::OpenNewPromptModal,
            ShortcutAction::OpenDialog => MenuAction::OpenDialog,
            ShortcutAction::Save => MenuAction::Save,
            ShortcutAction::SaveAs => MenuAction::SaveAs,
            ShortcutAction::Build => MenuAction::Build,
            ShortcutAction::Recheck => MenuAction::Recheck,
            ShortcutAction::CloseActive => MenuAction::CloseActive,
            ShortcutAction::Quit => MenuAction::Quit,
            ShortcutAction::OpenOptions => MenuAction::OpenOptions,
            ShortcutAction::Frame => MenuAction::Frame,
        }
    }

    /// Every shortcut action; iteration order doesn't matter — `dispatch` is
    /// short-circuiting at the first match.
    pub(super) const ALL: &'static [ShortcutAction] = &[
        ShortcutAction::NewUntitled,
        ShortcutAction::OpenNewPromptModal,
        ShortcutAction::OpenDialog,
        ShortcutAction::Save,
        ShortcutAction::SaveAs,
        ShortcutAction::Build,
        ShortcutAction::Recheck,
        ShortcutAction::CloseActive,
        ShortcutAction::Quit,
        ShortcutAction::OpenOptions,
        ShortcutAction::Frame,
    ];
}

/// Render a menu item as a button with the keyboard shortcut shown
/// right-aligned in the menu and appended to the hover tooltip. The shortcut
/// itself is dispatched globally in `dispatch_shortcuts` — this just makes
/// the binding discoverable from the menu and on hover.
pub(super) fn shortcut_menu_item(
    ui: &mut egui::Ui,
    label: &str,
    action: ShortcutAction,
    tooltip: &str,
) -> egui::Response {
    let sc = action.shortcut();
    let sc_text = ui.ctx().format_shortcut(&sc);
    let resp = ui.add(egui::Button::new(label).shortcut_text(sc_text.clone()));
    if tooltip.is_empty() {
        resp.on_hover_text(sc_text)
    } else {
        resp.on_hover_text(format!("{tooltip}  ({sc_text})"))
    }
}

/// Render a menu item with a shortcut label that is NOT dispatched globally —
/// the binding is handled natively by egui's `TextEdit` (undo/redo/cut/copy/
/// paste/select-all). Clicking the menu item injects the corresponding event,
/// so the action still works even when the user reaches for the menu instead
/// of the keyboard.
pub(super) fn native_shortcut_menu_item(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: egui::KeyboardShortcut,
    tooltip: &str,
    enabled: bool,
) -> egui::Response {
    let sc_text = ui.ctx().format_shortcut(&shortcut);
    let resp = ui.add_enabled(
        enabled,
        egui::Button::new(label).shortcut_text(sc_text.clone()),
    );
    if tooltip.is_empty() {
        resp.on_hover_text(sc_text)
    } else {
        resp.on_hover_text(format!("{tooltip}  ({sc_text})"))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LlmKind {
    Generate,
    Modify,
    Animate,
    Repair,
    Textures,
}

impl LlmKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            LlmKind::Generate => "generate",
            LlmKind::Modify => "modify",
            LlmKind::Animate => "animate",
            LlmKind::Repair => "repair",
            LlmKind::Textures => "textures",
        }
    }
}

/// Which prompt field the enhance action should rewrite and replace in place.
/// Drives both the system-instruction template (different intents need very
/// different rewrites) and the routing of the result back into the correct
/// buffer when the call returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EnhanceTarget {
    /// The `New from Prompt` modal draft (`new_prompt_draft` on the app).
    Generate,
    /// The active file's `mod_prompt` in the inspector.
    Modify,
    /// The active file's `anim_prompt` in the inspector.
    Animate,
    /// The active file's `texture_cfg.style` in the Texture options grid.
    TextureStyle,
}

/// Per-session in-flight enhance call. A single slot on the app keeps the UI
/// simple — clicking Enhance on any of the four prompt inputs while one is
/// already running disables the other buttons until it returns. Replacement
/// happens in `poll_prompt_enhance` using `target` + `file_index` to route
/// the result into the right buffer.
pub(super) struct EnhanceInFlight {
    pub(super) target: EnhanceTarget,
    /// File index that owned the target field when the call started. For
    /// `Generate` this is the file that was active at click time (unused for
    /// routing since the draft lives on the app), kept only so tab closures
    /// mid-call can't panic.
    pub(super) file_index: usize,
    pub(super) rx: Receiver<Result<String, String>>,
}

/// User-tweakable knobs on the texture pipeline. Mirrors the CLI's
/// `mogen textures` flags so the GUI is not silently more restrictive.
#[derive(Clone)]
pub(super) struct TextureUiConfig {
    pub(super) style: String,
    pub(super) texture_size: u32,
    pub(super) no_normal: bool,
    pub(super) no_metallic_roughness: bool,
    pub(super) no_occlusion: bool,
    pub(super) force: bool,
    /// Whether the "Advanced" expander is open. Persisted per-file so users
    /// can leave it open on the file they're iterating on.
    pub(super) expanded: bool,
}

impl Default for TextureUiConfig {
    fn default() -> Self {
        Self {
            style: "photorealistic".to_string(),
            texture_size: DEFAULT_TEXTURE_SIZE,
            no_normal: false,
            no_metallic_roughness: false,
            no_occlusion: false,
            force: false,
            expanded: false,
        }
    }
}

/// Transient state for the editor autocomplete popup. One instance lives on
/// the app since only one editor is visible at a time — switching tabs or
/// losing focus closes the popup, so there's no need to persist per-file.
#[derive(Default)]
pub(super) struct AutocompleteState {
    pub(super) open: bool,
    pub(super) selected: usize,
    pub(super) candidates: Vec<crate::autocomplete::Candidate>,
    /// Byte range in the active file's source that the selected candidate
    /// replaces on accept.
    pub(super) range: Option<std::ops::Range<usize>>,
    /// Screen-space anchor point (below-left of the caret) for the popup.
    pub(super) anchor: Option<egui::Pos2>,
    /// When the user hits Esc we want the popup to stay closed even though
    /// the caret is still in an identifier. Cleared the next time the source
    /// changes (so typing another letter re-opens it).
    pub(super) suppressed_for_source_len: Option<usize>,
}

/// Deferred action decoded from a popup key press. Applied after the TextEdit
/// has rendered so we can mutate source/state without fighting the widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AutocompleteKey {
    None,
    MoveUp,
    MoveDown,
    Accept,
    Cancel,
}

/// Per-file state. Every open `.mog` owns its own buffer, compile result,
/// prompts, and in-flight LLM job, so switching files while Gemini is running
/// does not clobber the other file — you can generate on several models at
/// once.
pub(super) struct FileState {
    /// Stable per-tab identifier minted from the app's monotonic counter.
    /// Used as the salt for the editor's `egui::Id` so each tab's TextEdit
    /// owns its own cursor / undo history — without this, egui memory keyed
    /// on a shared id would let one tab's typing pollute another's undo
    /// stack.
    pub(super) tab_id: u64,
    pub(super) path: Option<PathBuf>,
    pub(super) source: String,
    pub(super) last_saved_source: String,
    pub(super) dirty: bool,
    pub(super) last_result: Option<CompileResult>,

    /// Mtime of the on-disk file as of the last load or save. Compared by the
    /// watcher each tick to detect external edits. `None` for untitled tabs,
    /// for files that don't yet exist on disk, or when the platform refuses
    /// to give us a modified timestamp (treated as "watching disabled" rather
    /// than as a sentinel — we won't prompt without a baseline).
    pub(super) disk_mtime: Option<SystemTime>,
    /// Last instant the watcher checked this file's mtime. Throttled by
    /// `WATCH_INTERVAL` so we don't `stat()` on every paint.
    pub(super) last_watch_check: Option<Instant>,

    pub(super) gen_prompt: String,
    pub(super) mod_prompt: String,
    pub(super) anim_prompt: String,
    pub(super) texture_cfg: TextureUiConfig,

    /// Per-file export toggles, remembered so the Build GLB modal starts with
    /// the user's last pick for *this* scene (merge + texture settings are
    /// often scene-specific).
    pub(super) export_opts: ExportOptions,

    /// Per-file override for the Gemini thinking budget. `None` means "use the
    /// global default from Options"; `Some(level)` wins over that default for
    /// every LLM call on this file and is persisted into the `.mog` header.
    /// Seeded from `parse_thinking_header` when a file is loaded.
    pub(super) thinking_override: Option<ThinkingLevel>,

    pub(super) llm_rx: Option<Receiver<LlmMessage>>,
    pub(super) llm_in_flight: Option<LlmKind>,
    /// Most recent progress event from the worker. Drives the spinner caption
    /// so users can see "Repair 1/2 — 3 errors" instead of a flat spinner.
    pub(super) llm_progress: Option<LlmProgress>,
    /// Wall-clock time the current LLM call started. Drives the elapsed-time
    /// counter in the progress card.
    pub(super) llm_started_at: Option<Instant>,
    /// Recent progress events for the active call, oldest → newest. Capped at
    /// `LLM_EVENT_CAP` so a long texture run doesn't grow unbounded.
    pub(super) llm_events: Vec<LlmEvent>,
    /// Structured error from the most recent call, if it failed. Kept after
    /// the call returns so the user can read the headline and hit Retry.
    pub(super) llm_error: Option<LlmErrorInfo>,
    /// Last prompt the user submitted to the LLM. Preserved on failure so the
    /// Retry button doesn't force them to re-type anything.
    pub(super) llm_last_prompt: Option<(LlmKind, String)>,

    /// Captured camera so switching tabs doesn't snap the user's framing.
    /// Restored on `activate` when present, refreshed every frame for the
    /// active tab.
    pub(super) camera: Option<CameraSnapshot>,

    /// Wall-clock time of the last edit. Drives the compile debounce so the
    /// AST isn't re-built on every keystroke.
    pub(super) last_edit_at: Option<Instant>,
    /// Edits since the last successful compile that haven't been processed.
    pub(super) needs_compile: bool,

    /// If set, the editor should move its cursor to this byte offset on the
    /// next frame and then clear the field. Populated when the viewport
    /// selection changes so clicking a leg in 3D jumps the editor caret.
    pub(super) pending_caret: Option<usize>,

    pub(super) status: String,
}

impl FileState {
    pub(super) fn untitled(tab_id: u64) -> Self {
        Self {
            tab_id,
            path: None,
            source: String::new(),
            last_saved_source: String::new(),
            dirty: false,
            last_result: None,
            disk_mtime: None,
            last_watch_check: None,
            gen_prompt: String::new(),
            mod_prompt: String::new(),
            anim_prompt: String::new(),
            texture_cfg: TextureUiConfig::default(),
            export_opts: ExportOptions::default(),
            thinking_override: None,
            llm_rx: None,
            llm_in_flight: None,
            llm_progress: None,
            llm_started_at: None,
            llm_events: Vec::new(),
            llm_error: None,
            llm_last_prompt: None,
            camera: None,
            last_edit_at: None,
            needs_compile: false,
            pending_caret: None,
            status: "new scene".into(),
        }
    }

    pub(super) fn loaded(
        tab_id: u64,
        path: PathBuf,
        source: String,
        disk_mtime: Option<SystemTime>,
    ) -> Self {
        let status = format!("opened {}", path.display());
        let thinking_override = mogen_llm::parse_thinking_header(&source);
        Self {
            tab_id,
            path: Some(path),
            source: source.clone(),
            last_saved_source: source,
            dirty: false,
            last_result: None,
            disk_mtime,
            last_watch_check: None,
            gen_prompt: String::new(),
            mod_prompt: String::new(),
            anim_prompt: String::new(),
            texture_cfg: TextureUiConfig::default(),
            export_opts: ExportOptions::default(),
            thinking_override,
            llm_rx: None,
            llm_in_flight: None,
            llm_progress: None,
            llm_started_at: None,
            llm_events: Vec::new(),
            llm_error: None,
            llm_last_prompt: None,
            camera: None,
            last_edit_at: None,
            needs_compile: false,
            pending_caret: None,
            status,
        }
    }

    pub(super) fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into())
    }

    /// A freshly-spawned untitled buffer that has never been touched. Used to
    /// decide whether opening a file should replace the current tab or push a
    /// new one — we don't want to pile up empty tabs every time.
    pub(super) fn is_pristine_untitled(&self) -> bool {
        self.path.is_none()
            && self.source.is_empty()
            && !self.dirty
            && self.llm_in_flight.is_none()
    }

}
