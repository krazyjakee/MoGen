use std::fs;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use mogen_core::Severity;
use mogen_llm::{system_instruction, StdlibIndex};

use mogen_llm::textures::TextureStage;

use super::pricing::{cost_images, cost_text, format_usd, image_pricing, text_pricing};
use super::types::{
    EnhanceInFlight, EnhanceTarget, LlmEvent, LlmEventTone, LlmKind, LlmMessage, LlmOutcome,
    LlmProgress, LLM_EVENT_CAP,
};
use super::util::{run_llm, run_llm_textures, run_prompt_enhance, LlmRunConfig};
use super::MogenStudioApp;

impl MogenStudioApp {
    pub(super) fn start_llm_generate(&mut self, ctx: egui::Context) {
        let prompt = self.active().gen_prompt.trim().to_string();
        let image = self.active().gen_image.clone();
        if prompt.is_empty() && image.is_none() {
            self.active_mut().status = "enter a prompt first".into();
            return;
        }
        self.spawn_llm(ctx, LlmKind::Generate, prompt, None, image);
    }

    pub(super) fn start_llm_modify(&mut self, ctx: egui::Context) {
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
        self.spawn_llm(ctx, LlmKind::Modify, prompt, Some(existing), None);
    }

    /// Kick off a Gemini repair call on the current buffer. No prompt field —
    /// the diagnostics are the prompt. No-ops (with a friendly status) when
    /// the source is empty or already validates.
    pub(super) fn start_llm_repair(&mut self, ctx: egui::Context) {
        let (src_empty, existing) = {
            let f = self.active();
            (f.source.trim().is_empty(), f.source.clone())
        };
        if src_empty {
            self.active_mut().status = "repair needs existing DSL to fix".into();
            return;
        }
        // Peek at validation before spending tokens. If there are no errors
        // there is nothing to repair — tell the user and bail.
        let diags = mogen_llm::validate_text(&existing);
        if !mogen_core::has_errors(&diags) {
            self.active_mut().status = "repair: no errors to fix".into();
            return;
        }
        // `spawn_llm` stashes the prompt into `llm_last_prompt` for Retry.
        // For Repair there's no natural prompt string, so use a synthetic
        // label — Retry will just re-read the current source and diagnostics.
        self.spawn_llm(
            ctx,
            LlmKind::Repair,
            "repair validation errors".to_string(),
            Some(existing),
            None,
        );
    }

    pub(super) fn start_llm_animate(&mut self, ctx: egui::Context) {
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
        self.spawn_llm(ctx, LlmKind::Animate, prompt, Some(existing), None);
    }

    pub(super) fn start_llm_textures(&mut self, ctx: egui::Context) {
        self.start_llm_textures_inner(ctx, None);
    }

    /// Right-click → Regenerate on a single texture preview lands here. The
    /// pipeline runs with `force=true` (overridden inside `run_llm_textures`)
    /// and a one-element material filter, so only that material's slots are
    /// touched even if the rest of the scene is fully textured.
    pub(super) fn start_llm_textures_for_material(
        &mut self,
        ctx: egui::Context,
        material: String,
    ) {
        self.start_llm_textures_inner(ctx, Some(vec![material]));
    }

    fn start_llm_textures_inner(
        &mut self,
        ctx: egui::Context,
        material_filter: Option<Vec<String>>,
    ) {
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
            self.active_mut().status = "textures needs an open .mog".into();
            return;
        }
        let Some(path) = path_opt else {
            self.active_mut().status =
                "save the file first — textures writes PNGs next to it".into();
            return;
        };
        // Texture generation is Gemini-only (no other backend has an image
        // synthesis API in mogen-llm), so this path bypasses the active
        // provider and reads `GEMINI_API_KEY` directly.
        let api_key = match self.resolve_gemini_api_key() {
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
        let banner = match &material_filter {
            Some(m) if m.len() == 1 => {
                format!("regenerating textures for \"{}\"…", m[0])
            }
            _ => "generating textures with Gemini Image…".to_string(),
        };
        af.llm_progress = Some(LlmProgress::Status(banner.clone()));
        af.llm_started_at = Some(Instant::now());
        af.llm_events.clear();
        af.llm_events.push(LlmEvent {
            at: Instant::now(),
            text: match &material_filter {
                Some(m) if m.len() == 1 => format!("regenerating textures for \"{}\"", m[0]),
                _ => "starting texture pipeline".into(),
            },
            tone: LlmEventTone::Info,
        });
        af.llm_error = None;
        af.status = banner;

        let worker_tx = tx.clone();
        std::thread::spawn(move || {
            let outcome = run_llm_textures(src, path, api_key, cfg, material_filter, worker_tx);
            let _ = tx.send(LlmMessage::Done(outcome));
            ctx.request_repaint();
        });
    }

