//! Scene Wizard UI window plus the background-worker plumbing that drives
//! each stage. Owns a small `WizardSession` struct on the app so the rest
//! of the studio doesn't have to know about it.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use eframe::egui;
use mogen_llm::image_client::ImageClient;

use crate::app::util::{pick_default_seed, Credential};
use crate::viewer::{CaptureFrame, CaptureKind, CaptureRequest};

use super::wizard::iso_camera;
use super::wizard::pipeline::{
    build_text_client, run_brief, run_manifest, run_object_mog, run_object_review,
    run_reference_image, run_scene_review, WizardRunConfig,
};
use super::wizard::state::{
    next_pending_object, next_pending_reference, Stage, WizardMessage, WizardState,
};
use super::wizard::{persist, write_assembly};
use super::MogenStudioApp;

/// In-flight pump for the wizard. Holds the message channel and small UI
/// drafts that don't belong in `WizardState` (per-row name overrides,
/// regenerate confirms, etc.).
pub(in crate::app) struct WizardSession {
    pub state: WizardState,
    pub rx: Receiver<WizardMessage>,
    pub tx: Sender<WizardMessage>,
    /// Highest-level status line shown above the stage panel.
    pub status: String,
    /// Last reported failure, surfaced as a banner under the stage panel
    /// until the next successful action clears it.
    pub error: Option<String>,
    /// Set while a worker thread is in flight for the current stage so the
    /// UI can grey out the action button and show a spinner. Cleared when
    /// the matching `WizardMessage::*Done` arrives.
    pub running: WizardBusy,
    /// User-edited prompt while on the Prompt stage. Mirrors
    /// `state.prompt` but is the source of truth while the field has focus
    /// (so persisting on every keystroke doesn't fight egui's TextEdit).
    pub prompt_draft: String,
    /// Apply-corrections confirm latch on the ReviewScene stage.
    pub apply_corrections_armed: bool,
    /// Counter of correction iterations done on this run. Capped via
    /// [`WizardSession::MAX_CORRECTION_ITERS`] so a misbehaving model can't
    /// loop the wizard indefinitely.
    pub correction_iterations: u32,
}

/// Which stage's worker is in-flight (used for spinner / disable gating).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum WizardBusy {
    None,
    Brief,
    Manifest,
    /// id of the object currently being processed; the stage is implied by
    /// `state.stage`.
    Object(usize),
    Reference(usize),
    ReviewObject(usize),
    SceneReview,
    Assemble,
    Build,
}

impl WizardSession {
    pub const MAX_CORRECTION_ITERS: u32 = 3;
}

