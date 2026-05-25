//! Per-stage UI draw functions and the worker-message fold function. Each
//! `draw_*_stage` borrows `&mut WizardSession`, returns a `WizardAction`, and
//! is called from the main `ui_wizard` window method.

use eframe::egui;

use super::{WizardAction, WizardBusy, WizardSession};
use super::super::wizard::persist;
use super::super::wizard::state::{
    next_pending_review_object, ObjectGenResult, Stage, WizardMessage,
};

pub(super) fn draw_stage_strip(ui: &mut egui::Ui, session: &WizardSession) {
    const STAGES: &[Stage] = &[
        Stage::Prompt,
        Stage::Brief,
        Stage::Manifest,
        Stage::References,
        Stage::Objects,
        Stage::Assemble,
        Stage::ReviewObjects,
        Stage::ReviewScene,
        Stage::Done,
    ];
    ui.horizontal_wrapped(|ui| {
        for (i, &stage) in STAGES.iter().enumerate() {
            let active = stage == session.state.stage;
            let reached = stage.order() <= session.state.stage.order();
            let mut text = egui::RichText::new(format!(
                "{}. {}",
                stage.order() + 1,
                stage.label()
            ));
            if active {
                text = text.strong();
            } else if !reached {
                text = text.weak();
            }
            ui.label(text);
            if i < STAGES.len() - 1 {
                ui.label("→");
            }
        }
    });
}

pub(super) fn draw_prompt_stage(ui: &mut egui::Ui, session: &mut WizardSession) -> WizardAction {
    let mut action = WizardAction::None;
    ui.label("Scene prompt:");
    let id = egui::Id::new("wizard_prompt_draft");
    ui.add(
        egui::TextEdit::multiline(&mut session.prompt_draft)
            .hint_text("e.g. a cosy reading nook with a chair, lamp, and a stack of books")
            .desired_rows(4)
            .desired_width(f32::INFINITY)
            .id(id),
    );
    ui.add_space(8.0);
    ui.label(
        "The wizard chains brief → manifest → per-object images → per-object models \
         → assemble → visual review. Each stage is gated behind an explicit click \
         so you can inspect and edit between calls.",
    );
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let busy = !matches!(session.running, WizardBusy::None);
        let can_run = !busy && !session.prompt_draft.trim().is_empty();
        if ui
            .add_enabled(can_run, egui::Button::new("Generate brief →"))
            .on_hover_text("Calls the active LLM provider for a one-paragraph design brief")
            .clicked()
        {
            action = WizardAction::GenerateBrief;
        }
        if ui.button("Save").clicked() {
            action = WizardAction::SavePrompt;
        }
        if ui.button("Reset wizard").on_hover_text("Discard manifest and start over").clicked() {
            action = WizardAction::Reset;
        }
        if busy {
            ui.spinner();
        }
    });
    action
}

pub(super) fn draw_brief_stage(ui: &mut egui::Ui, session: &mut WizardSession) -> WizardAction {
    let mut action = WizardAction::None;
    let busy = matches!(session.running, WizardBusy::Brief);
    if let Some(brief) = session.state.brief.clone() {
        ui.label("Scene brief:");
        let mut buf = brief.clone();
        let resp = ui.add(
            egui::TextEdit::multiline(&mut buf)
                .desired_rows(6)
                .desired_width(f32::INFINITY),
        );
        if resp.changed() {
            session.state.brief = Some(buf);
        }
    } else if busy {
        ui.label("Calling the LLM for a scene brief…");
        ui.spinner();
    } else {
        ui.label("No brief yet — go back to the Prompt stage to generate one.");
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("← Back").clicked() {
            action = WizardAction::BackToPrompt;
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Regenerate brief"))
            .clicked()
        {
            action = WizardAction::RegenerateBrief;
        }
        let has_brief = session
            .state
            .brief
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if ui
            .add_enabled(has_brief && !busy, egui::Button::new("Next → Manifest"))
            .clicked()
        {
            action = WizardAction::AdvanceToManifest;
        }
        if busy {
            ui.spinner();
        }
    });
    action
}

