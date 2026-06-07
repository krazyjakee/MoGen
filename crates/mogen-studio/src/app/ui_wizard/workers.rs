//! `MogenStudioApp` methods that spawn the wizard's background worker threads,
//! one per pipeline stage (brief, manifest, reference images, per-object
//! modules, assemble + build, per-object review, scene review). Each worker
//! posts its result back through the session channel as a [`WizardMessage`]
//! that [`super::message::apply_wizard_message`] folds into the live session.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use mogen_llm::image_client::ImageClient;

use crate::app::wizard::pipeline::{
    run_brief, run_manifest, run_object_mog, run_object_review, run_reference_image,
    run_scene_review, WizardRunConfig,
};
use crate::app::wizard::state::{ObjectEntry, WizardMessage, WizardState};
use crate::app::wizard::{persist, write_assembly};
use crate::app::MogenStudioApp;

use super::{claim_pending_batch, object_missing, reference_missing, WizardBusy, WizardSession};

impl MogenStudioApp {
    pub(in crate::app) fn start_wizard_brief(&mut self, ctx: &egui::Context) {
        let Some((client, sys)) = self.resolve_wizard_text_client() else {
            self.wizard_set_error(
                "Provider credentials missing — open Preferences and sign in.".into(),
            );
            return;
        };
        let provider = self.settings.provider_slot().to_provider();
        let cfg = self.build_wizard_run_config();
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        session.state.prompt = session.prompt_draft.clone();
        let has_image_path = session.state.source_image.is_some();
        let (source_bytes, source_mime) = read_source_image(&session.state);
        let has_image = source_bytes.is_some();
        // Source image path is set but the file can't be read — likely moved or
        // deleted after being picked. Surface this rather than silently falling
        // back to text-only while the status line says "Reading the source image".
        if has_image_path && !has_image {
            session.error = Some(
                "Source image could not be read — it may have been moved or deleted. \
                 Clear the source image or choose a new one."
                    .into(),
            );
            return;
        }
        // An image-driven run needs a vision-capable text provider for the
        // brief/manifest; bail clearly instead of sending a photo into the
        // void on a text-only provider.
        if has_image && !provider.supports_images() {
            session.error = Some(format!(
                "{} can't read images — switch to a vision provider (Gemini, OpenAI, \
                 Xiaomi MiMo, Z.ai, Fireworks, or Claude Code) in Preferences, or clear the source image.",
                provider.label()
            ));
            return;
        }
        if !has_image && session.state.prompt.trim().is_empty() {
            session.error = Some("Enter a scene prompt or choose a source image first.".into());
            return;
        }
        let _ = persist::save(&session.state);
        session.running = WizardBusy::Brief;
        session.error = None;
        session.status = if has_image {
            "Reading the source image…".into()
        } else {
            "Generating scene brief…".into()
        };
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        let prompt = session.state.prompt.clone();
        std::thread::spawn(move || {
            let result = run_brief(client, sys, prompt, source_bytes, source_mime, cfg);
            let _ = tx.send(WizardMessage::BriefDone(result));
            ctx_clone.request_repaint();
        });
    }

