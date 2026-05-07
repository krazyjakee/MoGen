use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use mogen_llm::{system_instruction, StdlibIndex};

use crate::app::types::{LlmEvent, LlmEventTone, LlmKind, LlmMessage, LlmProgress};
use crate::app::util::{run_llm, LlmRunConfig};
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Build (or reuse) the LLM system instruction. It pulls in the full
    /// stdlib + grammar so it isn't free; cache the string and clone the Arc
    /// across spawns instead of regenerating per call.
    pub(in crate::app) fn cached_system_instruction(&mut self) -> Arc<String> {
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
    pub(in crate::app) fn build_run_config(&self) -> LlmRunConfig {
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
            plan: self.settings.plan_first(),
            zai_refine_use_vision: self.settings.zai_refine_use_vision(),
        }
    }

    pub(in crate::app) fn spawn_llm(
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
                     (or switch to \"Gemini (API key)\")"
                        .to_string()
                } else if matches!(provider, mogen_llm::Provider::Gemini) {
                    "no Gemini API key — set GEMINI_API_KEY, paste a key in Edit → \
                     Preferences…, or switch to \"Gemini (Google OAuth)\""
                        .to_string()
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
            LlmKind::Refine => unreachable!("refine uses spawn_llm_refine, not spawn_llm"),
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
                LlmKind::Refine => unreachable!(),
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
            // reaches spawn_llm. Refine takes its own path via
            // `spawn_llm_refine`.
            LlmKind::Textures => unreachable!("spawn_llm is text-only"),
            LlmKind::Refine => unreachable!("refine uses spawn_llm_refine, not spawn_llm"),
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

    /// Spawn the worker thread for one visual auto-refinement iteration.
    ///
    /// The render has already happened on the GL thread (called from
    /// `on_refine_render_done`); this just kicks the LLM half — resolves
    /// the credential, snapshots the run config, and posts a worker that
    /// runs [`crate::app::util::run_llm_refine`] and reports back through
    /// the standard `llm_rx` channel.
    ///
    /// `file_index` is bound on entry rather than read from `self.active`
    /// so a concurrent tab switch routes the outcome to the correct file
    /// (the worker thread does not see the active-tab state).
    pub(in crate::app) fn spawn_llm_refine(
        &mut self,
        ctx: egui::Context,
        file_index: usize,
        png: Vec<u8>,
        original_prompt: String,
        current_dsl: String,
    ) {
        let provider = self.settings.provider();
        // Re-check the credential here — `start_llm_refine` checked at
        // session start, but multi-iter sessions can span minutes and
        // the user could have signed out / cleared keys in between.
        // Failing here surfaces through the same status-line + session
        // teardown the GL pre-spawn failures use.
        let credential = match self.resolve_credential() {
            Some(c) => c,
            None => {
                let f = &mut self.files[file_index];
                f.refine_session = None;
                f.llm_in_flight = None;
                f.llm_progress = None;
                f.llm_started_at = None;
                f.llm_events.clear();
                f.status = format!(
                    "refine: lost {} credential mid-session — re-add in Edit → Preferences…",
                    provider.label(),
                );
                return;
            }
        };

        let mut run_cfg = self.build_run_config();
        if let Some(level) = self.files[file_index].thinking_override {
            run_cfg.thinking = level;
        }
        run_cfg.base_dir = self.files[file_index]
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));

        let provider_label = provider.label();
        let pass_label = self.files[file_index]
            .refine_session
            .as_ref()
            .map(|s| {
                format!(
                    "refine {}/{}",
                    s.iters_total - s.iters_remaining + 1,
                    s.iters_total,
                )
            })
            .unwrap_or_else(|| "refine".into());

        let (tx, rx) = std::sync::mpsc::channel();
        let f = &mut self.files[file_index];
        f.llm_rx = Some(rx);
        // llm_in_flight is already Some(LlmKind::Refine) from
        // submit_refine_capture; just refresh the headline.
        f.llm_progress = Some(LlmProgress::Status(format!(
            "calling {provider_label} ({pass_label})…"
        )));
        f.llm_events.push(LlmEvent {
            at: Instant::now(),
            text: format!("calling {provider_label} for reviewer critique"),
            tone: LlmEventTone::Info,
        });
        f.status = format!("calling {provider_label} ({pass_label})…");

        let worker_tx = tx.clone();
        std::thread::spawn(move || {
            let outcome = crate::app::util::run_llm_refine(
                provider,
                credential,
                run_cfg,
                original_prompt,
                current_dsl,
                png,
                worker_tx,
            );
            let _ = tx.send(LlmMessage::Done(outcome));
            ctx.request_repaint();
        });
    }
}