impl MogenStudioApp {
    /// Open the Scene Wizard. Picks (or creates) a `wizard/` folder next to
    /// the active file (or under the project root for an untitled buffer),
    /// reloads any persisted state, and shows the window. Idempotent —
    /// calling while open is a no-op.
    pub(super) fn open_scene_wizard(&mut self) {
        if self.wizard.is_some() {
            self.show_wizard = true;
            return;
        }
        let project_dir = self.resolve_wizard_project_dir();
        let state = persist::load(&project_dir).unwrap_or_else(|| WizardState {
            project_dir: project_dir.clone(),
            seed: pick_default_seed(),
            ..Default::default()
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let prompt_draft = state.prompt.clone();
        let status = if state.manifest.is_empty() {
            "Describe the scene you want to generate.".to_string()
        } else {
            format!(
                "Resumed wizard at stage `{}` ({} objects in manifest).",
                state.stage.label(),
                state.manifest.len()
            )
        };
        self.wizard = Some(WizardSession {
            state,
            rx,
            tx,
            status,
            error: None,
            running: WizardBusy::None,
            prompt_draft,
            apply_corrections_armed: false,
            correction_iterations: 0,
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
    pub(super) fn poll_wizard(&mut self, ctx: &egui::Context) {
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

        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        while let Ok(msg) = session.rx.try_recv() {
            apply_wizard_message(session, msg);
        }
        // Keep repainting while any worker is in flight so spinners tick and
        // results land promptly even without user input.
        if session.running != WizardBusy::None {
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
    pub(super) fn ui_wizard(&mut self, ctx: &egui::Context) {
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
            WizardAction::SavePrompt => {
                if let Some(s) = self.wizard.as_mut() {
                    s.state.prompt = s.prompt_draft.clone();
                    let _ = persist::save(&s.state);
                }
            }
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
            WizardAction::GenerateNextReference => self.start_wizard_next_reference(ctx),
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
            WizardAction::GenerateNextObject => self.start_wizard_next_object(ctx),
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
            let _ = persist::save(&s.state);
        }
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

    fn build_wizard_run_config(&self) -> WizardRunConfig {
        WizardRunConfig {
            model: self.settings.provider_model(),
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

    fn resolve_wizard_text_client(&mut self) -> Option<(mogen_llm::LlmClient, Arc<String>)> {
        let slot = self.settings.provider_slot();
        let provider = slot.to_provider();
        let cred = self.resolve_credential()?;
        let claude_path = self.settings.claude_code_path();
        let zai_url = self.settings.zai_base_url().to_string();
        let client = build_text_client(provider, cred, &claude_path, &zai_url);
        let sys = self.cached_system_instruction();
        Some((client, sys))
    }

    fn resolve_wizard_image_client(&self) -> Option<ImageClient> {
        let cred = self.resolve_gemini_credential()?;
        match cred {
            Credential::Zai(key) => Some(ImageClient::Zai(mogen_llm::zai::ZaiClient::new(key))),
            Credential::AntigravityOAuth(bundle) => Some(ImageClient::Gemini(
                mogen_llm::GeminiClient::from_antigravity_oauth(bundle),
            )),
            Credential::GeminiOAuth(bundle) => Some(ImageClient::Gemini(
                mogen_llm::GeminiClient::from_oauth(bundle),
            )),
            Credential::ApiKey(key) => Some(ImageClient::Gemini(
                mogen_llm::GeminiClient::new(key),
            )),
        }
    }

    fn start_wizard_brief(&mut self, ctx: &egui::Context) {
        let Some((client, sys)) = self.resolve_wizard_text_client() else {
            self.wizard_set_error("Provider credentials missing — open Preferences and sign in.".into());
            return;
        };
        let cfg = self.build_wizard_run_config();
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        session.state.prompt = session.prompt_draft.clone();
        if session.state.prompt.trim().is_empty() {
            session.error = Some("Enter a scene prompt first.".into());
            return;
        }
        let _ = persist::save(&session.state);
        session.running = WizardBusy::Brief;
        session.error = None;
        session.status = "Generating scene brief…".into();
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        let prompt = session.state.prompt.clone();
        std::thread::spawn(move || {
            let result = run_brief(client, sys, prompt, cfg);
            let _ = tx.send(WizardMessage::BriefDone(result));
            ctx_clone.request_repaint();
        });
    }

    fn start_wizard_manifest(&mut self, ctx: &egui::Context) {
        let Some((client, sys)) = self.resolve_wizard_text_client() else {
            self.wizard_set_error("Provider credentials missing — open Preferences and sign in.".into());
            return;
        };
        let cfg = self.build_wizard_run_config();
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        let prompt = session.state.prompt.clone();
        let Some(brief) = session.state.brief.clone() else {
            session.error = Some("Generate a brief first.".into());
            return;
        };
        session.running = WizardBusy::Manifest;
        session.error = None;
        session.status = "Generating object manifest…".into();
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let result = run_manifest(client, sys, prompt, brief, cfg);
            let _ = tx.send(WizardMessage::ManifestDone(result));
            ctx_clone.request_repaint();
        });
    }

    fn start_wizard_next_reference(&mut self, ctx: &egui::Context) {
        let Some(client) = self.resolve_wizard_image_client() else {
            self.wizard_set_error(
                "Image generation needs a Gemini or Z.ai credential — check Preferences › Image.".into(),
            );
            return;
        };
        let cfg = self.build_wizard_run_config();
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        let Some(obj) = next_pending_reference(&session.state).cloned() else {
            session.status = "All reference images generated.".into();
            return;
        };
        let out = session.state.references_dir().join(format!("{}.png", obj.id));
        let idx = session
            .state
            .manifest
            .iter()
            .position(|o| o.id == obj.id)
            .unwrap_or(0);
        session.running = WizardBusy::Reference(idx);
        session.status = format!("Generating reference image for {}…", obj.name);
        session.error = None;
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        let id = obj.id.clone();
        std::thread::spawn(move || {
            let result = run_reference_image(client, obj, out, cfg.seed);
            let _ = tx.send(WizardMessage::ReferenceDone { id, result });
            ctx_clone.request_repaint();
        });
    }

    fn start_wizard_next_object(&mut self, ctx: &egui::Context) {
        let Some((client, sys)) = self.resolve_wizard_text_client() else {
            self.wizard_set_error("Provider credentials missing — open Preferences and sign in.".into());
            return;
        };
        let cfg = self.build_wizard_run_config();
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        let Some(obj) = next_pending_object(&session.state).cloned() else {
            session.status = "All object modules generated.".into();
            return;
        };
        let out = session.state.objects_dir().join(format!("{}.mog", obj.id));
        let idx = session
            .state
            .manifest
            .iter()
            .position(|o| o.id == obj.id)
            .unwrap_or(0);
        let reference = obj.reference_image.clone();
        let (ref_bytes, ref_mime) = if let Some(p) = reference {
            std::fs::read(&p)
                .ok()
                .map(|b| (Some(b), Some("image/png".to_string())))
                .unwrap_or((None, None))
        } else {
            (None, None)
        };
        session.running = WizardBusy::Object(idx);
        session.status = format!("Generating module for {}…", obj.name);
        session.error = None;
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        let id = obj.id.clone();
        std::thread::spawn(move || {
            let result = run_object_mog(client, sys, obj, out, ref_bytes, ref_mime, cfg);
            let _ = tx.send(WizardMessage::ObjectDone { id, result });
            ctx_clone.request_repaint();
        });
    }

    fn start_wizard_regenerate_object(&mut self, ctx: &egui::Context, id: String) {
        // Drop the existing .mog so `next_pending_object` picks it back up,
        // then defer to the normal next-object path.
        if let Some(s) = self.wizard.as_mut() {
            if let Some(obj) = s.state.find_object_mut(&id) {
                if let Some(p) = obj.mog_path.as_ref() {
                    let _ = std::fs::remove_file(p);
                }
                obj.mog_path = None;
                obj.position_guide = None;
                obj.thumb_path = None;
            }
            s.state.per_object_reviews.remove(&id);
            let _ = persist::save(&s.state);
            s.status = format!("Regenerating {id}…");
        }
        self.start_wizard_next_object(ctx);
    }

    fn start_wizard_assemble_and_build(&mut self, ctx: &egui::Context) {
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        session.running = WizardBusy::Assemble;
        session.status = "Assembling scene…".into();
        session.error = None;
        let state_clone = session.state.clone();
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let result = write_assembly(&state_clone);
            let _ = tx.send(WizardMessage::AssemblyDone(result.clone()));
            // On success, build immediately.
            if let Ok(asm_path) = result {
                let build_result = build_assembly(&asm_path);
                let _ = tx.send(WizardMessage::BuildDone(build_result));
            }
            ctx_clone.request_repaint();
        });
    }

    fn start_wizard_object_review(&mut self, ctx: &egui::Context, id: String) {
        let Some((client, sys)) = self.resolve_wizard_text_client() else {
            self.wizard_set_error("Provider credentials missing — open Preferences and sign in.".into());
            return;
        };
        let provider = self.settings.provider_slot().to_provider();
        if !provider.supports_images() {
            self.wizard_set_error(format!(
                "Provider {} can't read images — pick a vision-capable provider in Preferences.",
                provider.label()
            ));
            return;
        }
        let cfg = self.build_wizard_run_config();
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        let Some(obj) = session.state.find_object(&id).cloned() else {
            session.error = Some(format!("Object {id} no longer in manifest"));
            return;
        };
        // Use the per-object reference image as the LLM target. The proper
        // shape would be a per-object rendered thumbnail; we lean on the
        // reference image as a stand-in so the review stage works even
        // before the user has rebuilt thumbnails. (A rendered thumbnail
        // can replace this once thumb_path is wired up.)
        let img_path = obj.thumb_path.clone().or_else(|| obj.reference_image.clone());
        let Some(img_path) = img_path else {
            session.error = Some(format!("No image available for {id} — generate a reference first."));
            return;
        };
        let Ok(image_bytes) = std::fs::read(&img_path) else {
            session.error = Some(format!("Couldn't read image at {}", img_path.display()));
            return;
        };
        let mime = guess_image_mime(&img_path);
        let idx = session
            .state
            .manifest
            .iter()
            .position(|o| o.id == id)
            .unwrap_or(0);
        session.running = WizardBusy::ReviewObject(idx);
        session.status = format!("Reviewing {}…", obj.name);
        session.error = None;
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let result = run_object_review(client, sys, obj, image_bytes, mime, cfg);
            let _ = tx.send(WizardMessage::ObjectReviewDone { id, result });
            ctx_clone.request_repaint();
        });
    }

    fn start_wizard_scene_review(&mut self, ctx: &egui::Context) {
        let Some((client, sys)) = self.resolve_wizard_text_client() else {
            self.wizard_set_error("Provider credentials missing — open Preferences and sign in.".into());
            return;
        };
        let provider = self.settings.provider_slot().to_provider();
        if !provider.supports_images() {
            self.wizard_set_error(format!(
                "Provider {} can't read images — pick a vision-capable provider in Preferences.",
                provider.label()
            ));
            return;
        }
        let cfg = self.build_wizard_run_config();
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        let Some(thumb) = session.state.scene_thumb.clone() else {
            session.error = Some(
                "No scene screenshot yet — assemble + render the scene first.".into(),
            );
            return;
        };
        let Ok(bytes) = std::fs::read(&thumb) else {
            session.error = Some(format!("Couldn't read scene screenshot at {}", thumb.display()));
            return;
        };
        let mime = guess_image_mime(&thumb);
        let prompt = session.state.prompt.clone();
        let manifest = session.state.manifest.clone();
        let iter = session.correction_iterations + 1;
        session.running = WizardBusy::SceneReview;
        session.status = format!("Reviewing full scene (iter {})…", iter);
        session.error = None;
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let result = run_scene_review(client, sys, prompt, manifest, bytes, mime, iter, cfg);
            let _ = tx.send(WizardMessage::SceneReviewDone(result));
            ctx_clone.request_repaint();
        });
    }

    fn apply_wizard_corrections(&mut self, ctx: &egui::Context) {
        // Scope the &mut borrow on self.wizard so we can call
        // `start_wizard_assemble_and_build` after the corrections land.
        let should_rebuild = {
            let Some(session) = self.wizard.as_mut() else {
                return;
            };
            let Some(review) = session.state.scene_review.clone() else {
                return;
            };
            let Some(asm_path) = session.state.assembly_path.clone() else {
                session.error =
                    Some("No assembly file to edit — build the scene first.".into());
                return;
            };
            let manifest = session.state.manifest.clone();
            let src = match std::fs::read_to_string(&asm_path) {
                Ok(s) => s,
                Err(e) => {
                    session.error = Some(format!("Couldn't read assembly: {e}"));
                    return;
                }
            };
            let (new_src, applied) = super::wizard::corrections::apply_corrections(
                &src,
                &manifest,
                &review.corrections,
            );
            if applied == 0 {
                session.status = "No corrections matched any manifest objects.".into();
                return;
            }
            if let Err(e) = std::fs::write(&asm_path, new_src.as_bytes()) {
                session.error = Some(format!("Couldn't write corrected assembly: {e}"));
                return;
            }
            // Mirror the corrections onto the manifest so subsequent rebuilds
            // emit the new positions from the assembler, not just from the
            // in-place patch (keeps the manifest authoritative).
            for c in &review.corrections {
                if let Some(obj) = session.state.find_object_mut(&c.object_id) {
                    if let Some(p) = c.new_position {
                        obj.position = p;
                    }
                    if let Some(r) = c.new_rotation_y_deg {
                        obj.rotation_y_deg = r;
                    }
                }
            }
            session.correction_iterations += 1;
            session.status = format!(
                "Applied {applied} correction(s) (iter {}/{}).",
                session.correction_iterations,
                WizardSession::MAX_CORRECTION_ITERS
            );
            session.apply_corrections_armed = false;
            session.state.scene_review = None;
            let _ = persist::save(&session.state);
            true
        };
        // Rebuild so the next screenshot reflects the corrections.
        if should_rebuild {
            self.start_wizard_assemble_and_build(ctx);
        }
    }

    fn wizard_set_error(&mut self, msg: String) {
        if let Some(s) = self.wizard.as_mut() {
            s.error = Some(msg);
            s.running = WizardBusy::None;
        }
    }
}

/// Spawn a small build of the assembly file. Reuses the canonical
/// `pipeline::build_to_glb` so the wizard's output is bit-identical to
/// "File → Build GLB" on the same source.
fn build_assembly(asm_path: &std::path::Path) -> Result<PathBuf, String> {
    let src = std::fs::read_to_string(asm_path)
        .map_err(|e| format!("read {}: {e}", asm_path.display()))?;
    let result = crate::pipeline::compile(&src, asm_path.parent());
    if result.stage != crate::pipeline::Stage::Ok {
        return Err(format!(
            "build failed at stage {:?}: {}",
            result.stage,
            result
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    let Some(scene) = result.scene else {
        return Err("build produced no scene".into());
    };
    let out = asm_path.with_extension("glb");
    let scene_clone = (*scene).clone();
    let opts = mogen_export::ExportOptions::default();
    crate::pipeline::write_glb_with_source_and_options(
        &scene_clone,
        &out,
        asm_path.parent(),
        &opts,
        |_| {},
    )
    .map_err(|e| format!("{e}"))?;
    Ok(out)
}

fn guess_image_mime(path: &std::path::Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
    .to_string()
}

/// One queued button-click intent. Collected while drawing so the stage-draw
/// helpers can borrow `&mut WizardSession` without fighting the borrow
/// checker over `self.start_wizard_*` reborrows.
#[derive(Debug, Clone)]
pub(super) enum WizardAction {
    None,
    SavePrompt,
    GenerateBrief,
    RegenerateBrief,
    AdvanceToManifest,
    BackToPrompt,
    GenerateManifest,
    RegenerateManifest,
    AdvanceToReferences,
    BackToBrief,
    GenerateNextReference,
    AdvanceToObjects,
    SkipReferences,
    BackToManifest,
    GenerateNextObject,
    AdvanceToAssemble,
    BackToReferences,
    RunAssembleAndBuild,
    AdvanceToReviewObjects,
    BackToObjects,
    ReviewObject(String),
    RegenerateObject(String),
    AdvanceToReviewScene,
    BackToAssemble,
    BackToReviewObjects,
    RunSceneReview,
    ApplyCorrections,
    FinishWizard,
    OpenAssemblyInTab,
    /// Open the assembly in a tab and submit an isometric thumbnail capture
    /// — the screenshot is what the scene-review LLM call reads.
    CaptureScene,
    RemoveObject(String),
    Reset,
    Close,
}

mod stages;
use stages::{
    apply_wizard_message, draw_assemble_stage, draw_brief_stage, draw_done_stage,
    draw_manifest_stage, draw_objects_stage, draw_prompt_stage, draw_references_stage,
    draw_review_objects_stage, draw_review_scene_stage, draw_stage_strip,
};
