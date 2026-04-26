use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use eframe::egui;
use mogen_export::ExportOptions;

use crate::settings::Settings;
use crate::theme::apply_theme;
use crate::viewer::Viewer;

mod autocomplete;
mod build;
mod compile;
mod error_class;
mod files;
mod llm;
mod pricing;
mod text_menu;
mod types;
mod ui_dialogs;
mod ui_llm;
mod ui_menu;
mod ui_panels;
mod util;
mod watcher;

use self::types::{
    AutocompleteState, BuildOutcome, EnhanceInFlight, EnhanceTarget, ExternalConflict, FileState,
    SessionUsage, ThumbCache, VIEWER_BG_COLOR,
};
use self::util::locate_project_root;

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

    /// "New from Prompt…" modal visibility and its prompt draft. The LLM
    /// generator only surfaces here — it is no longer part of the inspector.
    show_new_prompt: bool,
    new_prompt_draft: String,

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

    /// Help → About modal visibility.
    show_about: bool,

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

    /// Editor autocomplete popup state. One instance since only one editor is
    /// on-screen at a time; it's reset on tab switch / focus loss.
    autocomplete: AutocompleteState,

    /// Pending on-disk conflict awaiting user resolution. Set by the file
    /// watcher when an open file changed on disk and the buffer is dirty
    /// (clean buffers reload silently — see `watcher.rs`). Cleared when the
    /// modal is dismissed.
    pending_external: Option<ExternalConflict>,
}

impl MogenStudioApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let gl = cc
            .gl
            .as_ref()
            .expect("eframe was built with the glow backend, so cc.gl is Some");
        let viewer = Viewer::new(gl).expect("failed to init 3D viewer");

        let project_root = locate_project_root();

        let settings = Settings::load();
        let options_api_key_draft = settings.gemini_api_key.clone();
        apply_theme(&cc.egui_ctx, settings.theme());
        viewer.set_preview_shader(settings.preview_shader());

        // Hand the seed tab id 0 directly; the counter that hands out
        // subsequent ids starts at 1 so it never collides.
        let mut initial = FileState::untitled(0);
        initial.status = "welcome — open a MOG file to get started".into();

        let mut app = Self {
            files: vec![initial],
            active: 0,
            next_tab_id: 1,
            project_root,
            settings,
            show_options: false,
            options_api_key_draft,
            show_new_prompt: false,
            new_prompt_draft: String::new(),
            show_quit_confirm: false,
            confirmed_quit: false,
            pending_close_index: None,
            show_about: false,
            show_export: false,
            export_opts_draft: ExportOptions::default(),
            build_rx: None,
            build_stage: Arc::new(Mutex::new(String::new())),
            viewer,
            system_instruction_cache: None,
            tex_exists_cache: HashMap::new(),
            session_usage: SessionUsage::default(),
            thumb_cache: ThumbCache::new(),
            enhance_in_flight: None,
            enhance_error: None,
            autocomplete: AutocompleteState::default(),
            pending_external: None,
        };

        // Restore the last opened MOG when it still exists. Otherwise leave
        // the pristine untitled buffer in place.
        let last = app
            .settings
            .recent_files
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
            .or_else(|| {
                app.settings
                    .last_opened
                    .as_ref()
                    .map(PathBuf::from)
                    .filter(|p| p.is_file())
            });
        if let Some(p) = last {
            app.open_path(&p);
        }

        app
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
        self.poll_llm();
        self.poll_prompt_enhance();
        self.poll_build();
        self.drive_compile_debounce(ctx);
        self.check_external_changes(ctx);

        // Consume global shortcuts before the menu / editor see the key event,
        // so e.g. Ctrl+S doesn't reach the TextEdit.
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
                            .default_open(false)
                            .show(ui, |ui| self.ui_summary(ui));
                        egui::CollapsingHeader::new("Materials")
                            .default_open(false)
                            .show(ui, |ui| self.ui_materials(ui));
                        if !self.viewer.clips_snapshot().is_empty() {
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
        // directly under the code it refers to.
        egui::SidePanel::left("editor_panel")
            .resizable(true)
            .default_width(520.0)
            .min_width(200.0)
            .show(ctx, |ui| {
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
                egui::CentralPanel::default().show_inside(ui, |ui| self.ui_editor(ui));
            });

        let mut viewport_rect: Option<egui::Rect> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            // The 3D viewport is deliberately independent of the UI theme —
            // a calibrated neutral charcoal matches the look of every major
            // DCC app (Blender, Maya, 3ds Max, Modo) and keeps the model's
            // colours reading consistently regardless of the panel scheme.
            let fill = VIEWER_BG_COLOR;
            egui::Frame::canvas(ui.style()).fill(fill).show(ui, |ui| {
                let resp = self.viewer.show(ui);
                viewport_rect = Some(resp.rect);
            });
        });
        if let Some(r) = viewport_rect {
            self.ui_viewport_overlay(ctx, r);
        }

        // Pick up any gizmo drags / caret jumps produced by the viewport.
        // Must run after `viewer.show` because that's where new drag
        // commits and selections are queued.
        self.drain_viewport_edits(ctx);

        // After the editor has rendered for the active tab, snapshot its
        // camera back into the FileState so future tab switches restore it.
        let snap = self.viewer.camera_snapshot();
        self.files[self.active].camera = Some(snap);

        self.ui_options(ctx);
        self.ui_new_prompt(ctx);
        self.ui_quit_confirm(ctx);
        self.ui_close_confirm(ctx);
        self.ui_export_dialog(ctx);
        self.ui_external_conflict(ctx);
        self.ui_about(ctx);

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
        if self.any_in_flight() || self.any_enhance_in_flight() || self.build_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
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