pub(super) fn draw_manifest_stage(
    ui: &mut egui::Ui,
    session: &mut WizardSession,
) -> WizardAction {
    let mut action = WizardAction::None;
    let busy = matches!(session.running, WizardBusy::Manifest);
    if session.state.manifest.is_empty() {
        if busy {
            ui.label("Calling the LLM for an object manifest…");
            ui.spinner();
        } else {
            ui.label("No manifest yet. Generate one to populate the scene plan.");
        }
    } else {
        ui.label(format!("{} objects planned:", session.state.manifest.len()));
        egui::ScrollArea::vertical()
            .max_height(280.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let mut remove: Option<String> = None;
                for obj in session.state.manifest.iter_mut() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&obj.name).strong());
                            ui.weak(format!("({} • id={})", obj.role, obj.id));
                            if ui.small_button("✕").on_hover_text("Remove from manifest").clicked() {
                                remove = Some(obj.id.clone());
                            }
                        });
                        ui.add(
                            egui::TextEdit::singleline(&mut obj.prompt)
                                .desired_width(f32::INFINITY),
                        );
                        ui.horizontal(|ui| {
                            ui.label("pos");
                            ui.add(egui::DragValue::new(&mut obj.position[0]).speed(0.05));
                            ui.add(egui::DragValue::new(&mut obj.position[1]).speed(0.05));
                            ui.add(egui::DragValue::new(&mut obj.position[2]).speed(0.05));
                            ui.label("rot Y°");
                            ui.add(egui::DragValue::new(&mut obj.rotation_y_deg).speed(1.0));
                        });
                        ui.horizontal(|ui| {
                            ui.label("size");
                            ui.add(egui::DragValue::new(&mut obj.size[0]).speed(0.02));
                            ui.add(egui::DragValue::new(&mut obj.size[1]).speed(0.02));
                            ui.add(egui::DragValue::new(&mut obj.size[2]).speed(0.02));
                        });
                    });
                }
                if let Some(id) = remove {
                    action = WizardAction::RemoveObject(id);
                }
            });
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("← Back").clicked() {
            action = WizardAction::BackToBrief;
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Generate manifest"))
            .clicked()
        {
            action = WizardAction::GenerateManifest;
        }
        if ui
            .add_enabled(
                !busy && !session.state.manifest.is_empty(),
                egui::Button::new("Regenerate"),
            )
            .clicked()
        {
            action = WizardAction::RegenerateManifest;
        }
        if ui
            .add_enabled(
                !busy && !session.state.manifest.is_empty(),
                egui::Button::new("Next → References"),
            )
            .clicked()
        {
            action = WizardAction::AdvanceToReferences;
        }
        if busy {
            ui.spinner();
        }
    });
    action
}

pub(super) fn draw_references_stage(
    ui: &mut egui::Ui,
    session: &mut WizardSession,
) -> WizardAction {
    let mut action = WizardAction::None;
    let busy = matches!(session.running, WizardBusy::Reference(_));
    let total = session.state.manifest.len();
    let done = session
        .state
        .manifest
        .iter()
        .filter(|o| o.reference_image.as_ref().map(|p| p.exists()).unwrap_or(false))
        .count();
    ui.label(format!(
        "Reference images: {done} of {total} generated"
    ));
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for obj in &session.state.manifest {
                ui.horizontal(|ui| {
                    let has = obj
                        .reference_image
                        .as_ref()
                        .map(|p| p.exists())
                        .unwrap_or(false);
                    ui.label(if has { "✓" } else { "•" });
                    ui.label(&obj.name);
                    ui.weak(format!("({})", obj.id));
                });
            }
        });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("← Back").clicked() {
            action = WizardAction::BackToManifest;
        }
        if ui
            .add_enabled(!busy && done < total, egui::Button::new("Generate next"))
            .clicked()
        {
            action = WizardAction::GenerateNextReference;
        }
        if ui.button("Skip references").clicked() {
            action = WizardAction::SkipReferences;
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Next → Objects"))
            .clicked()
        {
            action = WizardAction::AdvanceToObjects;
        }
        if busy {
            ui.spinner();
        }
    });
    action
}