    pub(in crate::app) fn start_wizard_manifest(&mut self, ctx: &egui::Context) {
        let Some((client, sys)) = self.resolve_wizard_text_client() else {
            self.wizard_set_error(
                "Provider credentials missing — open Preferences and sign in.".into(),
            );
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
        let (source_bytes, source_mime) = read_source_image(&session.state);
        session.running = WizardBusy::Manifest;
        session.error = None;
        session.status = "Generating object manifest…".into();
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let result = run_manifest(client, sys, prompt, brief, source_bytes, source_mime, cfg);
            let _ = tx.send(WizardMessage::ManifestDone(result));
            ctx_clone.request_repaint();
        });
    }

    /// Spawn up to `target_concurrency` reference-image workers, skipping
    /// objects that already have a file on disk or have a worker in flight.
    /// `auto_continue` arms `poll_wizard` to keep the pool topped up until
    /// the manifest is exhausted.
    pub(in crate::app) fn start_wizard_reference_batch(
        &mut self,
        ctx: &egui::Context,
        target_concurrency: usize,
        auto_continue: bool,
    ) {
        let Some(client) = self.resolve_wizard_image_client() else {
            // Don't keep retrying on the auto-continue tick.
            if let Some(s) = self.wizard.as_mut() {
                s.auto_continue_refs = false;
            }
            self.wizard_set_error(
                "Image generation needs a Gemini or Z.ai credential — check Preferences › Image."
                    .into(),
            );
            return;
        };
        let cfg = self.build_wizard_run_config();
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        // Pool already full: let the in-flight workers finish before topping up.
        if session.running_ref_ids.len() >= target_concurrency {
            return;
        }
        // Claim+reserve in one shot so the auto-continue poll can't re-issue
        // the same ids on the next 150ms tick before their PNGs land on disk.
        let pending = claim_pending_batch(
            &session.state.manifest,
            &mut session.running_ref_ids,
            target_concurrency,
            reference_missing,
        );
        if pending.is_empty() {
            if auto_continue {
                session.auto_continue_refs = false;
                session.status = "All reference images generated.".into();
            }
            return;
        }
        session.auto_continue_refs = auto_continue;
        session.error = None;
        let names: Vec<String> = pending.iter().map(|o| o.name.clone()).collect();
        session.status = format!(
            "Generating {} reference image(s): {}…",
            pending.len(),
            names.join(", ")
        );
        // Read the source image once here so batch spawns don't each re-read
        // the file from disk (one read per object for up to 18 objects is
        // wasteful; the bytes are cloned into each worker thread instead).
        let (source_bytes, source_mime) = read_source_image(&session.state);
        // Wrap in Arc so each worker shares the same client without forcing
        // `ImageClient: Clone` (its inner Gemini auth holds a Mutex).
        let client = Arc::new(client);
        let seed = cfg.seed;
        for obj in pending {
            self.spawn_reference_worker(
                ctx,
                obj,
                Arc::clone(&client),
                seed,
                source_bytes.clone(),
                source_mime.clone(),
            );
        }
    }

    /// Spawn a single reference-image worker. Assumes the caller has already
    /// reserved `obj.id` in `running_ref_ids` so the snapshot-and-filter loop
    /// can't double-issue an id. `source_bytes`/`source_mime` should be read
    /// once by the caller (via `read_source_image`) and passed in so batch
    /// spawns don't re-read the source file once per object.
    pub(in crate::app) fn spawn_reference_worker(
        &mut self,
        ctx: &egui::Context,
        obj: ObjectEntry,
        client: Arc<ImageClient>,
        seed: u64,
        source_bytes: Option<Vec<u8>>,
        source_mime: Option<String>,
    ) {
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        let out = session
            .state
            .references_dir()
            .join(format!("{}.png", obj.id));
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        let id = obj.id.clone();
        std::thread::spawn(move || {
            let result = run_reference_image(&client, obj, out, source_bytes, source_mime, seed);
            let _ = tx.send(WizardMessage::ReferenceDone { id, result });
            ctx_clone.request_repaint();
        });
    }

    /// (Re-)generate a single reference image by id. Drops any existing PNG
    /// from disk so the worker writes fresh bytes; reserves the slot up-front
    /// so it can't race with an auto-continue tick.
    pub(in crate::app) fn start_wizard_one_reference(&mut self, ctx: &egui::Context, id: String) {
        let Some(client) = self.resolve_wizard_image_client() else {
            self.wizard_set_error(
                "Image generation needs a Gemini or Z.ai credential — check Preferences › Image."
                    .into(),
            );
            return;
        };
        let seed = self.build_wizard_run_config().seed;
        let obj = {
            let Some(s) = self.wizard.as_mut() else {
                return;
            };
            if s.running_ref_ids.contains(&id) {
                return;
            }
            if let Some(obj) = s.state.find_object_mut(&id) {
                if let Some(p) = obj.reference_image.as_ref() {
                    let _ = std::fs::remove_file(p);
                }
                obj.reference_image = None;
            }
            let _ = persist::save(&s.state);
            s.status = format!("Generating reference image for {id}…");
            s.error = None;
            let Some(obj) = s.state.find_object(&id).cloned() else {
                return;
            };
            s.running_ref_ids.insert(id.clone());
            obj
        };
        let (source_bytes, source_mime) = {
            let Some(s) = self.wizard.as_ref() else {
                return;
            };
            read_source_image(&s.state)
        };
        self.spawn_reference_worker(ctx, obj, Arc::new(client), seed, source_bytes, source_mime);
    }

    /// Spawn up to `target_concurrency` per-object module workers, skipping
    /// objects that already have a `.mog` on disk or have a worker in flight.
    /// `auto_continue` keeps the pool topped up via `poll_wizard`.
    pub(in crate::app) fn start_wizard_object_batch(
        &mut self,
        ctx: &egui::Context,
        target_concurrency: usize,
        auto_continue: bool,
    ) {
        // Resolve credentials once; bail early so we don't spin the
        // auto-continue tick.
        if self.resolve_wizard_text_client().is_none() {
            if let Some(s) = self.wizard.as_mut() {
                s.auto_continue_objects = false;
            }
            self.wizard_set_error(
                "Provider credentials missing — open Preferences and sign in.".into(),
            );
            return;
        }
        let cfg = self.build_wizard_run_config();
        // Snapshot the work list under one short &mut borrow.
        let pending = {
            let Some(session) = self.wizard.as_mut() else {
                return;
            };
            // Pool already full: let the in-flight workers finish first.
            if session.running_object_ids.len() >= target_concurrency {
                return;
            }
            // Claim+reserve atomically (see `claim_pending_batch`).
            let pending = claim_pending_batch(
                &session.state.manifest,
                &mut session.running_object_ids,
                target_concurrency,
                object_missing,
            );
            if pending.is_empty() {
                if auto_continue {
                    session.auto_continue_objects = false;
                    session.status = "All object modules generated.".into();
                }
                return;
            }
            session.auto_continue_objects = auto_continue;
            session.error = None;
            let names: Vec<String> = pending.iter().map(|o| o.name.clone()).collect();
            session.status = format!(
                "Generating {} module(s): {}…",
                pending.len(),
                names.join(", ")
            );
            pending
        };
        for obj in pending {
            self.spawn_object_worker(ctx, obj, cfg.clone());
        }
    }

    /// Spawn a single per-object worker. Assumes the caller has already
    /// reserved `obj.id` in `running_object_ids` (the batch path does this
    /// up-front so the snapshot-and-filter loop can't double-issue an id).
    pub(in crate::app) fn spawn_object_worker(
        &mut self,
        ctx: &egui::Context,
        obj: ObjectEntry,
        cfg: WizardRunConfig,
    ) {
        let Some((client, sys)) = self.resolve_wizard_text_client() else {
            if let Some(s) = self.wizard.as_mut() {
                s.running_object_ids.remove(&obj.id);
                s.auto_continue_objects = false;
                s.error = Some("Lost provider credentials mid-batch.".into());
            }
            return;
        };
        let Some(session) = self.wizard.as_mut() else {
            return;
        };
        let out = session.state.objects_dir().join(format!("{}.mog", obj.id));
        let (ref_bytes, ref_mime) = if let Some(p) = obj.reference_image.as_ref() {
            std::fs::read(p)
                .ok()
                .map(|b| (Some(b), Some("image/png".to_string())))
                .unwrap_or((None, None))
        } else {
            (None, None)
        };
        let tx = session.tx.clone();
        let ctx_clone = ctx.clone();
        let id = obj.id.clone();
        std::thread::spawn(move || {
            let result = run_object_mog(client, sys, obj, out, ref_bytes, ref_mime, cfg);
            let _ = tx.send(WizardMessage::ObjectDone { id, result });
            ctx_clone.request_repaint();
        });
    }

    pub(in crate::app) fn start_wizard_regenerate_object(
        &mut self,
        ctx: &egui::Context,
        id: String,
    ) {
        let cfg = self.build_wizard_run_config();
        // Drop the existing .mog and reserve the worker slot up-front so the
        // regenerated object can't collide with an auto-continue tick that
        // also has it in its pending snapshot.
        let obj = {
            let Some(s) = self.wizard.as_mut() else {
                return;
            };
            if s.running_object_ids.contains(&id) {
                return;
            }
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
            let Some(obj) = s.state.find_object(&id).cloned() else {
                return;
            };
            s.running_object_ids.insert(id.clone());
            obj
        };
        self.spawn_object_worker(ctx, obj, cfg);
    }

    pub(in crate::app) fn start_wizard_assemble_and_build(&mut self, ctx: &egui::Context) {
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

    pub(in crate::app) fn start_wizard_object_review(&mut self, ctx: &egui::Context, id: String) {
        let Some((client, sys)) = self.resolve_wizard_text_client() else {
            self.wizard_set_error(
                "Provider credentials missing — open Preferences and sign in.".into(),
            );
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
        let img_path = obj
            .thumb_path
            .clone()
            .or_else(|| obj.reference_image.clone());
        let Some(img_path) = img_path else {
            session.error = Some(format!(
                "No image available for {id} — generate a reference first."
            ));
            return;
        };
        let Ok(image_bytes) = std::fs::read(&img_path) else {
            session.error = Some(format!("Couldn't read image at {}", img_path.display()));
            return;
        };
        let mime = guess_image_mime(&img_path);
        session.running = WizardBusy::ReviewObject;
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

    pub(in crate::app) fn start_wizard_scene_review(&mut self, ctx: &egui::Context) {
        let Some((client, sys)) = self.resolve_wizard_text_client() else {
            self.wizard_set_error(
                "Provider credentials missing — open Preferences and sign in.".into(),
            );
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
            session.error =
                Some("No scene screenshot yet — assemble + render the scene first.".into());
            return;
        };
        let Ok(bytes) = std::fs::read(&thumb) else {
            session.error = Some(format!(
                "Couldn't read scene screenshot at {}",
                thumb.display()
            ));
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

    pub(in crate::app) fn apply_wizard_corrections(&mut self, ctx: &egui::Context) {
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
                session.error = Some("No assembly file to edit — build the scene first.".into());
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
            let (new_src, applied) = crate::app::wizard::corrections::apply_corrections(
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
            session.state.scene_review = None;
            let _ = persist::save(&session.state);
            true
        };
        // Rebuild so the next screenshot reflects the corrections.
        if should_rebuild {
            self.start_wizard_assemble_and_build(ctx);
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

/// Read the run's source image (if any) into bytes + mime for handing to a
/// wizard worker thread. Returns `(None, None)` when there's no source image
/// or it can't be read (e.g. the user deleted the copy) — workers treat that
/// as a plain text-driven stage.
fn read_source_image(state: &WizardState) -> (Option<Vec<u8>>, Option<String>) {
    let Some(path) = state.source_image.as_ref() else {
        return (None, None);
    };
    match std::fs::read(path) {
        Ok(bytes) => (Some(bytes), Some(guess_image_mime(path))),
        Err(_) => (None, None),
    }
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
