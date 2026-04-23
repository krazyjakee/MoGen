use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use mgen_core::{Diagnostic, Severity};
use mgen_llm::gemini::{GeminiClient, GenerateConfig, DEFAULT_MODEL};
use mgen_llm::textures::{
    build_plan, maybe_cache, run_plan, splice_textures, PlanAction, TexturesArgs,
    DEFAULT_TEXTURE_SIZE,
};
use mgen_llm::{
    embed_seed_header, generate_with_repair, parse_seed_header, system_instruction, RepairConfig,
    StdlibIndex, ThinkingLevel, DEFAULT_IMAGE_MODEL,
};

use crate::pipeline::{compile, write_glb_with_source, CompileResult, Stage};
use crate::settings::{thinking_level_key, thinking_level_label, Settings, THINKING_LEVELS};
use crate::viewer::{CameraSnapshot, Viewer};

/// Debounce window before a keystroke triggers a recompile. Long enough that
/// holding a key (or pasting) doesn't recompile mid-word; short enough that
/// the diagnostics panel feels live.
const COMPILE_DEBOUNCE: Duration = Duration::from_millis(180);

/// TTL on the texture-existence cache. Texture roster runs every paint and
/// would otherwise stat every PNG every frame.
const TEX_EXISTS_TTL: Duration = Duration::from_millis(1500);

/// Result from a background LLM call. Always includes the DSL we tried to
/// compile so the UI can drop it into the editor even when validation failed.
struct LlmOutcome {
    dsl: String,
    diagnostics: Vec<Diagnostic>,
    tokens: u32,
    calls: u32,
    error: Option<String>,
    kind: LlmKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LlmKind {
    Generate,
    Modify,
    Animate,
    Textures,
}

impl LlmKind {
    fn label(self) -> &'static str {
        match self {
            LlmKind::Generate => "generate",
            LlmKind::Modify => "modify",
            LlmKind::Animate => "animate",
            LlmKind::Textures => "textures",
        }
    }
}

/// User-tweakable knobs on the texture pipeline. Mirrors the CLI's
/// `mgen textures` flags so the GUI is not silently more restrictive.
#[derive(Clone)]
struct TextureUiConfig {
    style: String,
    texture_size: u32,
    normal_strength: f32,
    no_normal: bool,
    no_metallic_roughness: bool,
    no_occlusion: bool,
    force: bool,
    no_cache: bool,
    /// Whether the "Advanced" expander is open. Persisted per-file so users
    /// can leave it open on the file they're iterating on.
    expanded: bool,
}

impl Default for TextureUiConfig {
    fn default() -> Self {
        Self {
            style: "photorealistic".to_string(),
            texture_size: DEFAULT_TEXTURE_SIZE,
            normal_strength: 1.5,
            no_normal: false,
            no_metallic_roughness: false,
            no_occlusion: false,
            force: false,
            no_cache: false,
            expanded: false,
        }
    }
}

/// Per-file state. Every open `.mg` owns its own buffer, compile result,
/// prompts, and in-flight LLM job, so switching files while Gemini is running
/// does not clobber the other file — you can generate on several models at
/// once.
struct FileState {
    path: Option<PathBuf>,
    source: String,
    last_saved_source: String,
    dirty: bool,
    last_result: Option<CompileResult>,

    gen_prompt: String,
    mod_prompt: String,
    anim_prompt: String,
    texture_cfg: TextureUiConfig,

    llm_rx: Option<Receiver<LlmOutcome>>,
    llm_in_flight: Option<LlmKind>,

    /// Captured camera so switching tabs doesn't snap the user's framing.
    /// Restored on `activate` when present, refreshed every frame for the
    /// active tab.
    camera: Option<CameraSnapshot>,

    /// Wall-clock time of the last edit. Drives the compile debounce so the
    /// AST isn't re-built on every keystroke.
    last_edit_at: Option<Instant>,
    /// Edits since the last successful compile that haven't been processed.
    needs_compile: bool,

    status: String,
}

impl FileState {
    fn untitled() -> Self {
        Self {
            path: None,
            source: String::new(),
            last_saved_source: String::new(),
            dirty: false,
            last_result: None,
            gen_prompt: String::new(),
            mod_prompt: String::new(),
            anim_prompt: String::new(),
            texture_cfg: TextureUiConfig::default(),
            llm_rx: None,
            llm_in_flight: None,
            camera: None,
            last_edit_at: None,
            needs_compile: false,
            status: "new scene".into(),
        }
    }

    fn loaded(path: PathBuf, source: String) -> Self {
        let status = format!("opened {}", path.display());
        Self {
            path: Some(path),
            source: source.clone(),
            last_saved_source: source,
            dirty: false,
            last_result: None,
            gen_prompt: String::new(),
            mod_prompt: String::new(),
            anim_prompt: String::new(),
            texture_cfg: TextureUiConfig::default(),
            llm_rx: None,
            llm_in_flight: None,
            camera: None,
            last_edit_at: None,
            needs_compile: false,
            status,
        }
    }

    fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into())
    }

    /// A freshly-spawned untitled buffer that has never been touched. Used to
    /// decide whether opening a file should replace the current tab or push a
    /// new one — we don't want to pile up empty tabs every time.
    fn is_pristine_untitled(&self) -> bool {
        self.path.is_none()
            && self.source.is_empty()
            && !self.dirty
            && self.llm_in_flight.is_none()
    }

}

pub struct MgenApp {
    files: Vec<FileState>,
    active: usize,

    project_root: PathBuf,
    examples: Vec<PathBuf>,
    generated: Vec<PathBuf>,

    settings: Settings,
    show_options: bool,
    options_api_key_draft: String,

    viewer: Viewer,

    /// Computed once: the system instruction grows with stdlib + grammar
    /// and is shared by every text-LLM call.
    system_instruction_cache: Option<Arc<String>>,

    /// `(path, mtime)` -> exists, with last-checked timestamp. Avoids stat'ing
    /// every texture path on every frame.
    tex_exists_cache: HashMap<PathBuf, (Option<SystemTime>, bool, Instant)>,
}