pub(super) fn draw_objects_stage(
    ui: &mut egui::Ui,
    session: &mut WizardSession,
) -> WizardAction {
    let mut action = WizardAction::None;
    let busy = matches!(session.running, WizardBusy::Object(_));
    let total = session.state.manifest.len();
    let done = session
        .state
        .manifest
        .iter()
        .filter(|o| o.mog_path.as_ref().map(|p| p.exists()).unwrap_or(false))
        .count();
    ui.label(format!("Per-object modules: {done} of {total} generated"));
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for obj in &session.state.manifest {
                ui.horizontal(|ui| {
                    let has = obj.mog_path.as_ref().map(|p| p.exists()).unwrap_or(false);
                    ui.label(if has { "✓" } else { "•" });
                    ui.label(&obj.name);
                    ui.weak(format!("({})", obj.id));
                });
            }
        });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("← Back").clicked() {
            action = WizardAction::BackToReferences;
        }
        if ui
            .add_enabled(!busy && done < total, egui::Button::new("Generate next"))
            .clicked()
        {
            action = WizardAction::GenerateNextObject;
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Next → Assemble"))
            .clicked()
        {
            action = WizardAction::AdvanceToAssemble;
        }
        if busy {
            ui.spinner();
        }
    });
    action
}

pub(super) fn draw_assemble_stage(
    ui: &mut egui::Ui,
    session: &mut WizardSession,
) -> WizardAction {
    let mut action = WizardAction::None;
    let busy = matches!(
        session.running,
        WizardBusy::Assemble | WizardBusy::Build
    );
    if let Some(asm) = session.state.assembly_path.as_ref() {
        ui.label(format!("Assembly: {}", asm.display()));
    }
    if let Some(glb) = session.state.built_glb.as_ref() {
        ui.label(format!("Built: {}", glb.display()));
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("← Back").clicked() {
            action = WizardAction::BackToObjects;
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Assemble + build"))
            .clicked()
        {
            action = WizardAction::RunAssembleAndBuild;
        }
        if ui
            .add_enabled(
                session.state.assembly_path.is_some(),
                egui::Button::new("Open assembly in a tab"),
            )
            .clicked()
        {
            action = WizardAction::OpenAssemblyInTab;
        }
        if ui
            .add_enabled(
                session.state.built_glb.is_some(),
                egui::Button::new("Next → Per-object review"),
            )
            .clicked()
        {
            action = WizardAction::AdvanceToReviewObjects;
        }
        if busy {
            ui.spinner();
        }
    });
    action
}

pub(super) fn draw_review_objects_stage(
    ui: &mut egui::Ui,
    session: &mut WizardSession,
) -> WizardAction {
    let mut action = WizardAction::None;
    let busy = matches!(session.running, WizardBusy::ReviewObject(_));
    if let Some(next) = next_pending_review_object(&session.state) {
        ui.weak(format!("Next to review: {}", next.name));
    }

    egui::ScrollArea::vertical()
        .max_height(260.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for obj in &session.state.manifest {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&obj.name).strong());
                        ui.weak(format!("({})", obj.id));
                    });
                    let review = session.state.per_object_reviews.get(&obj.id).cloned();
                    if let Some(r) = review {
                        let colour = if r.pass {
                            egui::Color32::from_rgb(110, 200, 120)
                        } else {
                            egui::Color32::from_rgb(230, 130, 110)
                        };
                        ui.colored_label(
                            colour,
                            if r.pass { "PASS" } else { "FAIL" },
                        );
                        if !r.notes.is_empty() {
                            ui.label(&r.notes);
                        }
                        if !r.pass {
                            if ui.button("Regenerate object").clicked() {
                                action = WizardAction::RegenerateObject(obj.id.clone());
                            }
                        }
                    } else {
                        ui.horizontal(|ui| {
                            let can = !busy && (obj.thumb_path.is_some() || obj.reference_image.is_some());
                            if ui
                                .add_enabled(can, egui::Button::new("Review"))
                                .on_hover_text("Ask the LLM whether this model looks right")
                                .clicked()
                            {
                                action = WizardAction::ReviewObject(obj.id.clone());
                            }
                            if !can {
                                ui.weak("(no image to review)");
                            }
                        });
                    }
                });
            }
        });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("← Back").clicked() {
            action = WizardAction::BackToAssemble;
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Next → Scene review"))
            .clicked()
        {
            action = WizardAction::AdvanceToReviewScene;
        }
        if busy {
            ui.spinner();
        }
    });
    action
}

