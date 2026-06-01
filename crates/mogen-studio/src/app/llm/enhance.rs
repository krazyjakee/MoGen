use eframe::egui;

use crate::app::types::{EnhanceInFlight, EnhanceTarget};
use crate::app::util::run_prompt_enhance;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Kick off a background prompt-enhancement call for `target`. No-ops when
    /// another enhance is already in flight (single app-level slot), the
    /// source field is empty, or no API key is available (the caller surfaces
    /// a warning next to the button). On failure a per-target error string is
    /// stashed on the app for the button's label row to render.
    pub(in crate::app) fn start_prompt_enhance(
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
            self.enhance_error = Some((target, "enter a prompt first".into()));
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
        let endpoints = self.provider_endpoints();
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
                endpoints,
            );
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// Drain the single in-flight enhance slot. Writes the rewritten prompt
    /// back into the original field on success, or records a per-target error
    /// string on failure. Runs alongside `poll_llm` every frame.
    pub(in crate::app) fn poll_prompt_enhance(&mut self) {
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
    pub(in crate::app) fn any_enhance_in_flight(&self) -> bool {
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
}