impl MgenApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let gl = cc
            .gl
            .as_ref()
            .expect("eframe was built with the glow backend, so cc.gl is Some");
        let viewer = Viewer::new(gl).expect("failed to init 3D viewer");

        let project_root = locate_project_root();
        let examples = scan_mg_dir(&project_root, "examples");
        let generated = scan_mg_dir(&project_root, "generated");

        let settings = Settings::load();
        let options_api_key_draft = settings.gemini_api_key.clone();

        let mut initial = FileState::untitled();
        initial.status = "welcome — open an example to get started".into();

        let mut app = Self {
            files: vec![initial],
            active: 0,
            project_root,
            examples,
            generated,
            settings,
            show_options: false,
            options_api_key_draft,
            viewer,
            system_instruction_cache: None,
            tex_exists_cache: HashMap::new(),
        };

        // Restore the last opened file when it still exists; otherwise fall
        // back to the chair example so the welcome state isn't blank.
        let last = app
            .settings
            .last_opened
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.is_file());
        if let Some(p) = last {
            app.open_path(&p);
        } else if let Some(p) = app.examples.iter().find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "chair.mg")
                .unwrap_or(false)
        }) {
            let p = p.clone();
            app.open_path(&p);
        }

        app
    }

    fn active(&self) -> &FileState {
        &self.files[self.active]
    }

    fn active_mut(&mut self) -> &mut FileState {
        &mut self.files[self.active]
    }

    fn file_index_by_path(&self, path: &Path) -> Option<usize> {
        self.files
            .iter()
            .position(|f| f.path.as_deref() == Some(path))
    }

    fn open_path(&mut self, path: &Path) {
        if let Some(i) = self.file_index_by_path(path) {
            self.activate(i);
            return;
        }
        match fs::read_to_string(path) {
            Ok(src) => {
                let f = FileState::loaded(path.to_path_buf(), src);
                if self.files.len() == 1 && self.files[0].is_pristine_untitled() {
                    self.files[0] = f;
                    self.activate(0);
                } else {
                    self.files.push(f);
                    let i = self.files.len() - 1;
                    self.activate(i);
                }
                self.remember_last_opened();
            }
            Err(e) => {
                self.active_mut().status = format!("open failed: {e}");
            }
        }
    }

    /// Save the active file's path as `last_opened` so the next launch picks
    /// up where the user left off. Quietly ignores save errors — this is a
    /// nice-to-have, not load-bearing.
    fn remember_last_opened(&mut self) {
        let path_str = self
            .active()
            .path
            .as_ref()
            .map(|p| p.display().to_string());
        if self.settings.last_opened == path_str {
            return;
        }
        self.settings.last_opened = path_str;
        let _ = self.settings.save();
    }

    fn activate(&mut self, i: usize) {
        // Snapshot the previous tab's camera so we can restore it on return.
        let prev = self.active;
        if prev < self.files.len() {
            self.files[prev].camera = Some(self.viewer.camera_snapshot());
        }
        self.active = i;
        if self.active().last_result.is_none() {
            self.compile_active();
        } else {
            self.refresh_viewer_from_active();
        }
        // Restore stored camera if we have one; otherwise leave Viewer's
        // freshly-fitted view in place.
        if let Some(snap) = self.files[self.active].camera {
            self.viewer.restore_camera(snap);
        }
    }

    fn refresh_viewer_from_active(&mut self) {
        let i = self.active;
        let base_dir = self.files[i].path.as_deref().and_then(|p| p.parent());
        match &self.files[i].last_result {
            Some(r) if matches!(r.stage, Stage::Ok) => {
                if let Some(scene) = &r.scene {
                    self.viewer.set_scene(scene, base_dir);
                    return;
                }
            }
            _ => {}
        }
        self.viewer.clear();
    }

    fn compile_active(&mut self) {
        self.compile_file(self.active);
    }

    fn compile_file(&mut self, i: usize) {
        let r = compile(&self.files[i].source);
        if i == self.active {
            match &r.scene {
                Some(scene) if matches!(r.stage, Stage::Ok) => {
                    let base_dir = self.files[i].path.as_deref().and_then(|p| p.parent());
                    self.viewer.set_scene(scene, base_dir);
                }
                Some(_) => {}
                None => self.viewer.clear(),
            }
        }
        self.files[i].last_result = Some(r);
        self.files[i].needs_compile = false;
    }

    /// Close the tab at `i`. If it's the only open tab, replace it with a
    /// fresh untitled buffer rather than leaving the app with zero files.
    /// Dropping `llm_rx` silently abandons any in-flight Gemini call for
    /// that file.
    fn close_file(&mut self, i: usize) {
        if self.files.len() <= 1 {
            self.files[0] = FileState::untitled();
            self.active = 0;
            self.viewer.clear();
            return;
        }
        self.files.remove(i);
        if self.active == i {
            if self.active >= self.files.len() {
                self.active = self.files.len() - 1;
            }
            if self.active().last_result.is_none() {
                self.compile_active();
            } else {
                self.refresh_viewer_from_active();
            }
            if let Some(snap) = self.files[self.active].camera {
                self.viewer.restore_camera(snap);
            }
        } else if i < self.active {
            self.active -= 1;
        }
    }

    fn save_to(&mut self, path: &Path) {
        let src = self.active().source.clone();
        if let Err(e) = fs::write(path, &src) {
            self.active_mut().status = format!("save failed: {e}");
            return;
        }
        let f = self.active_mut();
        f.path = Some(path.to_path_buf());
        f.last_saved_source = src;
        f.dirty = false;
        f.status = format!("saved {}", path.display());
        self.refresh_file_lists();
        self.remember_last_opened();
    }

    fn refresh_file_lists(&mut self) {
        self.examples = scan_mg_dir(&self.project_root, "examples");
        self.generated = scan_mg_dir(&self.project_root, "generated");
    }

    fn save(&mut self) {
        if let Some(p) = self.active().path.clone() {
            self.save_to(&p);
        } else {
            self.save_as();
        }
    }

    fn save_as(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("mgen DSL", &["mg"])
            .set_directory(&self.project_root);
        if let Some(p) = &self.active().path {
            if let Some(name) = p.file_name() {
                dialog = dialog.set_file_name(name.to_string_lossy());
            }
        }
        if let Some(chosen) = dialog.save_file() {
            self.save_to(&chosen);
        }
    }

    fn open_dialog(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("mgen DSL", &["mg"])
            .set_directory(&self.project_root);
        if let Some(chosen) = dialog.pick_file() {
            self.open_path(&chosen);
        }
    }

    fn new_untitled(&mut self) {
        let mut f = FileState::untitled();
        f.source = "scene {\n  box \"b\" (size=[1, 1, 1])\n}\n".to_string();
        f.last_saved_source = f.source.clone();
        f.status = "new scene".into();
        if self.files.len() == 1 && self.files[0].is_pristine_untitled() {
            self.files[0] = f;
            self.activate(0);
        } else {
            self.files.push(f);
            let i = self.files.len() - 1;
            self.activate(i);
        }
    }

    fn build_and_export(&mut self) {
        self.compile_active();
        let i = self.active;
        let Some(result) = &self.files[i].last_result else {
            return;
        };
        if result.stage != Stage::Ok {
            let msg = format!(
                "build failed at stage {:?} — see diagnostics",
                result.stage
            );
            self.files[i].status = msg;
            return;
        }
        let scene = result.scene.as_ref().expect("Ok implies Some(scene)");
        let out = self.files[i]
            .path
            .as_ref()
            .map(|p| p.with_extension("glb"))
            .unwrap_or_else(|| self.project_root.join("untitled.glb"));
        let source_dir = self.files[i].path.as_deref().and_then(|p| p.parent());
        let write_result = write_glb_with_source(scene, &out, source_dir);
        let msg = match write_result {
            Ok(()) => {
                let size = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                format!("wrote {} ({} bytes)", out.display(), size)
            }
            Err(e) => format!("export failed: {e}"),
        };
        self.files[i].status = msg;
    }

    fn start_llm_generate(&mut self, ctx: egui::Context) {
        let prompt = self.active().gen_prompt.trim().to_string();
        if prompt.is_empty() {
            self.active_mut().status = "enter a prompt first".into();
            return;
        }
        self.spawn_llm(ctx, LlmKind::Generate, prompt, None);
    }

    fn start_llm_modify(&mut self, ctx: egui::Context) {
        let (prompt, src_empty, existing) = {
            let f = self.active();
            (
                f.mod_prompt.trim().to_string(),
                f.source.trim().is_empty(),
                f.source.clone(),
            )
        };
        if prompt.is_empty() {
            self.active_mut().status = "enter a prompt first".into();
            return;
        }
        if src_empty {
            self.active_mut().status = "modify needs existing DSL to edit".into();
            return;
        }
        self.spawn_llm(ctx, LlmKind::Modify, prompt, Some(existing));
    }

    fn start_llm_animate(&mut self, ctx: egui::Context) {
        let (prompt, src_empty, existing) = {
            let f = self.active();
            (
                f.anim_prompt.trim().to_string(),
                f.source.trim().is_empty(),
                f.source.clone(),
            )
        };
        if prompt.is_empty() {
            self.active_mut().status = "enter a prompt first".into();
            return;
        }
        if src_empty {
            self.active_mut().status = "animate needs existing DSL to edit".into();
            return;
        }
        self.spawn_llm(ctx, LlmKind::Animate, prompt, Some(existing));
    }

    fn start_llm_textures(&mut self, ctx: egui::Context) {
        let (src_empty, path_opt, src, cfg) = {
            let f = self.active();
            (
                f.source.trim().is_empty(),
                f.path.clone(),
                f.source.clone(),
                f.texture_cfg.clone(),
            )
        };
        if src_empty {
            self.active_mut().status = "textures needs an open .mg".into();
            return;
        }
        let Some(path) = path_opt else {
            self.active_mut().status =
                "save the file first — textures writes PNGs next to it".into();
            return;
        };
        let api_key = match self.resolve_api_key() {
            Some(k) => k,
            None => {
                self.active_mut().status =
                    "no Gemini API key — set one in Options… or export GEMINI_API_KEY".into();
                return;
            }
        };

        let (tx, rx) = std::sync::mpsc::channel();
        let af = self.active_mut();
        af.llm_rx = Some(rx);
        af.llm_in_flight = Some(LlmKind::Textures);
        af.status = "generating textures with Gemini Image…".into();

        std::thread::spawn(move || {
            let outcome = run_llm_textures(src, path, api_key, cfg);
            let _ = tx.send(outcome);
            ctx.request_repaint();
        });
    }

    /// Drop the receiver for the active file's in-flight LLM call. The worker
    /// thread keeps running but its result is discarded silently — there's no
    /// portable way to abort an in-progress HTTP request through reqwest's
    /// blocking client.
    fn cancel_active_llm(&mut self) {
        let f = self.active_mut();
        if f.llm_in_flight.is_none() {
            return;
        }
        f.llm_rx = None;
        f.llm_in_flight = None;
        f.status = "llm: cancelled (background call may still finish but result is dropped)".into();
    }

    /// Prefer a key saved in Options; fall back to the `GEMINI_API_KEY` env
    /// var so existing shell-exported setups keep working.
    fn resolve_api_key(&self) -> Option<String> {
        if let Some(k) = self.settings.gemini_api_key() {
            return Some(k.to_string());
        }
        std::env::var("GEMINI_API_KEY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// Build (or reuse) the LLM system instruction. It pulls in the full
    /// stdlib + grammar so it isn't free; cache the string and clone the Arc
    /// across spawns instead of regenerating per call.
    fn cached_system_instruction(&mut self) -> Arc<String> {
        if self.system_instruction_cache.is_none() {
            self.system_instruction_cache = Some(Arc::new(system_instruction(
                &StdlibIndex::default(),
            )));
        }
        self.system_instruction_cache.as_ref().unwrap().clone()
    }

    fn spawn_llm(
        &mut self,
        ctx: egui::Context,
        kind: LlmKind,
        prompt: String,
        existing: Option<String>,
    ) {
        let api_key = match self.resolve_api_key() {
            Some(k) => k,
            None => {
                self.active_mut().status =
                    "no Gemini API key — set one in Options… or export GEMINI_API_KEY".into();
                return;
            }
        };

        let thinking = self.settings.thinking_level();
        let sys_instr = self.cached_system_instruction();

        let (tx, rx) = std::sync::mpsc::channel();
        let f = self.active_mut();
        f.llm_rx = Some(rx);
        f.llm_in_flight = Some(kind);
        f.status = match kind {
            LlmKind::Generate => "calling Gemini (generate)…".into(),
            LlmKind::Modify => "calling Gemini (modify)…".into(),
            LlmKind::Animate => "calling Gemini (animate)…".into(),
            // Textures takes its own path via `start_llm_textures` and never
            // reaches spawn_llm.
            LlmKind::Textures => unreachable!("spawn_llm is text-only"),
        };

        std::thread::spawn(move || {
            let outcome = run_llm(kind, prompt, existing, api_key, thinking, sys_instr);
            let _ = tx.send(outcome);
            ctx.request_repaint();
        });
    }

    /// Drain any completed LLM outcomes across all open files. Multiple jobs
    /// can be in flight simultaneously, one per file, so we have to walk them
    /// all every frame.
    fn poll_llm(&mut self) {
        let pending: Vec<(usize, LlmOutcome)> = (0..self.files.len())
            .filter_map(|i| {
                let rx = self.files[i].llm_rx.as_ref()?;
                rx.try_recv().ok().map(|o| (i, o))
            })
            .collect();
        if pending.is_empty() {
            return;
        }
        for (i, outcome) in pending {
            self.apply_llm_outcome(i, outcome);
        }
        // Generate / Modify / Animate / Textures may have written DSL or PNGs
        // to disk; refresh the sidebar so the user sees them without a
        // manual save round-trip.
        self.refresh_file_lists();
    }

    fn apply_llm_outcome(&mut self, i: usize, outcome: LlmOutcome) {
        let f = &mut self.files[i];
        f.llm_rx = None;
        f.llm_in_flight = None;

        if let Some(err) = outcome.error {
            f.status = format!("llm error: {err}");
            return;
        }

        let kind_label = outcome.kind.label();

        // Drop the returned DSL into the file's buffer so the user can inspect
        // / save it even when validation later fails.
        f.source = outcome.dsl;
        f.dirty = f.source != f.last_saved_source;
        f.needs_compile = true;
        f.last_edit_at = Some(Instant::now());

        // Textures wrote PNG files next to the .mg; persist the spliced DSL
        // there too so the texture paths resolve on the next GLB export.
        if matches!(outcome.kind, LlmKind::Textures) {
            if let Some(p) = f.path.clone() {
                let src = f.source.clone();
                match fs::write(&p, &src) {
                    Ok(()) => {
                        f.last_saved_source = src;
                        f.dirty = false;
                    }
                    Err(e) => {
                        f.status = format!("textures: wrote PNGs but saving DSL failed: {e}");
                        return;
                    }
                }
            }
        }

        // Only reset the camera when the file that just completed is the one
        // currently on screen — otherwise a background job would yank the
        // user's view out from under them.
        if matches!(outcome.kind, LlmKind::Generate) && i == self.active {
            self.viewer.reset_view();
        }

        self.compile_file(i);

        let has_errors = outcome
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, Severity::Error));

        let status = if has_errors {
            format!(
                "{kind_label}: DSL invalid after {} call(s), {} tokens — see diagnostics",
                outcome.calls, outcome.tokens
            )
        } else if matches!(outcome.kind, LlmKind::Textures) {
            format!(
                "textures: wrote {} PNG{}, DSL updated",
                outcome.calls,
                if outcome.calls == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "{kind_label}: ready ({} call(s), {} tokens)",
                outcome.calls, outcome.tokens
            )
        };
        self.files[i].status = status;
    }

    fn any_in_flight(&self) -> bool {
        self.files.iter().any(|f| f.llm_in_flight.is_some())
    }

    fn count_in_flight(&self) -> usize {
        self.files
            .iter()
            .filter(|f| f.llm_in_flight.is_some())
            .count()
    }

    /// Trigger a compile if the active buffer's debounce window has elapsed.
    /// Also keeps repainting while the window is open so the UI lands on the
    /// recompile naturally.
    fn drive_compile_debounce(&mut self, ctx: &egui::Context) {
        let i = self.active;
        let f = &self.files[i];
        if !f.needs_compile {
            return;
        }
        if let Some(t) = f.last_edit_at {
            let elapsed = t.elapsed();
            if elapsed >= COMPILE_DEBOUNCE {
                self.compile_active();
            } else {
                ctx.request_repaint_after(COMPILE_DEBOUNCE - elapsed);
            }
        }
    }

    fn ui_toolbar(&mut self, ui: &mut egui::Ui) {
        egui::menu::bar(ui, |ui| {
            if ui
                .button("New")
                .on_hover_text("Create a fresh untitled .mg buffer")
                .clicked()
            {
                self.new_untitled();
            }
            if ui
                .button("Open…")
                .on_hover_text("Open a .mg file from disk")
                .clicked()
            {
                self.open_dialog();
            }
            if ui
                .button("Save")
                .on_hover_text("Save the active buffer")
                .clicked()
            {
                self.save();
            }
            if ui.button("Save As…").clicked() {
                self.save_as();
            }
            ui.separator();
            if ui
                .button("Build GLB")
                .on_hover_text("Compile and export .glb next to the source")
                .clicked()
            {
                self.build_and_export();
            }
            if ui
                .button("Re-check")
                .on_hover_text("Re-run validate without exporting")
                .clicked()
            {
                self.compile_active();
            }
            ui.separator();
            if ui
                .button("Frame")
                .on_hover_text("Re-fit the camera to the scene")
                .clicked()
            {
                self.viewer.frame_view();
            }
            ui.separator();
            if ui.button("Options…").clicked() {
                self.options_api_key_draft = self.settings.gemini_api_key.clone();
                self.show_options = true;
            }
            ui.separator();
            let f = self.active();
            let marker = if f.dirty { " •" } else { "" };
            let inflight = if f.llm_in_flight.is_some() { " ⟳" } else { "" };
            ui.label(format!("{}{}{}", f.display_name(), marker, inflight));
        });
    }

    fn ui_sidebar(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // "Open" section doubles as tabs: every file we're tracking
                // (active or not) appears here, with spinner ⟳ for in-flight
                // jobs and • for unsaved edits. Click to activate, × to close.
                ui.heading("Open");
                ui.separator();
                let mut activate: Option<usize> = None;
                let mut close: Option<usize> = None;
                for (i, f) in self.files.iter().enumerate() {
                    let selected = i == self.active;
                    let mut label = f.display_name();
                    if f.dirty {
                        label.push_str(" •");
                    }
                    if f.llm_in_flight.is_some() {
                        label.push_str(" ⟳");
                    }
                    ui.horizontal(|ui| {
                        let resp = ui.selectable_label(selected, &label);
                        if resp.clicked() {
                            activate = Some(i);
                        }
                        if ui.small_button("×").clicked() {
                            close = Some(i);
                        }
                    });
                }
                if let Some(i) = activate {
                    self.activate(i);
                }
                if let Some(i) = close {
                    self.close_file(i);
                }

                ui.add_space(12.0);
                let mut to_open: Option<PathBuf> = None;

                egui::CollapsingHeader::new("Examples")
                    .default_open(true)
                    .show(ui, |ui| {
                        for ex in &self.examples {
                            let name = ex
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| ex.to_string_lossy().into_owned());
                            let loaded_idx = self.file_index_by_path(ex);
                            let selected = loaded_idx == Some(self.active);
                            let mut label = name;
                            if let Some(idx) = loaded_idx {
                                if self.files[idx].llm_in_flight.is_some() {
                                    label.push_str(" ⟳");
                                }
                            }
                            if ui.selectable_label(selected, label).clicked() {
                                to_open = Some(ex.clone());
                            }
                        }
                    });

                ui.add_space(8.0);
                egui::CollapsingHeader::new("Generated")
                    .default_open(true)
                    .show(ui, |ui| {
                        if self.generated.is_empty() {
                            ui.label("(none yet)");
                        } else {
                            for gen in &self.generated {
                                let name = gen
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| gen.to_string_lossy().into_owned());
                                let loaded_idx = self.file_index_by_path(gen);
                                let selected = loaded_idx == Some(self.active);
                                let mut label = name;
                                if let Some(idx) = loaded_idx {
                                    if self.files[idx].llm_in_flight.is_some() {
                                        label.push_str(" ⟳");
                                    }
                                }
                                if ui.selectable_label(selected, label).clicked() {
                                    to_open = Some(gen.clone());
                                }
                            }
                        }
                    });

                if let Some(p) = to_open {
                    self.open_path(&p);
                }
            });
    }

    fn ui_editor(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let resp = ui.add_sized(
                    [ui.available_width(), 0.0],
                    egui::TextEdit::multiline(&mut self.files[self.active].source)
                        // code_editor() implies lock_focus(true), so Tab inserts
                        // a tab character instead of moving focus out of the
                        // editor — the right behavior for a code surface.
                        .code_editor()
                        .desired_rows(20)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
                if resp.changed() {
                    changed = true;
                }
            });

        if changed {
            let i = self.active;
            self.files[i].dirty = self.files[i].source != self.files[i].last_saved_source;
            self.files[i].needs_compile = true;
            self.files[i].last_edit_at = Some(Instant::now());
            // Compilation itself is gated by `drive_compile_debounce` so a
            // burst of keystrokes only re-parses once the user pauses.
        }
    }

    fn ui_diagnostics(&mut self, ui: &mut egui::Ui) {
        let f = &self.files[self.active];
        let Some(result) = &f.last_result else {
            ui.label("(no build yet)");
            return;
        };
        if result.diagnostics.is_empty() {
            match result.stage {
                Stage::Ok => {
                    ui.colored_label(egui::Color32::from_rgb(80, 200, 120), "✓ ok");
                }
                _ => {
                    ui.label(format!("{:?}", result.stage));
                }
            }
            return;
        }
        for d in &result.diagnostics {
            let (color, tag) = match d.severity {
                Severity::Error => (egui::Color32::from_rgb(230, 100, 100), "error"),
                Severity::Warning => (egui::Color32::from_rgb(230, 200, 100), "warn"),
                Severity::Info => (egui::Color32::from_rgb(150, 180, 230), "info"),
            };
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(color, format!("[{tag}] {}", d.code));
                if let Some(span) = d.span {
                    let (line, col) = offset_to_line_col(&f.source, span.start);
                    ui.label(format!("{line}:{col}"));
                }
                ui.label(&d.message);
            });
        }
    }

    fn ui_summary(&mut self, ui: &mut egui::Ui) {
        let i = self.active;
        let Some(result) = &self.files[i].last_result else {
            ui.label("(no build yet)");
            return;
        };
        let Some(scene) = &result.scene else {
            ui.label("(no scene — fix errors first)");
            return;
        };
        let mut tris = 0usize;
        let mut verts = 0usize;
        let mut meshes = 0usize;
        for n in &scene.nodes {
            if let Some(m) = &n.mesh {
                tris += m.indices.len() / 3;
                verts += m.positions.len();
                meshes += 1;
            }
        }
        ui.label(format!("nodes: {}", scene.nodes.len()));
        ui.label(format!("meshes: {meshes}"));
        ui.label(format!("triangles: {tris}"));
        ui.label(format!("vertices: {verts}"));
        ui.label(format!("materials: {}", scene.materials.len()));
        if !scene.skins.is_empty() {
            ui.label(format!("skins: {}", scene.skins.len()));
        }
        if !scene.clips.is_empty() {
            ui.label(format!("clips: {}", scene.clips.len()));
        }
        if !scene.joints.is_empty() {
            ui.label(format!("joints: {}", scene.joints.len()));
        }

        // Texture roster. Listing each path (resolved against the .mg dir)
        // with a green ✓ / red ✗ lets users verify their files exist without
        // waiting for the export failure. Existence is cached for ~1.5s so
        // we don't stat every PNG every frame.
        // Own `source_dir` so the existence-cache call below can take
        // `&mut self` without overlapping borrows from `self.files[i].path`.
        let source_dir: Option<PathBuf> = self.files[i]
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        let texture_slots = gather_texture_refs(scene);
        if !texture_slots.is_empty() {
            ui.add_space(8.0);
            ui.label(format!("textures: {}", texture_slots.len()));
            // Pre-resolve and check existence once before the ScrollArea so
            // we don't double-borrow self in the closure.
            let rows: Vec<(String, &'static str, PathBuf, bool)> = texture_slots
                .into_iter()
                .map(|(mat, slot, rel)| {
                    let resolved = resolve_for_check(&rel, source_dir.as_deref());
                    let exists = self.cached_exists(&resolved);
                    (mat, slot, rel, exists)
                })
                .collect();
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .max_height(140.0)
                .show(ui, |ui| {
                    for (mat_name, slot, rel_path, exists) in &rows {
                        let (mark, color) = if *exists {
                            ("✓", egui::Color32::from_rgb(80, 200, 120))
                        } else {
                            ("✗", egui::Color32::from_rgb(230, 100, 100))
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(color, mark);
                            ui.label(format!("{mat_name}.{slot}"));
                            let display = ellipsize_path(rel_path, 36);
                            ui.label(display)
                                .on_hover_text(rel_path.to_string_lossy());
                        });
                    }
                });
        }
    }

    /// Stat-cached existence check. The texture-roster paint runs every frame
    /// so a naive `Path::exists()` would hit the FS once per slot per frame.
    fn cached_exists(&mut self, path: &Path) -> bool {
        let now = Instant::now();
        if let Some((_mtime, exists, checked)) = self.tex_exists_cache.get(path) {
            if now.duration_since(*checked) < TEX_EXISTS_TTL {
                return *exists;
            }
        }
        let meta = fs::metadata(path);
        let exists = meta.is_ok();
        let mtime = meta.ok().and_then(|m| m.modified().ok());
        self.tex_exists_cache
            .insert(path.to_path_buf(), (mtime, exists, now));
        exists
    }

    fn ui_animation(&mut self, ui: &mut egui::Ui) {
        let clips = self.viewer.clips_snapshot();
        if clips.is_empty() {
            return;
        }

        let mut active = self.viewer.active_clips();
        let mut times = self.viewer.anim_times();
        // A fresh compile can briefly desync the snapshot lengths; pad so
        // iteration below is safe.
        if active.len() != clips.len() {
            active.resize(clips.len(), false);
        }
        if times.len() != clips.len() {
            times.resize(clips.len(), 0.0);
        }

        let playing = self.viewer.is_playing();
        ui.horizontal(|ui| {
            let label = if playing { "⏸ Pause" } else { "▶ Play" };
            if ui
                .button(label)
                .on_hover_text("Toggle clip playback")
                .clicked()
            {
                self.viewer.set_playing(!playing);
            }
            if ui
                .button("Reset")
                .on_hover_text("Rewind every clip to t = 0")
                .clicked()
            {
                self.viewer.reset_anim_times();
            }
            if ui
                .button("All")
                .on_hover_text("Activate every clip")
                .clicked()
            {
                self.viewer.set_all_clips_active(true);
            }
            if ui
                .button("None")
                .on_hover_text("Deactivate every clip")
                .clicked()
            {
                self.viewer.set_all_clips_active(false);
            }
        });

        ui.add_space(4.0);
        for (i, c) in clips.iter().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    let mut on = active[i];
                    if ui
                        .checkbox(&mut on, "")
                        .on_hover_text("Include this clip in the active pose")
                        .changed()
                    {
                        self.viewer.set_clip_active(i, on);
                    }
                    ui.label(egui::RichText::new(&c.name).strong());
                    ui.label(
                        egui::RichText::new(format!("{:.2}s", c.duration))
                            .small()
                            .weak(),
                    );
                });
                let dur = c.duration.max(0.001);
                let mut t = times[i].clamp(0.0, dur);
                let resp = ui.add(
                    egui::Slider::new(&mut t, 0.0..=dur)
                        .text("t")
                        .clamping(egui::SliderClamping::Always)
                        .fixed_decimals(2),
                );
                if resp.changed() {
                    self.viewer.seek_clip(i, t);
                }
            });
        }
    }

    fn ui_llm(&mut self, ui: &mut egui::Ui) {
        let has_key = self.resolve_api_key().is_some();
        if !has_key {
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                "no Gemini API key — set one in Options…",
            );
        }

        // Gate buttons on *this file's* in-flight state only — other files can
        // still have jobs running in parallel and the user can kick off a new
        // one here as long as this file isn't already busy.
        let busy = self.active().llm_in_flight.is_some();
        let src_empty = self.active().source.trim().is_empty();
        let has_path = self.active().path.is_some();

        ui.label("Generate:");
        ui.add(
            egui::TextEdit::multiline(&mut self.files[self.active].gen_prompt)
                .hint_text("e.g. a wooden stool with three legs")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        let gen_enabled = has_key && !busy;
        let gen_btn = ui
            .add_enabled(gen_enabled, egui::Button::new("Generate"))
            .on_hover_text(
                "Ask Gemini to write a fresh .mg from your prompt. \
                 If the active buffer has content you'll be asked whether \
                 to overwrite or open in a new tab.",
            );
        if gen_btn.clicked() {
            let ctx = ui.ctx().clone();
            self.start_llm_generate(ctx);
        }

        ui.add_space(8.0);
        ui.label("Modify current:");
        ui.add(
            egui::TextEdit::multiline(&mut self.files[self.active].mod_prompt)
                .hint_text("e.g. make the legs taller")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        let mod_enabled = has_key && !busy && !src_empty;
        if ui
            .add_enabled(mod_enabled, egui::Button::new("Modify"))
            .on_hover_text("Smallest-edit rewrite of the current buffer")
            .clicked()
        {
            let ctx = ui.ctx().clone();
            self.start_llm_modify(ctx);
        }

        ui.add_space(8.0);
        ui.label("Animate current:");
        ui.add(
            egui::TextEdit::multiline(&mut self.files[self.active].anim_prompt)
                .hint_text("e.g. spin the rotor at 120 rpm")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        let anim_enabled = has_key && !busy && !src_empty;
        if ui
            .add_enabled(anim_enabled, egui::Button::new("Animate"))
            .on_hover_text("Append joints/clips/skeleton to the current buffer")
            .clicked()
        {
            let ctx = ui.ctx().clone();
            self.start_llm_animate(ctx);
        }

        ui.add_space(8.0);
        ui.label("Textures:");
        ui.label(
            egui::RichText::new(
                "generates a base_color PNG per material using Gemini Image, \
                 writes to ./textures/ next to the .mg, and splices the \
                 resulting paths into each material",
            )
            .small()
            .weak(),
        );

        // Advanced texture knobs — the CLI exposes all of these and the GUI
        // used to silently pin them to defaults. Persisted per-file so users
        // can iterate.
        let cfg_open = self.active().texture_cfg.expanded;
        let header = egui::CollapsingHeader::new("Texture options")
            .default_open(cfg_open)
            .id_salt(("tex_opts", self.active));
        let resp = header.show(ui, |ui| {
            let cfg = &mut self.files[self.active].texture_cfg;
            egui::Grid::new("tex_opts_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Style").on_hover_text(
                        "Free-form prompt suffix appended to every material's image prompt",
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut cfg.style)
                            .hint_text("photorealistic")
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();

                    ui.label("Texture size")
                        .on_hover_text("Cap on the longer side, in pixels (0 = keep native)");
                    ui.add(
                        egui::DragValue::new(&mut cfg.texture_size)
                            .range(0..=4096)
                            .speed(8.0),
                    );
                    ui.end_row();

                    ui.label("Normal strength")
                        .on_hover_text("Slope multiplier for the derived normal map");
                    ui.add(
                        egui::DragValue::new(&mut cfg.normal_strength)
                            .range(0.0..=8.0)
                            .speed(0.05),
                    );
                    ui.end_row();
                });

            ui.checkbox(&mut cfg.no_normal, "Skip normal map");
            ui.checkbox(&mut cfg.no_metallic_roughness, "Skip metallic/roughness");
            ui.checkbox(&mut cfg.no_occlusion, "Skip occlusion (AO)");
            ui.checkbox(&mut cfg.force, "Re-generate even if texture file exists");
            ui.checkbox(&mut cfg.no_cache, "Bypass on-disk image cache");
        });
        // Persist whether the expander is open so it survives recompiles.
        self.files[self.active].texture_cfg.expanded = resp.openness > 0.5;

        let tex_enabled = has_key && !busy && !src_empty && has_path;
        if ui
            .add_enabled(tex_enabled, egui::Button::new("Generate Textures"))
            .on_hover_text(
                "Run the textures pipeline with the options above. \
                 Writes PNGs to ./textures/ next to the .mg.",
            )
            .clicked()
        {
            let ctx = ui.ctx().clone();
            self.start_llm_textures(ctx);
        }
        if !has_path && !src_empty {
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                "save the file first — textures writes PNGs next to it",
            );
        }

        if busy {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("waiting for Gemini…");
                if ui
                    .button("Cancel")
                    .on_hover_text(
                        "Stop waiting and discard the result. \
                         The background call may finish but its output is dropped.",
                    )
                    .clicked()
                {
                    self.cancel_active_llm();
                }
            });
        }
    }

    /// Floating overlay buttons drawn on top of the viewport. Keeps the
    /// camera controls within the user's eye line instead of forcing a trip
    /// to the toolbar.
    fn ui_viewport_overlay(&mut self, ctx: &egui::Context, viewport_rect: egui::Rect) {
        egui::Area::new(egui::Id::new("viewport_overlay"))
            .fixed_pos(viewport_rect.left_top() + egui::vec2(8.0, 8.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(ui.visuals().window_fill().linear_multiply(0.85))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .small_button("Frame")
                                .on_hover_text("Re-fit the camera to the scene")
                                .clicked()
                            {
                                self.viewer.frame_view();
                            }
                            ui.label(
                                egui::RichText::new(
                                    "drag: orbit · shift+drag/middle/right: pan · scroll: zoom",
                                )
                                .small()
                                .weak(),
                            );
                        });
                    });
            });
    }

    fn ui_options(&mut self, ctx: &egui::Context) {
        if !self.show_options {
            return;
        }
        let mut open = true;
        let mut close_after = false;
        egui::Window::new("Options")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("Gemini API key");
                ui.label(
                    "Used by Generate / Modify / Animate / Textures. Stored in your user \
                     config directory and persists between sessions.",
                );
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.options_api_key_draft)
                        .password(true)
                        .hint_text("paste key (leave blank to clear)")
                        .desired_width(f32::INFINITY),
                );
                if std::env::var("GEMINI_API_KEY")
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false)
                {
                    ui.add_space(4.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(150, 180, 230),
                        "GEMINI_API_KEY is also set in your environment — \
                         the saved key here takes precedence when non-empty.",
                    );
                }

                ui.add_space(12.0);
                ui.heading("Thinking budget");
                ui.label(
                    "Cap on Gemini's hidden reasoning tokens per call. Higher = better DSL on \
                     hard prompts but slower and more expensive.",
                );
                ui.add_space(6.0);
                let current = self.settings.thinking_level();
                egui::ComboBox::from_id_salt("opts_thinking_level")
                    .selected_text(thinking_level_label(current))
                    .show_ui(ui, |ui| {
                        for level in THINKING_LEVELS {
                            let selected = level == current;
                            if ui
                                .selectable_label(selected, thinking_level_label(level))
                                .clicked()
                                && !selected
                            {
                                self.settings.thinking_level =
                                    thinking_level_key(level).to_string();
                            }
                        }
                    });

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.settings.gemini_api_key =
                            self.options_api_key_draft.trim().to_string();
                        match self.settings.save() {
                            Ok(()) => {
                                let msg = if self.settings.gemini_api_key.is_empty() {
                                    "options: cleared saved Gemini API key".to_string()
                                } else {
                                    "options: settings saved".to_string()
                                };
                                self.active_mut().status = msg;
                                close_after = true;
                            }
                            Err(e) => {
                                self.active_mut().status = format!("options: save failed: {e}");
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close_after = true;
                    }
                });
            });
        if !open || close_after {
            self.show_options = false;
        }
    }
}