pub(super) fn draw_review_scene_stage(
    ui: &mut egui::Ui,
    session: &mut WizardSession,
) -> WizardAction {
    let mut action = WizardAction::None;
    let busy = matches!(session.running, WizardBusy::SceneReview);

    ui.label(
        "Run an isometric render of the assembled scene through the LLM. \
         It returns a list of position corrections; you can review each one \
         before applying them via the span-aware editor.",
    );

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let has_thumb = session.state.scene_thumb.is_some();
        if let Some(p) = session.state.scene_thumb.as_ref() {
            ui.label(format!("screenshot: {}", p.display()));
        } else {
            ui.label(egui::RichText::new("No isometric screenshot yet").weak());
        }
        if ui
            .add_enabled(!busy, egui::Button::new(if has_thumb {
                "Recapture iso screenshot"
            } else {
                "Capture iso screenshot"
            }))
            .on_hover_text(
                "Open the assembly tab and render a 768px isometric screenshot \
                 (30° pitch / 45° yaw) — the LLM uses this for the scene review.",
            )
            .clicked()
        {
            action = WizardAction::CaptureScene;
        }
    });

    if let Some(review) = session.state.scene_review.clone() {
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Scene review:").strong());
        if !review.notes.is_empty() {
            ui.label(&review.notes);
        }
        if review.corrections.is_empty() {
            ui.label("No corrections needed.");
        } else {
            ui.label(format!("{} correction(s):", review.corrections.len()));
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for c in &review.corrections {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&c.object_id).strong());
                                if let Some(p) = c.new_position {
                                    ui.label(format!(
                                        "→ pos [{:.2}, {:.2}, {:.2}]",
                                        p[0], p[1], p[2]
                                    ));
                                }
                                if let Some(r) = c.new_rotation_y_deg {
                                    ui.label(format!("→ rot Y {:.1}°", r));
                                }
                            });
                            if !c.rationale.is_empty() {
                                ui.weak(&c.rationale);
                            }
                        });
                    }
                });
        }
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("← Back").clicked() {
            action = WizardAction::BackToReviewObjects;
        }
        if ui
            .add_enabled(
                !busy && session.correction_iterations < WizardSession::MAX_CORRECTION_ITERS,
                egui::Button::new("Run scene review"),
            )
            .on_hover_text(format!(
                "Run a fresh isometric render through the LLM (iteration {}/{})",
                session.correction_iterations + 1,
                WizardSession::MAX_CORRECTION_ITERS,
            ))
            .clicked()
        {
            action = WizardAction::RunSceneReview;
        }
        let has_correctable = session
            .state
            .scene_review
            .as_ref()
            .map(|r| !r.corrections.is_empty())
            .unwrap_or(false);
        if ui
            .add_enabled(
                !busy && has_correctable,
                egui::Button::new("Apply corrections"),
            )
            .on_hover_text("Patch pos/rot on each flagged group via the span-preserving editor and rebuild")
            .clicked()
        {
            action = WizardAction::ApplyCorrections;
        }
        if ui.button("Finish wizard →").clicked() {
            action = WizardAction::FinishWizard;
        }
        if busy {
            ui.spinner();
        }
    });
    action
}

