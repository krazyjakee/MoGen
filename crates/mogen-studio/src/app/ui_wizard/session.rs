//! `MogenStudioApp` methods that own the wizard window lifecycle: opening it,
//! polling worker messages each frame, drawing the current stage, and turning
//! the [`WizardAction`] a stage emits into concrete state changes or worker
//! launches. The background worker launches themselves live in
//! [`super::workers`].

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use mogen_llm::image_client::ImageClient;

use crate::app::util::{pick_default_seed, Credential};
use crate::app::wizard::iso_camera;
use crate::app::wizard::persist;
use crate::app::wizard::pipeline::{build_text_client, WizardRunConfig};
use crate::app::wizard::state::{Stage, WizardState};
use crate::app::MogenStudioApp;
use crate::viewer::{CaptureFrame, CaptureKind, CaptureRequest};

use super::draw::{
    draw_assemble_stage, draw_brief_stage, draw_done_stage, draw_location_stage,
    draw_manifest_stage, draw_objects_stage, draw_prompt_stage, draw_references_stage,
    draw_review_objects_stage, draw_review_scene_stage, draw_stage_strip, WizardAction,
};
use super::message::apply_wizard_message;
use super::{
    next_pending_object_skipping, next_pending_reference_skipping, WizardBusy, WizardSession,
};

impl MogenStudioApp {
    /// Open the Scene Wizard. Picks (or creates) a `wizard/` folder next to
    /// the active file (or under the project root for an untitled buffer),
    /// reloads any persisted state, and shows the window. Idempotent —
    /// calling while open is a no-op.
    pub(in crate::app) fn open_scene_wizard(&mut self) {
        if self.wizard.is_some() {
            self.show_wizard = true;
            return;
        }
        let suggested = self.resolve_wizard_project_dir();
        // Always start at the Location stage so the user explicitly confirms
        // where wizard artefacts will be written. State is loaded (or
        // freshly seeded) only after the location is committed.
        let state = WizardState {
            project_dir: PathBuf::new(),
            seed: pick_default_seed(),
            stage: Stage::Location,
            ..Default::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let prompt_draft = state.prompt.clone();
        let status =
            "Choose where wizard artefacts (objects, references, state.json) will be saved."
                .to_string();
        self.wizard = Some(WizardSession {
            state,
            rx,
            tx,
            status,
            error: None,
            running: WizardBusy::None,
            prompt_draft,
            location_draft: suggested,
            correction_iterations: 0,
            running_ref_ids: HashSet::new(),
            running_object_ids: HashSet::new(),
            auto_continue_refs: false,
            auto_continue_objects: false,
        });
        self.show_wizard = true;
    }

    /// Pick the wizard's project folder. For titled tabs it's
    /// `<file_parent>/wizard/`; for untitled buffers it falls back to
    /// `<project_root>/wizard/`. Created on the first save, not here.
    fn resolve_wizard_project_dir(&self) -> PathBuf {
        let base = self
            .active()
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| self.project_root.clone());
        base.join("wizard")
    }

    /// Drain wizard worker messages and apply them to the session state. One
    /// call per frame from `update`.
    pub(in crate::app) fn poll_wizard(&mut self, ctx: &egui::Context) {
        // Pick up any pending isometric thumbnail capture for the wizard.
        // Routed here (rather than in `poll_generate`) so the active tab's
        // status line is untouched while a wizard-owned screenshot lands.
        if let Some(outcome) = self
            .viewer
            .take_capture_outcome_if(|kind| matches!(kind, CaptureKind::WizardThumb))
        {
            if let Some(session) = self.wizard.as_mut() {
                if let Some(err) = outcome.error {
                    session.error = Some(format!("isometric screenshot failed: {err}"));
                    session.running = WizardBusy::None;
                } else if let Some(path) = outcome.frame_paths.last().cloned() {
                    session.state.scene_thumb = Some(path.clone());
                    session.status = format!("Captured isometric screenshot at {}", path.display());
                    let _ = persist::save(&session.state);
                    session.running = WizardBusy::None;
                }
            }
        }

        {
            let Some(session) = self.wizard.as_mut() else {
                return;
            };
            while let Ok(msg) = session.rx.try_recv() {
                apply_wizard_message(session, msg);
            }
        }
        // If auto-continue is on and we have spare capacity, top the pool back
        // up. Done outside the &mut borrow above so `start_wizard_*` can
        // re-borrow self.wizard. Bounded by the same BATCH_SIZE the manual
        // batch button uses.
        let (need_refs, need_objects) = self
            .wizard
            .as_ref()
            .map(|s| {
                let refs = s.state.stage == Stage::References
                    && s.auto_continue_refs
                    && s.running_ref_ids.len() < WizardSession::BATCH_SIZE
                    && next_pending_reference_skipping(&s.state, &s.running_ref_ids).is_some();
                let objs = s.state.stage == Stage::Objects
                    && s.auto_continue_objects
                    && s.running_object_ids.len() < WizardSession::BATCH_SIZE
                    && next_pending_object_skipping(&s.state, &s.running_object_ids).is_some();
                (refs, objs)
            })
            .unwrap_or((false, false));
        if need_refs {
            self.start_wizard_reference_batch(ctx, WizardSession::BATCH_SIZE, true);
        }
        if need_objects {
            self.start_wizard_object_batch(ctx, WizardSession::BATCH_SIZE, true);
        }
        // Keep repainting while any worker is in flight so spinners tick and
        // results land promptly even without user input.
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        if session.running != WizardBusy::None
            || !session.running_ref_ids.is_empty()
            || !session.running_object_ids.is_empty()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }

    /// Submit an isometric thumbnail capture of the assembled scene. The
    /// caller is responsible for having the assembly tab active in the
    /// viewer — without that the capture will render whatever scene happens
    /// to be loaded (which is almost certainly not what the wizard wants).
    fn submit_wizard_scene_capture(&mut self, ctx: &egui::Context) {
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        let project_dir = session.state.project_dir.clone();
        let out = project_dir.join("scene_thumb.png");
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (yaw, pitch) = iso_camera();
        let bg = self.settings.viewer_bg_rgb();
        // Marking this as in-flight so the UI grey-outs the button.
        session.running = WizardBusy::SceneReview;
        session.status = "Capturing isometric scene screenshot…".into();
        self.viewer.submit_capture(CaptureRequest {
            kind: CaptureKind::WizardThumb,
            size: 768,
            bg,
            frames: vec![CaptureFrame {
                yaw,
                pitch,
                time: 0.0,
                path: out,
            }],
            total: 0,
            written: Vec::new(),
            error: None,
        });
        ctx.request_repaint();
    }

    /// Draw the wizard window. No-op when closed.
    pub(in crate::app) fn ui_wizard(&mut self, ctx: &egui::Context) {
        if !self.show_wizard {
            return;
        }
        if self.wizard.is_none() {
            return;
        }
        let mut open = true;
        let mut action = WizardAction::None;
        egui::Window::new("Scene Wizard")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(720.0)
            .default_height(560.0)
            .show(ctx, |ui| {
                let Some(session) = self.wizard.as_mut() else {
                    return;
                };
                draw_stage_strip(ui, session);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&session.status));
                });
                if let Some(err) = session.error.clone() {
                    ui.colored_label(egui::Color32::from_rgb(230, 120, 120), err);
                }
                ui.add_space(4.0);
                action = match session.state.stage {
                    Stage::Location => draw_location_stage(ui, session),
                    Stage::Prompt => draw_prompt_stage(ui, session),
                    Stage::Brief => draw_brief_stage(ui, session),
                    Stage::Manifest => draw_manifest_stage(ui, session),
                    Stage::References => draw_references_stage(ui, session),
                    Stage::Objects => draw_objects_stage(ui, session),
                    Stage::Assemble => draw_assemble_stage(ui, session),
                    Stage::ReviewObjects => draw_review_objects_stage(ui, session),
                    Stage::ReviewScene => draw_review_scene_stage(ui, session),
                    Stage::Done => draw_done_stage(ui, session),
                };
            });
        if !open {
            self.show_wizard = false;
        }
        self.dispatch_wizard_action(ctx, action);
    }

    fn dispatch_wizard_action(&mut self, ctx: &egui::Context, action: WizardAction) {
        match action {
            WizardAction::None => {}
            WizardAction::PickLocation => self.pick_wizard_location(),
            WizardAction::ConfirmLocation => self.confirm_wizard_location(),
            WizardAction::SavePrompt => {
                if let Some(s) = self.wizard.as_mut() {
                    s.state.prompt = s.prompt_draft.clone();
                    let _ = persist::save(&s.state);
                }
            }
            WizardAction::PickSourceImage => self.pick_wizard_source_image(),
            WizardAction::ClearSourceImage => self.clear_wizard_source_image(),
            WizardAction::GenerateBrief => self.start_wizard_brief(ctx),
            WizardAction::AdvanceToManifest => {
                if let Some(s) = self.wizard.as_mut() {
                    s.state.stage = Stage::Manifest;
                    s.error = None;
                    let _ = persist::save(&s.state);
                }
            }
            WizardAction::BackToPrompt => self.wizard_back_to(Stage::Prompt),
            WizardAction::GenerateManifest => self.start_wizard_manifest(ctx),
            WizardAction::AdvanceToReferences => {
                if let Some(s) = self.wizard.as_mut() {
                    s.state.stage = Stage::References;
                    s.error = None;
                    let _ = persist::save(&s.state);
                }
            }
            WizardAction::BackToBrief => self.wizard_back_to(Stage::Brief),
            WizardAction::GenerateNextReference => {
                self.start_wizard_reference_batch(ctx, 1, false);
            }
            WizardAction::GenerateOneReference(id) => {
                self.start_wizard_one_reference(ctx, id);
            }
            WizardAction::AutoGenerateReferences => {
                self.start_wizard_reference_batch(ctx, WizardSession::BATCH_SIZE, true);
            }
            WizardAction::StopAutoReferences => {
                if let Some(s) = self.wizard.as_mut() {
                    s.auto_continue_refs = false;
                    s.status = "Stopped auto-generate; in-flight workers will finish.".into();
                }
            }
            WizardAction::AdvanceToObjects => {
                if let Some(s) = self.wizard.as_mut() {
                    s.state.stage = Stage::Objects;
                    s.error = None;
                    let _ = persist::save(&s.state);
                }
            }
            WizardAction::SkipReferences => {
                if let Some(s) = self.wizard.as_mut() {
                    s.state.stage = Stage::Objects;
                    s.status = "Skipping reference images.".into();
                    s.error = None;
                    let _ = persist::save(&s.state);
                }
            }
            WizardAction::BackToManifest => self.wizard_back_to(Stage::Manifest),
            WizardAction::GenerateNextObject => {
                self.start_wizard_object_batch(ctx, 1, false);
            }
            WizardAction::GenerateOneObject(id) => {
                self.start_wizard_regenerate_object(ctx, id);
            }
            WizardAction::AutoGenerateObjects => {
                self.start_wizard_object_batch(ctx, WizardSession::BATCH_SIZE, true);
            }
            WizardAction::StopAutoObjects => {
                if let Some(s) = self.wizard.as_mut() {
                    s.auto_continue_objects = false;
                    s.status = "Stopped auto-generate; in-flight workers will finish.".into();
                }
            }
            WizardAction::AdvanceToAssemble => {
                if let Some(s) = self.wizard.as_mut() {
                    s.state.stage = Stage::Assemble;
                    s.error = None;
                    let _ = persist::save(&s.state);
                }
            }
            WizardAction::BackToReferences => self.wizard_back_to(Stage::References),
            WizardAction::RunAssembleAndBuild => self.start_wizard_assemble_and_build(ctx),
            WizardAction::AdvanceToReviewObjects => {
                if let Some(s) = self.wizard.as_mut() {
                    s.state.stage = Stage::ReviewObjects;
                    s.error = None;
                    let _ = persist::save(&s.state);
                }
            }
            WizardAction::BackToObjects => self.wizard_back_to(Stage::Objects),
            WizardAction::ReviewObject(id) => self.start_wizard_object_review(ctx, id),
            WizardAction::RegenerateObject(id) => self.start_wizard_regenerate_object(ctx, id),
            WizardAction::AdvanceToReviewScene => {
                if let Some(s) = self.wizard.as_mut() {
                    s.state.stage = Stage::ReviewScene;
                    s.error = None;
                    let _ = persist::save(&s.state);
                }
            }
            WizardAction::BackToAssemble => self.wizard_back_to(Stage::Assemble),
            WizardAction::BackToReviewObjects => self.wizard_back_to(Stage::ReviewObjects),
            WizardAction::RunSceneReview => self.start_wizard_scene_review(ctx),
            WizardAction::ApplyCorrections => self.apply_wizard_corrections(ctx),
            WizardAction::FinishWizard => {
                if let Some(s) = self.wizard.as_mut() {
                    s.state.stage = Stage::Done;
                    s.error = None;
                    let _ = persist::save(&s.state);
                }
            }
            WizardAction::OpenAssemblyInTab => self.open_wizard_assembly_in_tab(),
            WizardAction::CaptureScene => self.capture_wizard_scene(ctx),
            WizardAction::RegenerateBrief => self.start_wizard_brief(ctx),
            WizardAction::RegenerateManifest => self.start_wizard_manifest(ctx),
            WizardAction::RemoveObject(id) => self.remove_wizard_object(id),
            WizardAction::Close => {
                self.show_wizard = false;
            }
            WizardAction::Reset => self.reset_wizard(),
        }
    }

    fn wizard_back_to(&mut self, stage: Stage) {
        if let Some(s) = self.wizard.as_mut() {
            s.state.stage = stage;
            s.error = None;
            let _ = persist::save(&s.state);
        }
    }

    fn reset_wizard(&mut self) {
        if let Some(s) = self.wizard.as_mut() {
            let project_dir = s.state.project_dir.clone();
            let seed = pick_default_seed();
            s.state = WizardState {
                project_dir,
                seed,
                ..Default::default()
            };
            s.prompt_draft.clear();
            s.status = "Wizard reset.".into();
            s.error = None;
            s.running = WizardBusy::None;
            s.running_ref_ids.clear();
            s.running_object_ids.clear();
            s.auto_continue_refs = false;
            s.auto_continue_objects = false;
            let _ = persist::save(&s.state);
        }
    }

    fn pick_wizard_location(&mut self) {
        let start = self
            .wizard
            .as_ref()
            .map(|s| s.location_draft.clone())
            .unwrap_or_else(|| self.project_root.clone());
        // Pick the folder above the prospective wizard dir so the user lands
        // somewhere sensible regardless of whether `wizard/` exists yet.
        let dialog_start = start
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.project_root.clone());
        let picked = rfd::FileDialog::new()
            .set_title("Choose wizard save folder")
            .set_directory(&dialog_start)
            .pick_folder();
        if let (Some(picked), Some(s)) = (picked, self.wizard.as_mut()) {
            s.location_draft = picked;
            s.error = None;
        }
    }

    /// Pick a source image and copy it into the wizard project dir as
    /// `source.<ext>`, then point `state.source_image` at the copy. Copying
    /// keeps the run self-contained: the file survives even if the user moves
    /// or deletes the original, and a Studio restart resumes image-driven.
    fn pick_wizard_source_image(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Choose a source image")
            .add_filter("Image", &["png", "jpg", "jpeg", "webp"])
            .set_directory(&self.project_root)
            .pick_file();
        let Some(picked) = picked else {
            return;
        };
        let Some(s) = self.wizard.as_mut() else {
            return;
        };
        let ext = picked
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp"))
            .unwrap_or_else(|| "png".to_string());
        // Drop any prior copy so a re-pick with a different extension doesn't
        // leave a stale source.* behind.
        if let Some(old) = s.state.source_image.take() {
            let _ = std::fs::remove_file(old);
        }
        let dest = s.state.project_dir.join(format!("source.{ext}"));
        match std::fs::copy(&picked, &dest) {
            Ok(_) => {
                s.state.source_image = Some(dest);
                s.error = None;
                s.status = "Source image set — the scene will be generated from it.".into();
                let _ = persist::save(&s.state);
            }
            Err(e) => {
                s.error = Some(format!("Couldn't copy source image: {e}"));
            }
        }
    }

    fn clear_wizard_source_image(&mut self) {
        if let Some(s) = self.wizard.as_mut() {
            if let Some(old) = s.state.source_image.take() {
                let _ = std::fs::remove_file(old);
            }
            s.status = "Source image cleared — back to a text-only run.".into();
            let _ = persist::save(&s.state);
        }
    }

    fn confirm_wizard_location(&mut self) {
        let Some(s) = self.wizard.as_mut() else {
            return;
        };
        let picked = s.location_draft.clone();
        if picked.as_os_str().is_empty() {
            s.error = Some("Pick a folder before continuing.".into());
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&picked) {
            s.error = Some(format!("Could not create {}: {e}", picked.display()));
            return;
        }
        // Silent resume: if a wizard run already lives here, adopt its full
        // state; otherwise start a fresh run at the Prompt stage.
        if let Some(loaded) = persist::load(&picked) {
            s.prompt_draft = loaded.prompt.clone();
            s.status = format!(
                "Resumed wizard at stage `{}` ({} objects in manifest).",
                loaded.stage.label(),
                loaded.manifest.len()
            );
            s.state = loaded;
            // Force the resumed state's project_dir to match what the user
            // actually picked (covers moved folders).
            s.state.project_dir = picked;
        } else {
            s.state.project_dir = picked;
            s.state.stage = Stage::Prompt;
            s.status = "Describe the scene you want to generate.".into();
            let _ = persist::save(&s.state);
        }
        s.error = None;
    }

    fn open_wizard_assembly_in_tab(&mut self) {
        let path = self
            .wizard
            .as_ref()
            .and_then(|s| s.state.assembly_path.clone());
        if let Some(p) = path {
            if p.is_file() {
                self.show_wizard = false;
                self.open_path(&p);
            }
        }
    }

    /// Open the assembly tab (so the viewer's scene matches the wizard
    /// scene) and queue an isometric thumbnail capture. The capture lands
    /// at `<wizard>/scene_thumb.png` and the wizard's poll routes it back
    /// onto `state.scene_thumb`.
    fn capture_wizard_scene(&mut self, ctx: &egui::Context) {
        let path = self
            .wizard
            .as_ref()
            .and_then(|s| s.state.assembly_path.clone());
        let Some(p) = path else {
            self.wizard_set_error("No assembly file yet — build the scene first.".into());
            return;
        };
        if !p.is_file() {
            self.wizard_set_error(format!("Assembly file missing on disk: {}", p.display()));
            return;
        }
        // Make sure the viewer is showing the assembled scene. `open_path`
        // is idempotent — if the tab is already open it just re-activates.
        self.open_path(&p);
        // The window stays up; capture posts back via `poll_wizard`.
        self.show_wizard = true;
        self.submit_wizard_scene_capture(ctx);
    }

    fn remove_wizard_object(&mut self, id: String) {
        if let Some(s) = self.wizard.as_mut() {
            s.state.manifest.retain(|o| o.id != id);
            s.state.per_object_reviews.remove(&id);
            let _ = persist::save(&s.state);
            s.status = format!("Removed object \"{id}\" from manifest.");
        }
    }

    pub(in crate::app) fn build_wizard_run_config(&self) -> WizardRunConfig {
        WizardRunConfig {
            model: self.settings.provider_model(),
            provider: self.settings.provider(),
            thinking: self.settings.thinking_level(),
            temperature: self.settings.temperature(),
            max_repair_iters: self.settings.max_repair_iters(),
            seed: self
                .wizard
                .as_ref()
                .map(|s| s.state.seed)
                .unwrap_or_else(pick_default_seed),
            style: self.settings.style(),
            session_id: self.spend_session_id.clone(),
        }
    }

    pub(in crate::app) fn resolve_wizard_text_client(
        &mut self,
    ) -> Option<(mogen_llm::LlmClient, Arc<String>)> {
        let slot = self.settings.provider_slot();
        let provider = slot.to_provider();
        let cred = self.resolve_credential()?;
        let endpoints = self.provider_endpoints();
        let client = build_text_client(provider, cred, &endpoints);
        let sys = self.cached_system_instruction();
        Some((client, sys))
    }

    pub(in crate::app) fn resolve_wizard_image_client(&self) -> Option<ImageClient> {
        let cred = self.resolve_gemini_credential()?;
        match cred {
            Credential::Zai(key) => Some(ImageClient::Zai(mogen_llm::zai::ZaiClient::new(key))),
            Credential::AntigravityOAuth(bundle) => Some(ImageClient::Gemini(
                mogen_llm::GeminiClient::from_antigravity_oauth(bundle),
            )),
            Credential::GeminiOAuth(bundle) => Some(ImageClient::Gemini(
                mogen_llm::GeminiClient::from_oauth(bundle),
            )),
            Credential::ApiKey(key) => Some(ImageClient::Gemini(mogen_llm::GeminiClient::new(key))),
        }
    }

    pub(in crate::app) fn wizard_set_error(&mut self, msg: String) {
        if let Some(s) = self.wizard.as_mut() {
            s.error = Some(msg);
            s.running = WizardBusy::None;
        }
    }
}