impl eframe::App for MgenApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain LLM completions and run any pending compile from the editor's
        // debounce window before painting.
        self.poll_llm();
        self.drive_compile_debounce(ctx);

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.ui_toolbar(ui));
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
            });
        });
        egui::SidePanel::left("sidebar")
            .default_width(190.0)
            .show(ctx, |ui| self.ui_sidebar(ui));
        egui::SidePanel::right("inspector")
            .default_width(340.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // CollapsingHeader so users can fold what they don't
                        // need; keeps the most-actioned section (LLM) reachable
                        // without scrolling past three other groups.
                        egui::CollapsingHeader::new("Diagnostics")
                            .default_open(true)
                            .show(ui, |ui| self.ui_diagnostics(ui));
                        egui::CollapsingHeader::new("Scene")
                            .default_open(true)
                            .show(ui, |ui| self.ui_summary(ui));
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

        egui::TopBottomPanel::top("editor")
            .resizable(true)
            .default_height(260.0)
            .min_height(80.0)
            .show(ctx, |ui| self.ui_editor(ui));

        let mut viewport_rect: Option<egui::Rect> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::Frame::canvas(ui.style()).show(ui, |ui| {
                let resp = self.viewer.show(ui);
                viewport_rect = Some(resp.rect);
            });
        });
        if let Some(r) = viewport_rect {
            self.ui_viewport_overlay(ctx, r);
        }

        // After the editor has rendered for the active tab, snapshot its
        // camera back into the FileState so future tab switches restore it.
        let snap = self.viewer.camera_snapshot();
        self.files[self.active].camera = Some(snap);

        self.ui_options(ctx);

        // Keep repainting while ANY file has an LLM call in flight so every
        // spinner ticks and completions land promptly regardless of which tab
        // is active.
        if self.any_in_flight() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Some(gl) = gl {
            self.viewer.destroy(gl);
        }
    }
}

