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
            // Per-call style is set by `spawn_llm` from the active file's
            // captured `gen_style` so Retry / follow-up edits stay
            // consistent. `build_run_config` returns a default-empty value
            // here to keep that boundary clean.
            style: None,
            plan: self.settings.plan_first(),
            zai_base_url: self.settings.zai_base_url().to_string(),
            // Per-call: scene path comes from the active file, session id
            // from the app-wide UUID. `spawn_llm` populates these from the
            // file it's about to call against.
            scene_path: None,
            session_id: self.spend_session_id.clone(),
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
                    "Gemini OAuth: not signed in. Open Edit → Preferences and \
                     sign in with Google, or switch the active provider to \
                     \"Gemini (API key)\"."
                        .to_string()
                } else if matches!(provider, mogen_llm::Provider::Gemini) {
                    "Gemini: no API key. Pick one path: paste a key in \
                     Edit → Preferences, set $GEMINI_API_KEY in your \
                     environment, or switch the active provider to \
                     \"Gemini (Google OAuth)\" and sign in."
                        .to_string()
                } else {
                    format!(
                        "{}: no API key. Paste one in Edit → Preferences or \
                         export ${} in your environment.",
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
        // Per-call style: prefer the file's captured pick (set by the New
        // from Prompt dialog at submit time, or by `loaded()` for an
        // existing styled file) so Retry and follow-up modify/animate
        // calls stay in style. Falls back to the persisted Settings
        // default for fresh, never-styled tabs so the dropdown's last
        // pick still applies on the first generate of a new session.
        run_cfg.style = self
            .active()
            .gen_style
            .or_else(|| self.settings.style());
        // Spend-tracker attribution: stamp the active file path so the
        // Spending panel can answer "how much has this scene cost?".
        // Untitled buffers leave the field `None` and the call still
        // records, just without scene grouping.
        run_cfg.scene_path = self
            .active()
            .path
            .as_ref()
            .map(|p| p.display().to_string());
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

}