    /// Retry the most recent failed call on the active file. Carries forward
    /// the kind + prompt stored on `llm_last_prompt` so the user doesn't have
    /// to re-type the prompt — they can edit the draft textarea first.
    pub(super) fn retry_active_llm(&mut self, ctx: egui::Context) {
        let (kind, draft_prompt) = match &self.active().llm_last_prompt {
            Some((k, p)) => (*k, p.clone()),
            None => return,
        };
        // The user may have edited the prompt textarea between runs; prefer
        // whatever's in the field now. Fall back to the stored draft if it's
        // been cleared.
        let current_prompt = match kind {
            LlmKind::Generate => self.active().gen_prompt.clone(),
            LlmKind::Modify => self.active().mod_prompt.clone(),
            LlmKind::Animate => self.active().anim_prompt.clone(),
            LlmKind::Repair | LlmKind::Textures => String::new(),
        };
        let prompt = if current_prompt.trim().is_empty() {
            draft_prompt
        } else {
            current_prompt
        };

        // Clear the error banner so the retry path doesn't show the old one.
        self.active_mut().llm_error = None;

        match kind {
            LlmKind::Generate => {
                self.active_mut().gen_prompt = prompt;
                self.start_llm_generate(ctx);
            }
            LlmKind::Modify => {
                self.active_mut().mod_prompt = prompt;
                self.start_llm_modify(ctx);
            }
            LlmKind::Animate => {
                self.active_mut().anim_prompt = prompt;
                self.start_llm_animate(ctx);
            }
            LlmKind::Repair => {
                self.start_llm_repair(ctx);
            }
            LlmKind::Textures => {
                self.start_llm_textures(ctx);
            }
        }
    }