/// Walk upward from the CWD until we find the workspace root (the dir that
/// contains `examples/` and `Cargo.toml`). Falls back to CWD when unfound.
fn locate_project_root() -> PathBuf {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut cur = start.as_path();
    loop {
        if cur.join("examples").is_dir() && cur.join("Cargo.toml").is_file() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return start,
        }
    }
}

fn scan_mg_dir(root: &Path, subdir: &str) -> Vec<PathBuf> {
    let dir = root.join(subdir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("mg"))
        .collect();
    out.sort();
    out
}

/// `(material_name, slot_name, authored_path)` triples for every populated
/// texture slot on every material in the scene. Ordered deterministically
/// (material order × slot order) so the UI doesn't jitter between rebuilds.
fn gather_texture_refs(scene: &mgen_core::SceneGraph) -> Vec<(String, &'static str, PathBuf)> {
    const SLOTS: [&str; 5] = [
        "base_color",
        "metallic_roughness",
        "normal",
        "occlusion",
        "emissive",
    ];
    let mut out = Vec::new();
    for m in &scene.materials {
        let refs = [
            &m.base_color_texture,
            &m.metallic_roughness_texture,
            &m.normal_texture,
            &m.occlusion_texture,
            &m.emissive_texture,
        ];
        for (slot, r) in SLOTS.iter().zip(refs.iter()) {
            if let Some(t) = r {
                out.push((m.name.clone(), *slot, t.path.clone()));
            }
        }
    }
    out
}