pub(super) fn draw_done_stage(
    ui: &mut egui::Ui,
    session: &mut WizardSession,
) -> WizardAction {
    let mut action = WizardAction::None;
    ui.label(egui::RichText::new("Wizard complete!").strong());
    if let Some(p) = session.state.built_glb.as_ref() {
        ui.label(format!("Final GLB: {}", p.display()));
    }
    if let Some(p) = session.state.assembly_path.as_ref() {
        ui.label(format!("Assembly: {}", p.display()));
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Open assembly in a tab").clicked() {
            action = WizardAction::OpenAssemblyInTab;
        }
        if ui.button("Run again from scratch").clicked() {
            action = WizardAction::Reset;
        }
        if ui.button("Close").clicked() {
            action = WizardAction::Close;
        }
    });
    action
}

/// Fold a worker message into the live session, advancing stage state and
/// writing through to `state.json`.
pub(super) fn apply_wizard_message(session: &mut WizardSession, msg: WizardMessage) {
    match msg {
        WizardMessage::BriefDone(result) => {
            session.running = WizardBusy::None;
            match result {
                Ok(text) => {
                    session.state.brief = Some(text);
                    session.state.stage = Stage::Brief;
                    session.status = "Brief generated.".into();
                    let _ = persist::save(&session.state);
                }
                Err(e) => {
                    session.error = Some(format!("Brief failed: {e}"));
                }
            }
        }
        WizardMessage::ManifestDone(result) => {
            session.running = WizardBusy::None;
            match result {
                Ok(list) => {
                    let n = list.len();
                    session.state.manifest = list;
                    session.state.stage = Stage::Manifest;
                    session.status = format!("Manifest generated ({n} objects).");
                    let _ = persist::save(&session.state);
                }
                Err(e) => session.error = Some(format!("Manifest failed: {e}")),
            }
        }
        WizardMessage::ReferenceDone { id, result } => {
            session.running = WizardBusy::None;
            match result {
                Ok(path) => {
                    if let Some(obj) = session.state.find_object_mut(&id) {
                        obj.reference_image = Some(path);
                    }
                    session.status = format!("Reference for {id} ready.");
                    let _ = persist::save(&session.state);
                }
                Err(e) => session.error = Some(format!("Reference for {id} failed: {e}")),
            }
        }
        WizardMessage::ObjectDone { id, result } => {
            session.running = WizardBusy::None;
            match result {
                Ok(ObjectGenResult { mog_path, guide }) => {
                    if let Some(obj) = session.state.find_object_mut(&id) {
                        obj.mog_path = Some(mog_path);
                        obj.position_guide = Some(guide);
                    }
                    session.status = format!("Module for {id} ready.");
                    let _ = persist::save(&session.state);
                }
                Err(e) => session.error = Some(format!("Module for {id} failed: {e}")),
            }
        }
        WizardMessage::ObjectReviewDone { id, result } => {
            session.running = WizardBusy::None;
            match result {
                Ok(r) => {
                    session.state.per_object_reviews.insert(id.clone(), r);
                    session.status = format!("Reviewed {id}.");
                    let _ = persist::save(&session.state);
                }
                Err(e) => session.error = Some(format!("Object review for {id} failed: {e}")),
            }
        }
        WizardMessage::SceneReviewDone(result) => {
            session.running = WizardBusy::None;
            match result {
                Ok(r) => {
                    let n = r.corrections.len();
                    session.state.scene_review = Some(r);
                    session.status = format!("Scene review returned {n} correction(s).");
                    let _ = persist::save(&session.state);
                }
                Err(e) => session.error = Some(format!("Scene review failed: {e}")),
            }
        }
        WizardMessage::AssemblyDone(result) => match result {
            Ok(path) => {
                session.state.assembly_path = Some(path);
                session.running = WizardBusy::Build;
                session.status = "Assembly written; building GLB…".into();
                let _ = persist::save(&session.state);
            }
            Err(e) => {
                session.running = WizardBusy::None;
                session.error = Some(format!("Assemble failed: {e}"));
            }
        },
        WizardMessage::BuildDone(result) => {
            session.running = WizardBusy::None;
            match result {
                Ok(path) => {
                    session.state.built_glb = Some(path.clone());
                    session.status = format!("Built {}.", path.display());
                    let _ = persist::save(&session.state);
                }
                Err(e) => session.error = Some(format!("Build failed: {e}")),
            }
        }
    }
}
