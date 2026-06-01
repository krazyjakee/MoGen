//! Fold a finished worker's [`WizardMessage`] into the live [`WizardSession`],
//! advancing stage state and writing through to `state.json`. Called once per
//! drained message from `MogenStudioApp::poll_wizard`.

use crate::app::wizard::persist;
use crate::app::wizard::state::{ObjectGenResult, Stage, WizardMessage};

use super::{WizardBusy, WizardSession};

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
            session.running_ref_ids.remove(&id);
            match result {
                Ok(path) => {
                    if let Some(obj) = session.state.find_object_mut(&id) {
                        obj.reference_image = Some(path);
                    }
                    session.status = format!("Reference for {id} ready.");
                    let _ = persist::save(&session.state);
                }
                Err(e) => {
                    // Stop the auto-loop on failure so the user can react
                    // before more spend lands.
                    session.auto_continue_refs = false;
                    session.error = Some(format!("Reference for {id} failed: {e}"));
                }
            }
        }
        WizardMessage::ObjectDone { id, result } => {
            session.running_object_ids.remove(&id);
            match result {
                Ok(ObjectGenResult { mog_path, guide }) => {
                    if let Some(obj) = session.state.find_object_mut(&id) {
                        obj.mog_path = Some(mog_path);
                        obj.position_guide = Some(guide);
                    }
                    session.status = format!("Module for {id} ready.");
                    let _ = persist::save(&session.state);
                }
                Err(e) => {
                    session.auto_continue_objects = false;
                    session.error = Some(format!("Module for {id} failed: {e}"));
                }
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
