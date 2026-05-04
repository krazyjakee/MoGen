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
use super::util::{run_llm, run_llm_textures, run_prompt_enhance, Credential, LlmRunConfig};
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

    /// "New textures" banner button lands here. Forces a full regenerate by
    /// flipping `texture_cfg.force=true` on the active file before kicking
    /// off the same pipeline `start_llm_textures` uses, then clears the
    /// banner the button was rendered on so its message doesn't outlive the
    /// click. The cfg flip is persistent — the panel checkbox follows.
    pub(super) fn start_llm_textures_force(&mut self, ctx: egui::Context) {
        {
            let f = self.active_mut();
            f.texture_cfg.force = true;
            f.llm_error = None;
        }
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
        // provider and reads either a stored OAuth bundle (preferred —
        // unlocks `gemini-3-pro-image-preview` on a paid plan) or
        // `GEMINI_API_KEY` directly.
        let cred = match self.resolve_gemini_credential() {
            Some(c) => c,
            None => {
                self.active_mut().status =
                    "texture generation requires Gemini credentials — run \
                     `mogen auth login`, set a key in Options…, or export \
                     GEMINI_API_KEY"
                        .into();
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
            _ => "generating textures…".to_string(),
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
        // Stamp the retry slot so the error banner's Retry button routes back
        // into the textures pipeline instead of falling through to whatever
        // text-LLM kind set `llm_last_prompt` last. The prompt string slot is
        // unused for textures (the call has no free-text prompt) but must be
        // populated for `has_last` in the banner to enable the button.
        let retry_label = match &material_filter {
            Some(m) if !m.is_empty() => format!("regenerate textures for {}", m.join(", ")),
            _ => "regenerate textures".to_string(),
        };
        af.llm_last_prompt = Some((LlmKind::Textures, retry_label));
        af.texture_retry_filter = material_filter.clone();
        af.status = banner;

        let worker_tx = tx.clone();
        std::thread::spawn(move || {
            let outcome = run_llm_textures(src, path, cred, cfg, material_filter, worker_tx);
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
                // If the failed run was a per-material regenerate, retry the
                // same material(s) — otherwise fall back to a full-scene
                // textures pass. The filter lives on the FileState so the
                // retry button surfaces a single click of work.
                let filter = self.active().texture_retry_filter.clone();
                match filter {
                    Some(materials) if !materials.is_empty() => {
                        // `start_llm_textures_for_material` only takes one
                        // name today; widen the loop if the regenerate path
                        // ever picks multiple materials. Single-element call
                        // is the only shape currently produced.
                        if materials.len() == 1 {
                            self.start_llm_textures_for_material(ctx, materials[0].clone());
                        } else {
                            self.start_llm_textures_inner(ctx, Some(materials));
                        }
                    }
                    _ => self.start_llm_textures(ctx),
                }
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
        let credential = match self.resolve_credential() {
            Some(c) => c,
            None => {
                self.enhance_error = Some((
                    target,
                    format!(
                        "no {} credential — set an API key in Edit → Preferences… \
                         (Gemini also accepts `mogen auth login`)",
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
                credential,
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

    /// Resolve the credential for the active provider slot. The slot dictates
    /// which path is taken — there's no fallback between API-key and OAuth
    /// auth for Gemini, picking the slot in Preferences is the explicit
    /// choice.
    ///
    /// - `GeminiOAuth`: load the bundle from `google_auth.json`. No key
    ///   fallback even when one is saved.
    /// - `GeminiApiKey`: settings key → `GEMINI_API_KEY` env. No OAuth
    ///   fallback — the user picked API-key on purpose.
    /// - Other providers: settings key → env var → keyless empty key.
    pub(super) fn resolve_credential(&self) -> Option<Credential> {
        let slot = self.settings.provider_slot();
        if slot.is_gemini_oauth() {
            let path = mogen_llm::token_store_path()?;
            let bundle = mogen_llm::load_bundle(&path).ok().flatten()?;
            return Some(Credential::GeminiOAuth(bundle));
        }
        let provider = slot.to_provider();
        if let Some(k) = self.settings.provider_api_key() {
            return Some(Credential::ApiKey(k.to_string()));
        }
        let env_var = provider.env_var();
        if !env_var.is_empty() {
            if let Some(k) = std::env::var(env_var)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                return Some(Credential::ApiKey(k));
            }
        }
        if provider.is_keyless() {
            return Some(Credential::ApiKey(String::new()));
        }
        None
    }

    /// Back-compat shim that mirrors the old API-key surface for UI gating
    /// (`has_key`-style booleans). Returns `Some(())` whenever a usable
    /// credential exists — including a stored OAuth bundle for Gemini.
    pub(super) fn resolve_api_key(&self) -> Option<()> {
        self.resolve_credential().map(|_| ())
    }

    /// Provider-agnostic Gemini credential resolution for paths that
    /// hard-require Gemini regardless of the active text-LLM provider —
    /// currently just texture image generation.
    ///
    /// Precedence is steered by the user's `image_provider` preference
    /// (Preferences → LLM):
    /// - `Auto` (default): stored Antigravity OAuth bundle → persisted
    ///   `gemini_api_key` → `GEMINI_API_KEY` env → stored gemini-cli OAuth
    ///   bundle (kept last so the UI can surface a clear "wrong client"
    ///   error in `textures_run.rs`).
    /// - `Antigravity`: only the stored Antigravity OAuth bundle is
    ///   considered. Returns `None` when no bundle is on disk.
    /// - `ApiKey`: only the Gemini API key (settings or env) is considered.
    /// - `ZAI`: only the Z.ai key (settings or env) is considered. The
    ///   textures pipeline branches on [`Credential::Zai`] in
    ///   `textures_run.rs` and routes through [`mogen_llm::ImageClient::Zai`].
    pub(super) fn resolve_gemini_credential(&self) -> Option<Credential> {
        use crate::settings::ImageProvider;
        use mogen_llm::google_oauth::{ANTIGRAVITY_CONFIG, GEMINI_CLI_CONFIG};

        let pref = self.settings.image_provider();

        if matches!(pref, ImageProvider::ZAI) {
            if let Some(k) = self.settings.zai_api_key() {
                return Some(Credential::Zai(k.to_string()));
            }
            if let Some(k) = std::env::var("ZAI_API_KEY")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                return Some(Credential::Zai(k));
            }
            return None;
        }

        if matches!(pref, ImageProvider::Auto | ImageProvider::Antigravity) {
            if let Some(path) = mogen_llm::token_store_path_for(&ANTIGRAVITY_CONFIG) {
                if let Ok(Some(bundle)) = mogen_llm::load_bundle(&path) {
                    return Some(Credential::AntigravityOAuth(bundle));
                }
            }
            if matches!(pref, ImageProvider::Antigravity) {
                return None;
            }
        }

        if matches!(pref, ImageProvider::Auto | ImageProvider::ApiKey) {
            if let Some(k) = self.settings.gemini_api_key() {
                return Some(Credential::ApiKey(k.to_string()));
            }
            if let Some(k) = std::env::var("GEMINI_API_KEY")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                return Some(Credential::ApiKey(k));
            }
            if matches!(pref, ImageProvider::ApiKey) {
                return None;
            }
        }

        if let Some(path) = mogen_llm::token_store_path_for(&GEMINI_CLI_CONFIG) {
            if let Ok(Some(bundle)) = mogen_llm::load_bundle(&path) {
                return Some(Credential::GeminiOAuth(bundle));
            }
        }
        None
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
            // Populated per-call by `spawn_llm` from the active file's path
            // so relative `import "X.mog"` lookups resolve correctly.
            base_dir: None,
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
        let slot = self.settings.provider_slot();
        let provider = slot.to_provider();
        let credential = match self.resolve_credential() {
            Some(c) => c,
            None => {
                self.active_mut().status = if slot.is_gemini_oauth() {
                    "no Gemini OAuth token — sign in with Google in Edit → Preferences… \
                     (or switch to \"Gemini (API key)\")".to_string()
                } else if matches!(provider, mogen_llm::Provider::Gemini) {
                    "no Gemini API key — set GEMINI_API_KEY, paste a key in Edit → \
                     Preferences…, or switch to \"Gemini (Google OAuth)\"".to_string()
                } else {
                    format!(
                        "no {} API key — set one in Options… or export {}",
                        provider.label(),
                        provider.env_var(),
                    )
                };
                return;
            }
        };

        let mut run_cfg = self.build_run_config();
        // Per-file thinking override wins over the global default. Persisted
        // into the `.mog` header so switching files reads back the last pick.
        if let Some(level) = self.active().thinking_override {
            run_cfg.thinking = level;
        }
        // Resolve relative `import "X.mog"` paths against the active file's
        // directory so the modify/animate prompts can quote each `use`'s
        // local-frame AABB. Unsaved buffers leave `base_dir` as `None` —
        // imports still get listed verbatim, just without bounds.
        run_cfg.base_dir = self
            .active()
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
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
        // Clear any leftover textures-retry filter from a prior run — without
        // this, a Generate that follows a per-material regenerate could carry
        // the stale material list into a future Textures retry.
        f.texture_retry_filter = None;
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
                credential,
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
    pub(super) fn poll_llm(&mut self, ctx: &egui::Context) {
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
            self.apply_llm_outcome(ctx, i, outcome);
        }
    }

    pub(super) fn apply_llm_outcome(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        outcome: LlmOutcome,
    ) {
        // Snapshot the source as it stood before the LLM result lands so we
        // can splice the change into the editor's native undo history below.
        let pre_source = self.files[i].source.clone();
        let editor_id = egui::Id::new(("mog_editor_textedit", self.files[i].tab_id));

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

        // Textures partial-success: the run finished but some materials
        // failed. `outcome.dsl` already contains the splice for the materials
        // that did succeed and `outcome.calls` is the count of slots written
        // — so we still want to apply the DSL and save it to disk, then
        // surface the failure list as a banner. Any other kind, or a textures
        // run that produced zero edits, falls through the existing
        // hard-failure path.
        let textures_partial_success = matches!(outcome.kind, LlmKind::Textures)
            && outcome.error.is_some()
            && outcome.calls > 0;

        if let Some(info) = outcome.error.clone() {
            if !textures_partial_success {
                // Preserve retry_prompt so the user can re-submit without re-typing.
                if let Some(p) = outcome.retry_prompt {
                    f.llm_last_prompt = Some((outcome.kind, p));
                }
                let short = info.headline.clone();
                f.llm_error = Some(info);
                f.status = format!("{}: {short}", outcome.kind.label());
                return;
            }
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

        // Splice the LLM-driven replacement into the code editor's native
        // TextEdit undo history so Cmd+Z reverts to pre-LLM source and a
        // following Cmd+Shift+Z replays the LLM change. Without this push,
        // re-focusing the editor lets egui's undoer observe the changed
        // buffer mid-flight and clear the redo stack as a side-effect — so
        // changes the user undoes before the LLM runs become unreachable.
        push_llm_change_to_editor_history(ctx, editor_id, pre_source, f.source.clone());

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

        // The editor-side undo history captured the LLM change above. Reset
        // the programmatic (gizmo/inspector) coalesce window so a subsequent
        // transform edit doesn't merge into a stack entry whose `before`
        // predates the LLM run — the two undo surfaces stay independent.
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
            if textures_partial_success {
                format!(
                    "textures: partial — wrote {} PNG{} ({} image call(s), {}); see banner",
                    outcome.calls,
                    if outcome.calls == 1 { "" } else { "s" },
                    outcome.image_calls,
                    format_usd(image_cost),
                )
            } else {
                format!(
                    "textures: wrote {} PNG{} ({} image call(s), {})",
                    outcome.calls,
                    if outcome.calls == 1 { "" } else { "s" },
                    outcome.image_calls,
                    format_usd(image_cost),
                )
            }
        } else {
            format!(
                "{kind_label}: ready ({} call(s), {} tokens, {})",
                outcome.calls,
                total_tokens,
                format_usd(text_cost),
            )
        };
        self.files[i].status = status;

        // Surface the partial-failure banner *after* the DSL has been written
        // and the file recompiled, so the user sees the spliced PNGs in the
        // viewer alongside the "N material(s) failed" notice. `outcome.error`
        // is `Some(_)` on this branch by definition (textures_partial_success).
        if textures_partial_success {
            if let Some(info) = outcome.error {
                self.files[i].llm_error = Some(info);
            }
        }
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
                TextureStage::Failed => "failed",
            };
            (
                format!("{current}/{total} · {verb} — {material}"),
                LlmEventTone::Texture,
            )
        }
    }
}

/// Splice a wholesale LLM-driven source replacement into the editor
/// `TextEdit`'s native undo stack so users can Cmd+Z back to the pre-LLM
/// buffer and Cmd+Shift+Z to replay the LLM change. Two `add_undo`s with a
/// `feed_state` between them ensure pre-LLM is the latest entry, the stale
/// redo stack from any prior in-editor undos is cleared, and post-LLM lands
/// as the new tip — egui's undoer does this automatically only while the
/// widget owns focus, which it does not during an LLM call.
fn push_llm_change_to_editor_history(
    ctx: &egui::Context,
    editor_id: egui::Id,
    pre: String,
    post: String,
) {
    if pre == post {
        return;
    }
    let mut state = egui::TextEdit::load_state(ctx, editor_id).unwrap_or_default();
    let cursor = state.cursor.char_range().unwrap_or_default();
    let mut undoer = state.undoer();
    let now = ctx.input(|i| i.time);
    undoer.add_undo(&(cursor, pre));
    // `feed_state` clears `redos` whenever the new state differs from
    // `undos.back()` — exploited here to drop redo entries that pointed at a
    // timeline the LLM just forked away from. The follow-up `add_undo` is
    // what actually pushes the post-LLM tip onto the stack.
    undoer.feed_state(now, &(cursor, post.clone()));
    undoer.add_undo(&(cursor, post));
    state.set_undoer(undoer);
    state.store(ctx, editor_id);
}