fn resolve_for_check(path: &Path, base: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match base {
        Some(b) => b.join(path),
        None => path.to_path_buf(),
    }
}

/// Show "…/dir/filename.png", keeping the filename intact and ellipsizing
/// the directory prefix from the left if the whole thing is too long.
fn ellipsize_path(path: &Path, max_chars: usize) -> String {
    let s = path.to_string_lossy();
    let n = s.chars().count();
    if n <= max_chars {
        return s.into_owned();
    }
    // Always keep the filename intact — that's the part the user actually
    // recognizes. Trim the prefix and prepend an ellipsis.
    let file_chars = path
        .file_name()
        .map(|f| f.to_string_lossy().chars().count())
        .unwrap_or(0);
    if file_chars + 1 >= max_chars {
        // Filename alone is too long; keep its tail.
        let tail: String = s.chars().rev().take(max_chars.saturating_sub(1)).collect();
        let tail: String = tail.chars().rev().collect();
        return format!("…{tail}");
    }
    let keep = max_chars.saturating_sub(file_chars + 1); // 1 for ellipsis
    let prefix_chars = n.saturating_sub(file_chars);
    let drop = prefix_chars.saturating_sub(keep);
    let visible: String = s.chars().skip(drop).collect();
    format!("…{visible}")
}