    /// Kick off a background prompt-enhancement call for `target`. No-ops when
    /// another enhance is already in flight (single app-level slot), the
    /// source field is empty, or no API key is available (the caller surfaces
    /// a warning next to the button). On failure a per-target error string is
    /// stashed on the app for the button's label row to render.
    pub(super) fn start_prompt_enhance(
        &mut self,
        ctx: egui::Context,
        target: EnhanceTarget,
    ) {
        if self.enhance_in_flight.is_some() {
            return;
        }
        let input = self.read_enhance_source(target);
        let trimmed = input.trim();
        if trimmed.is_empty() {
            self.enhance_error =
                Some((target, "enter a prompt first".into()));
            return;
        }
        let provider = self.settings.provider();
        let api_key = match self.resolve_api_key() {
            Some(k) => k,
            None => {
                self.enhance_error = Some((
                    target,
                    format!(
                        "no {} API key — set one in Edit → Preferences…",
                        provider.label(),
                    ),
                ));
                return;
            }
        };
        let model = self.settings.provider_fast_model();
        let claude_code_path = self.settings.claude_code_path();
        let file_index = self.active;
        // Clear any stale error for this target; a fresh attempt gets a fresh
        // error slot.
        if matches!(&self.enhance_error, Some((t, _)) if *t == target) {
            self.enhance_error = None;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        self.enhance_in_flight = Some(EnhanceInFlight {
            target,
            file_index,
            rx,
        });

        let payload = trimmed.to_string();
        std::thread::spawn(move || {
            let result = run_prompt_enhance(
                target,
                payload,
                provider,
                api_key,
                model,
                claude_code_path,
            );
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// Drain the single in-flight enhance slot. Writes the rewritten prompt
    /// back into the original field on success, or records a per-target error
    /// string on failure. Runs alongside `poll_llm` every frame.
    pub(super) fn poll_prompt_enhance(&mut self) {
        let Some(slot) = self.enhance_in_flight.as_ref() else {
            return;
        };
        let result = match slot.rx.try_recv() {
            Ok(r) => r,
            Err(_) => return,
        };
        let target = slot.target;
        let file_index = slot.file_index;
        self.enhance_in_flight = None;
        match result {
            Ok(text) => {
                self.write_enhance_target(target, file_index, text);
                self.enhance_error = None;
            }
            Err(err) => {
                self.enhance_error = Some((target, err));
            }
        }
    }

    /// True when a prompt-enhance call is in flight — used alongside
    /// `any_in_flight` to keep the repaint heartbeat ticking.
    pub(super) fn any_enhance_in_flight(&self) -> bool {
        self.enhance_in_flight.is_some()
    }

    /// Read the current text of whichever field `target` points at. Generate
    /// reads the app-level modal draft; the rest read the active file.
    fn read_enhance_source(&self, target: EnhanceTarget) -> String {
        match target {
            EnhanceTarget::Generate => self.new_prompt_draft.clone(),
            EnhanceTarget::Modify => self.active().mod_prompt.clone(),
            EnhanceTarget::Animate => self.active().anim_prompt.clone(),
            EnhanceTarget::TextureStyle => self.active().texture_cfg.style.clone(),
        }
    }

    /// Write the enhanced text back into the target field, replacing whatever
    /// the user had. `file_index` is the tab that owned the field when the
    /// call started — if it no longer exists (tab closed mid-call) the write
    /// is dropped silently rather than clobbering a different file.
    fn write_enhance_target(
        &mut self,
        target: EnhanceTarget,
        file_index: usize,
        text: String,
    ) {
        match target {
            EnhanceTarget::Generate => {
                self.new_prompt_draft = text;
            }
            EnhanceTarget::Modify => {
                if let Some(f) = self.files.get_mut(file_index) {
                    f.mod_prompt = text;
                }
            }
            EnhanceTarget::Animate => {
                if let Some(f) = self.files.get_mut(file_index) {
                    f.anim_prompt = text;
                }
            }
            EnhanceTarget::TextureStyle => {
                if let Some(f) = self.files.get_mut(file_index) {
                    f.texture_cfg.style = text;
                }
            }
        }
    }

    /// Drop the receiver for the active file's in-flight LLM call. The worker
    /// thread keeps running but its result is discarded silently — there's no
    /// portable way to abort an in-progress HTTP request through reqwest's
    /// blocking client.
    pub(super) fn cancel_active_llm(&mut self) {
        let f = self.active_mut();
        if f.llm_in_flight.is_none() {
            return;
        }
        f.llm_rx = None;
        f.llm_in_flight = None;
        f.llm_progress = None;
        f.llm_started_at = None;
        f.llm_events.clear();
        f.status = "llm: cancelled (background call may still finish but result is dropped)".into();
    }

    /// Prefer a key saved in Options for the active provider; fall back to the
    /// matching environment variable so existing shell-exported setups keep
    /// working. For keyless providers (Ollama, Claude Code), returns
    /// `Some(String::new())` even when neither setting nor env var is set so
    /// callers can construct a keyless `LlmClient`.
    pub(super) fn resolve_api_key(&self) -> Option<String> {
        let provider = self.settings.provider();
        if let Some(k) = self.settings.provider_api_key() {
            return Some(k.to_string());
        }
        let env_var = provider.env_var();
        if !env_var.is_empty() {
            let env_key = std::env::var(env_var)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            if env_key.is_some() {
                return env_key;
            }
        }
        if provider.is_keyless() {
            return Some(String::new());
        }
        None
    }

    /// Provider-agnostic Gemini key resolution for paths that hard-require
    /// Gemini regardless of the active provider — currently just texture
    /// image generation. Mirrors [`Self::resolve_api_key`] but always reads
    /// the persisted `gemini_api_key` and the `GEMINI_API_KEY` env var.
    pub(super) fn resolve_gemini_api_key(&self) -> Option<String> {
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
    pub(super) fn cached_system_instruction(&mut self) -> Arc<String> {
        if self.system_instruction_cache.is_none() {
            self.system_instruction_cache = Some(Arc::new(system_instruction(
                &StdlibIndex::from_registry(mogen_dsl::stdlib_registry()),
            )));
        }
        self.system_instruction_cache.as_ref().unwrap().clone()
    }

    /// Snapshot the current LLM tuning knobs from the Settings into the
    /// thread-bound struct the worker consumes. Kept here (and not in
    /// Settings) so defaulting logic lives next to the worker.
    pub(super) fn build_run_config(&self) -> LlmRunConfig {
        LlmRunConfig {
            model: self.settings.provider_model(),
            thinking: self.settings.thinking_level(),
            temperature: self.settings.temperature(),
            max_repair_iters: self.settings.max_repair_iters(),
            seed_override: self.settings.seed_override(),
            claude_code_path: self.settings.claude_code_path(),
        }
    }

    pub(super) fn spawn_llm(
        &mut self,
        ctx: egui::Context,
        kind: LlmKind,
        prompt: String,
        existing: Option<String>,
        image: Option<crate::app::types::GenImageInput>,
    ) {
        let provider = self.settings.provider();
        let api_key = match self.resolve_api_key() {
            Some(k) => k,
            None => {
                self.active_mut().status = format!(
                    "no {} API key — set one in Options… or export {}",
                    provider.label(),
                    provider.env_var(),
                );
                return;
            }
        };

        let mut run_cfg = self.build_run_config();
        // Per-file thinking override wins over the global default. Persisted
        // into the `.mog` header so switching files reads back the last pick.
        if let Some(level) = self.active().thinking_override {
            run_cfg.thinking = level;
        }
        let sys_instr = self.cached_system_instruction();

        let provider_label = provider.label();
        let (tx, rx) = std::sync::mpsc::channel();
        let f = self.active_mut();
        f.llm_rx = Some(rx);
        f.llm_in_flight = Some(kind);
        f.llm_progress = Some(LlmProgress::Status(match kind {
            LlmKind::Generate => format!("calling {provider_label} (generate)…"),
            LlmKind::Modify => format!("calling {provider_label} (modify)…"),
            LlmKind::Animate => format!("calling {provider_label} (animate)…"),
            LlmKind::Repair => format!("calling {provider_label} (repair)…"),
            LlmKind::Textures => unreachable!("spawn_llm is text-only"),
        }));
        f.llm_started_at = Some(Instant::now());
        f.llm_events.clear();
        f.llm_events.push(LlmEvent {
            at: Instant::now(),
            text: match kind {
                LlmKind::Generate => "starting generate".into(),
                LlmKind::Modify => "starting modify".into(),
                LlmKind::Animate => "starting animate".into(),
                LlmKind::Repair => "starting repair".into(),
                LlmKind::Textures => unreachable!(),
            },
            tone: LlmEventTone::Info,
        });
        f.llm_error = None;
        f.llm_last_prompt = Some((kind, prompt.clone()));
        f.status = match kind {
            LlmKind::Generate => format!("calling {provider_label} (generate)…"),
            LlmKind::Modify => format!("calling {provider_label} (modify)…"),
            LlmKind::Animate => format!("calling {provider_label} (animate)…"),
            LlmKind::Repair => format!("calling {provider_label} (repair)…"),
            // Textures takes its own path via `start_llm_textures` and never
            // reaches spawn_llm.
            LlmKind::Textures => unreachable!("spawn_llm is text-only"),
        };

        let worker_tx = tx.clone();
        // The thumbnail handle is GUI-only; convert to the LLM crate's
        // `ImageInput` shape so the worker thread carries just the bytes.
        let llm_image = image.map(|img| mogen_llm::ImageInput {
            mime_type: img.mime_type,
            data: img.data,
        });
        std::thread::spawn(move || {
            let outcome = run_llm(
                kind,
                prompt,
                existing,
                provider,
                llm_image,
                api_key,
                run_cfg,
                sys_instr,
                worker_tx,
            );
            let _ = tx.send(LlmMessage::Done(outcome));
            ctx.request_repaint();
        });
    }

    /// Drain any messages from every open file's LLM worker. Both progress
    /// updates and the final `Done` flow on the same channel, so we handle
    /// them together: progress updates the spinner caption; `Done` applies
    /// the outcome to the file.
    pub(super) fn poll_llm(&mut self) {
        // Collect each file's pending messages without holding a borrow on
        // `self.files` across the apply step (which reborrows mutably).
        let mut to_apply: Vec<(usize, LlmOutcome)> = Vec::new();
        for i in 0..self.files.len() {
            let mut progress_updates: Vec<LlmProgress> = Vec::new();
            let mut done: Option<LlmOutcome> = None;
            if let Some(rx) = self.files[i].llm_rx.as_ref() {
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        LlmMessage::Progress(p) => progress_updates.push(p),
                        LlmMessage::Done(o) => {
                            done = Some(o);
                            // Drop any remaining progress after Done — it's
                            // from the same run and the outcome already knows
                            // the final state.
                            break;
                        }
                    }
                }
            }
            // Accumulate every progress event into the file's timeline so the
            // progress card can show a short history, not just the latest
            // line. `llm_progress` still tracks the most recent event since
            // it drives the headline stage caption.
            for p in &progress_updates {
                let (text, tone) = event_for_progress(p);
                let now = Instant::now();
                // Coalesce spammy duplicates (same text back-to-back) — the
                // texture worker emits Deriving events per-slot which compress
                // to one line in the UI.
                let f = &mut self.files[i];
                if f.llm_events.last().map(|e| e.text == text).unwrap_or(false) {
                    if let Some(last) = f.llm_events.last_mut() {
                        last.at = now;
                    }
                } else {
                    f.llm_events.push(LlmEvent { at: now, text, tone });
                    if f.llm_events.len() > LLM_EVENT_CAP {
                        let drop = f.llm_events.len() - LLM_EVENT_CAP;
                        f.llm_events.drain(0..drop);
                    }
                }
            }
            if let Some(last) = progress_updates.pop() {
                self.files[i].llm_progress = Some(last);
            }
            if let Some(o) = done {
                to_apply.push((i, o));
            }
        }
        for (i, outcome) in to_apply {
            self.apply_llm_outcome(i, outcome);
        }
    }

    pub(super) fn apply_llm_outcome(&mut self, i: usize, outcome: LlmOutcome) {
        let f = &mut self.files[i];
        f.llm_rx = None;
        f.llm_in_flight = None;
        f.llm_progress = None;
        f.llm_started_at = None;
        f.llm_events.clear();

        // Roll the usage into the session meter regardless of success: a
        // failed repair iteration still consumed tokens.
        let price = text_pricing(&outcome.model);
        let text_cost = cost_text(&outcome.usage, price);
        let image_price = image_pricing(&outcome.model);
        let image_cost = cost_images(outcome.image_calls, image_price);
        if outcome.calls > 0 || outcome.usage.total_tokens > 0 {
            self.session_usage
                .add_text(&outcome.usage, outcome.calls, text_cost);
        }
        if outcome.image_calls > 0 {
            self.session_usage
                .add_image(outcome.image_calls, image_cost);
        }

        if let Some(info) = outcome.error {
            // Preserve retry_prompt so the user can re-submit without re-typing.
            if let Some(p) = outcome.retry_prompt {
                f.llm_last_prompt = Some((outcome.kind, p));
            }
            let short = info.headline.clone();
            f.llm_error = Some(info);
            f.status = format!("{}: {short}", outcome.kind.label());
            return;
        }

        // Remember the prompt so a post-success Retry (e.g. to re-roll the
        // seed) can reuse it.
        if let Some(p) = outcome.retry_prompt {
            f.llm_last_prompt = Some((outcome.kind, p));
        }

        let kind_label = outcome.kind.label();

        // Drop the returned DSL into the file's buffer so the user can inspect
        // / save it even when validation later fails.
        f.source = outcome.dsl;
        f.dirty = f.source != f.last_saved_source;
        f.needs_compile = true;
        f.last_edit_at = Some(Instant::now());

        // Resync the per-file thinking override from the freshly-written
        // header so the dropdown reflects whatever the run actually used
        // (covers CLI/global fallback too).
        f.thinking_override = mogen_llm::parse_thinking_header(&f.source);

        // Textures wrote PNG files next to the .mog; persist the spliced DSL
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

        // Only refit the camera when the file that just completed is the one
        // currently on screen — otherwise a background job would yank the
        // user's view out from under them. Generate produces brand-new
        // geometry, so flag the file for a fresh fit; `compile_file` below
        // will re-frame against the new bounding sphere.
        if matches!(outcome.kind, LlmKind::Generate) && i == self.active {
            self.files[i].first_render = true;
        }

        // LLM completions are deliberately NOT undoable — the wholesale
        // source replacement is treated as a "commit" the user has to react
        // to with another LLM run or a manual edit. Break the coalesce chain
        // so a subsequent gizmo / inspector edit doesn't merge into a stack
        // entry whose `before` predates the LLM run.
        self.break_undo_chain(i);

        self.compile_file(i);

        let has_errors = outcome
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, Severity::Error));

        let total_tokens = outcome.usage.total_tokens;
        let status = if has_errors {
            format!(
                "{kind_label}: DSL invalid after {} call(s), {} tokens ({}) — see diagnostics",
                outcome.calls,
                total_tokens,
                format_usd(text_cost),
            )
        } else if matches!(outcome.kind, LlmKind::Textures) {
            format!(
                "textures: wrote {} PNG{} ({} image call(s), {})",
                outcome.calls,
                if outcome.calls == 1 { "" } else { "s" },
                outcome.image_calls,
                format_usd(image_cost),
            )
        } else {
            format!(
                "{kind_label}: ready ({} call(s), {} tokens, {})",
                outcome.calls,
                total_tokens,
                format_usd(text_cost),
            )
        };
        self.files[i].status = status;
    }

    pub(super) fn any_in_flight(&self) -> bool {
        self.files.iter().any(|f| f.llm_in_flight.is_some())
    }

    pub(super) fn count_in_flight(&self) -> usize {
        self.files
            .iter()
            .filter(|f| f.llm_in_flight.is_some())
            .count()
    }
}

/// Collapse an `LlmProgress` into the string + tone that the timeline renders.
/// Kept outside the impl so `ui_llm.rs` can't accidentally call it — the UI
/// reads baked `LlmEvent`s rather than formatting progress variants inline.
fn event_for_progress(p: &LlmProgress) -> (String, LlmEventTone) {
    match p {
        LlmProgress::Status(s) => {
            let tone = if s.contains("done") {
                LlmEventTone::Done
            } else {
                LlmEventTone::Info
            };
            (s.clone(), tone)
        }
        LlmProgress::Repair { iter, max, errors } => (
            format!(
                "repair {iter}/{max} · {errors} error{}",
                if *errors == 1 { "" } else { "s" }
            ),
            LlmEventTone::Repair,
        ),
        LlmProgress::Texture {
            current,
            total,
            material,
            stage,
        } => {
            let verb = match stage {
                TextureStage::Generating => "generating",
                TextureStage::Existing => "using existing PNG",
                TextureStage::Deriving => "deriving PBR",
                TextureStage::Done => "finished",
            };
            (
                format!("{current}/{total} · {verb} — {material}"),
                LlmEventTone::Texture,
            )
        }
    }
}
