use std::time::Instant;

use eframe::egui;

use crate::app::types::{LlmEvent, LlmEventTone, LlmKind, LlmMessage, LlmProgress};
use crate::app::util::run_llm_textures;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    pub(in crate::app) fn start_llm_textures(&mut self, ctx: egui::Context) {
        self.start_llm_textures_inner(ctx, None);
    }

    /// "New textures" banner button lands here. Forces a full regenerate by
    /// flipping `texture_cfg.force=true` on the active file before kicking
    /// off the same pipeline `start_llm_textures` uses, then clears the
    /// banner the button was rendered on so its message doesn't outlive the
    /// click. The cfg flip is persistent — the panel checkbox follows.
    pub(in crate::app) fn start_llm_textures_force(&mut self, ctx: egui::Context) {
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
    pub(in crate::app) fn start_llm_textures_for_material(
        &mut self,
        ctx: egui::Context,
        material: String,
    ) {
        self.start_llm_textures_inner(ctx, Some(vec![material]));
    }

    pub(in crate::app) fn start_llm_textures_inner(
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
}