fn offset_to_line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in src.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn run_llm(
    kind: LlmKind,
    prompt: String,
    existing: Option<String>,
    api_key: String,
    thinking: ThinkingLevel,
    sys_instr: Arc<String>,
) -> LlmOutcome {
    let client = GeminiClient::new(api_key);
    let seed = existing
        .as_deref()
        .and_then(parse_seed_header)
        .unwrap_or_else(pick_default_seed);

    let user_prompt = match kind {
        LlmKind::Generate => prompt.clone(),
        LlmKind::Modify => format!(
            "You are editing an existing mgen DSL file. Apply this modification:\n\n\
            {mod_prompt}\n\n\
            Make the smallest edit that satisfies the request. Do not rename, reorder, \
            reformat, or restyle parts the modification does not touch.\n\n\
            Reply with ONLY the full modified DSL — no commentary, no markdown fences. \
            Do not include the `// mgen-generate` header comments; the caller re-adds them.\n\n\
            Existing file:\n\n{existing}",
            mod_prompt = prompt.trim(),
            existing = existing.as_deref().unwrap_or("").trim_end(),
        ),
        LlmKind::Animate => format!(
            "You are editing an existing mgen DSL file. APPEND new animation and rigging \
            declarations to satisfy this request:\n\n\
            {anim_prompt}\n\n\
            mgen supports two rigging strategies. Pick the SIMPLER one that fits the request:\n\n\
            A) Node-transform animation (for articulations that can be expressed as rigid \
            transforms of existing scene nodes — door hinges, wheels, rotors, pistons, \
            breathing). Place these at the top level of the file (outside `scene {{ … }}`):\n\
              • `joint \"name\" (type=hinge|slider|ball|rotor, axis=[x,y,z], pivot=\"node\", limits=[lo,hi])`\n\
              • `clip \"name\" (seconds=N) {{ track \"joint_or_node\" (from=0, to=V, prop=\"rotation\"|\"translation\"|\"scale\") }}`\n\
              • procedural templates (one-liners): `spin`, `open_close`, `wave`, `flap`, `idle`\n\
                e.g. `spin \"rotor_spin\" (target=\"rotor\", axis=[0,0,1], rpm=30)`\n\
                     `open_close \"door_swing\" (target=\"door_hinge\", angle=90, seconds=1.2)`\n\
            When a template targets a scene node directly (not a joint), it MUST pass an \
            explicit `axis` (except `idle`, which is a scale breathe with no axis).\n\n\
            B) Skeletal skinning (for meshes that must deform smoothly — limbs bending, \
            tails whipping, any continuous body). Declare a `skeleton` INSIDE `scene {{ … }}` \
            and bind a primitive to it by adding `skin=\"skel_name\"` to its attrs:\n\
              • `skeleton \"skel_name\" {{ bone \"b1\" (pos=[x,y,z], envelope=R) {{ bone \"b2\" (pos=[…], envelope=R) {{ … }} }} }}`\n\
                — bones nest to form the chain; `pos` is RELATIVE to the parent bone; `envelope` \
                is the radius (in world units) within which vertices get weight from this bone.\n\
              • Any primitive in the same scene can bind to it by adding `skin=\"skel_name\"` \
                to its attribute list (e.g. `cylinder \"arm\" (…, skin=\"skel_name\")`). \
                Weights are assigned automatically by nearest-bone envelope falloff.\n\
              • Drive the deformation by rotating the bone scene nodes via a `clip` with \
                `track \"bone_name\" (prop=rotation, from=0, to=…)`. `from`/`to` are in \
                degrees when `prop=rotation`.\n\
            Minimal skinned example:\n\
              ```\n\
              scene {{\n\
                skeleton \"arm_skel\" {{\n\
                  bone \"shoulder\" (pos=[0,0,0], envelope=0.75) {{\n\
                    bone \"elbow\" (pos=[0,0.5,0], envelope=0.75)\n\
                  }}\n\
                }}\n\
                cylinder \"arm_mesh\" (pos=[0,0.5,0], radius=0.12, height=1.0, skin=\"arm_skel\")\n\
              }}\n\
              clip \"swing\" (seconds=1.0) {{ track \"elbow\" (prop=rotation, from=0, to=60) }}\n\
              ```\n\n\
            RULES:\n\
            - Prefer (A) for any rig the user describes in terms of hinges/sliders/spins. \
              Only reach for (B) when the request implies smooth continuous deformation of a \
              single mesh.\n\
            - Do not touch geometry. Preserve every `scene`, `material`, `mesh`, `primitive`, \
              `group`, `array`, `mirror`, `attach`, `connector`, `socket`, `plug`, `use`, and \
              `module` exactly as written — except you MAY add a single `skin=\"…\"` attribute \
              to the one primitive that a new (B)-style rig deforms.\n\
            - Preserve every existing `joint`, `clip`, `skeleton`, `spin`, `open_close`, \
              `wave`, `flap`, and `idle` declaration exactly as written. ADD new ones \
              alongside them; do not rewrite, rename, merge, or delete existing animation \
              or rigging. Only modify an existing declaration if the user's request \
              explicitly names it and asks to change it.\n\
            - Every animation `target=`, `joint pivot=`, and `track` name must reference a \
              node that already exists in the scene (bones become scene nodes once the \
              `skeleton` block is added). Do not invent or rename other nodes.\n\
            - New `joint`, `clip`, `skeleton`, and template names must not collide with \
              existing ones — pick a fresh unique name.\n\n\
            Reply with ONLY the full updated DSL — no commentary, no markdown fences. Do \
            not include the `// mgen-generate` header comments; the caller re-adds them.\n\n\
            Existing file:\n\n{existing}",
            anim_prompt = prompt.trim(),
            existing = existing.as_deref().unwrap_or("").trim_end(),
        ),
        LlmKind::Textures => unreachable!("run_llm is text-only; textures uses run_llm_textures"),
    };

    let mut cfg = GenerateConfig::new(user_prompt);
    cfg.model = DEFAULT_MODEL.to_string();
    cfg.seed = Some(seed);
    cfg.thinking_level = Some(thinking);
    cfg.system_instruction = Some((*sys_instr).clone());

    let repair = RepairConfig {
        max_iters: 2,
        on_iteration: None,
    };

    match generate_with_repair(&client, cfg, &repair) {
        Ok(outcome) => {
            let wrapped = embed_seed_header(&outcome.dsl, seed, &prompt);
            LlmOutcome {
                dsl: wrapped,
                diagnostics: outcome.diagnostics,
                tokens: outcome.usage.total_tokens,
                calls: outcome.call_count,
                error: None,
                kind,
            }
        }
        Err(e) => LlmOutcome {
            dsl: existing.unwrap_or_default(),
            diagnostics: Vec::new(),
            tokens: 0,
            calls: 0,
            error: Some(e.to_string()),
            kind,
        },
    }
}

