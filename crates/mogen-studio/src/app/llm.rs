//! LLM driver for the studio: generate / modify / animate / repair / textures
//! plus the prompt-enhance side channel. The submodules each carry one slice
//! of the per-call lifecycle so this file stays a thin entry-point + glue:
//!
//! - `credentials` — provider-slot → credential resolution (API key, OAuth
//!   bundle, Z.ai key, image-generation precedence).
//! - `enhance` — prompt rewriter with its own single-slot in-flight tracking.
//! - `poll` — drains worker channels, applies outcomes, splices LLM changes
//!   into the editor undo history.
//! - `spawn` — system-instruction cache, per-run tuning struct, and the
//!   text-LLM thread spawn (`spawn_llm`).
//! - `textures` — texture-pipeline starters (force, per-material, retry).

mod credentials;
mod enhance;
mod meta_generate;
pub(in crate::app) mod modify_screenshot;
mod poll;
mod spawn;
mod textures;

use eframe::egui;

use super::types::LlmKind;
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
        let (prompt, src_empty, existing, want_screenshot, scene_renderable) = {
            let f = self.active();
            let scene_ok = f
                .last_result
                .as_ref()
                .map(|r| r.scene.is_some() && !mogen_core::has_errors(&r.diagnostics))
                .unwrap_or(false);
            (
                f.mod_prompt.trim().to_string(),
                f.source.trim().is_empty(),
                f.source.clone(),
                f.mod_include_screenshot,
                scene_ok,
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
        // Screenshot path: only when the toggle is on, the active
        // provider can read images, AND the scene actually renders.
        // Falling back silently to text-only otherwise so flipping the
        // provider or breaking the file doesn't hide the Modify
        // button.
        let provider_supports_images = self.settings.provider().supports_images();
        if want_screenshot && provider_supports_images && scene_renderable {
            self.submit_modify_screenshot_capture(&ctx, prompt, existing);
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
            // Repair / Textures have no editable prompt field — their
            // `llm_last_prompt` carries a synthetic label that the
            // retry path falls back to via `draft_prompt`.
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
        f.status =
            "llm: cancelled (background call may still finish but result is dropped)".into();
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