/// Run the textures pipeline (image generation + splice) on a background
/// thread and shape the result into an [`LlmOutcome`] so it rides the same
/// channel as the text-LLM paths. Reports "PNGs written" in the `calls` slot
/// so `poll_llm` can display a counter without adding a new field.
///
/// Note: parsing and `build_plan` happen on this thread too — the previous
/// version did them on the UI thread before spawning, which stalled the
/// frame for big scenes.
fn run_llm_textures(
    src: String,
    mg_path: PathBuf,
    api_key: String,
    cfg: TextureUiConfig,
) -> LlmOutcome {
    let ast = match mgen_dsl::parse(&src) {
        Ok(a) => a,
        Err(e) => {
            return LlmOutcome {
                dsl: src,
                diagnostics: Vec::new(),
                tokens: 0,
                calls: 0,
                error: Some(format!("parse: {e}")),
                kind: LlmKind::Textures,
            };
        }
    };

    let args = TexturesArgs {
        input: mg_path.clone(),
        out: None,
        glb: None,
        textures_dir: PathBuf::from("textures"),
        style: cfg.style.clone(),
        model: DEFAULT_IMAGE_MODEL.to_string(),
        force: cfg.force,
        dry_run: false,
        no_build: true,
        no_cache: cfg.no_cache,
        api_key: Some(api_key.clone()),
        no_pbr: false,
        no_normal: cfg.no_normal,
        no_metallic_roughness: cfg.no_metallic_roughness,
        no_occlusion: cfg.no_occlusion,
        normal_strength: cfg.normal_strength,
        texture_size: cfg.texture_size,
    };

    let cache = maybe_cache(cfg.no_cache);
    let plans = build_plan(&src, &ast, &args, cache.as_ref());

    // If nothing needs generating *or* deriving, leave the source untouched so
    // the editor doesn't get marked dirty.
    let anything_to_do = plans.iter().any(|p| {
        matches!(
            p.action,
            PlanAction::Generate | PlanAction::CacheHit | PlanAction::Derive
        )
    });
    if !anything_to_do {
        return LlmOutcome {
            dsl: src,
            diagnostics: Vec::new(),
            tokens: 0,
            calls: 0,
            error: Some("every material already has a full PBR texture set".into()),
            kind: LlmKind::Textures,
        };
    }

    let client = GeminiClient::new(api_key);
    let base_dir = mg_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let edits = match run_plan(
        Some(&client),
        &args.model,
        &args,
        &ast,
        &plans,
        &base_dir,
        cache.as_ref(),
    ) {
        Ok(e) => e,
        Err(e) => {
            return LlmOutcome {
                dsl: src,
                diagnostics: Vec::new(),
                tokens: 0,
                calls: 0,
                error: Some(e.to_string()),
                kind: LlmKind::Textures,
            };
        }
    };

    match splice_textures(&src, &edits) {
        Ok(new_src) => LlmOutcome {
            dsl: new_src,
            diagnostics: Vec::new(),
            tokens: 0,
            calls: edits.len() as u32,
            error: None,
            kind: LlmKind::Textures,
        },
        Err(e) => LlmOutcome {
            dsl: src,
            diagnostics: Vec::new(),
            tokens: 0,
            calls: 0,
            error: Some(format!("splice: {e}")),
            kind: LlmKind::Textures,
        },
    }
}

fn pick_default_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5EED)
}
