//! Scene Wizard UI window plus the background-worker plumbing that drives
//! each stage. Owns a small `WizardSession` struct on the app so the rest
//! of the studio doesn't have to know about it.
//!
//! The implementation is split across focused submodules:
//! - [`session`] — the `MogenStudioApp` methods that open the window, poll
//!   worker messages, draw the frame, and dispatch button-click intents.
//! - [`workers`] — the `start_wizard_*` / `spawn_*` methods that spin up
//!   background threads for each LLM / image-generation stage.
//! - [`draw`] — the pure egui stage renderers and the `WizardAction` enum
//!   they emit.
//! - [`message`] — folding a finished worker's [`WizardMessage`] back into
//!   the live session.
//!
//! Shared state (`WizardSession`, `WizardBusy`) and the batch-claim helpers
//! live here so every submodule can reach them.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use crate::app::wizard::state::{ObjectEntry, WizardMessage, WizardState};

mod draw;
mod message;
mod session;
mod workers;

#[cfg(test)]
mod tests;

/// A reference image still needs generating when its PNG isn't on disk yet.
fn reference_missing(o: &ObjectEntry) -> bool {
    o.reference_image
        .as_ref()
        .map(|p| !p.exists())
        .unwrap_or(true)
}

/// A per-object module still needs generating when its `.mog` isn't on disk.
fn object_missing(o: &ObjectEntry) -> bool {
    o.mog_path.as_ref().map(|p| !p.exists()).unwrap_or(true)
}

/// Same intent as `next_pending_reference` but also skips ids that already
/// have a worker thread in flight, so the batch dispatcher doesn't double-
/// issue the same id before its file lands on disk.
fn next_pending_reference_skipping<'a>(
    state: &'a WizardState,
    in_flight: &HashSet<String>,
) -> Option<&'a ObjectEntry> {
    state
        .manifest
        .iter()
        .find(|o| reference_missing(o) && !in_flight.contains(&o.id))
}

/// Module equivalent of `next_pending_reference_skipping`.
fn next_pending_object_skipping<'a>(
    state: &'a WizardState,
    in_flight: &HashSet<String>,
) -> Option<&'a ObjectEntry> {
    state
        .manifest
        .iter()
        .find(|o| object_missing(o) && !in_flight.contains(&o.id))
}

/// Claim pending entries up to `target_concurrency - in_flight.len()` capacity, reserving them
/// in `in_flight` atomically — selection and reservation must stay one step to prevent the
/// 150ms poll tick from re-issuing in-flight ids before their artifacts land on disk.
fn claim_pending_batch(
    manifest: &[ObjectEntry],
    in_flight: &mut HashSet<String>,
    target_concurrency: usize,
    missing: impl Fn(&ObjectEntry) -> bool,
) -> Vec<ObjectEntry> {
    let capacity = target_concurrency.saturating_sub(in_flight.len());
    let claimed: Vec<ObjectEntry> = manifest
        .iter()
        .filter(|o| missing(o) && !in_flight.contains(&o.id))
        .take(capacity)
        .cloned()
        .collect();
    for o in &claimed {
        in_flight.insert(o.id.clone());
    }
    claimed
}

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
    /// Path the Location stage's picker starts in / shows as the candidate
    /// save location. Mirrors `state.project_dir` while the user is on the
    /// Location stage so editing the field doesn't fight egui.
    pub location_draft: PathBuf,
    /// Apply-corrections confirm latch on the ReviewScene stage.
    pub apply_corrections_armed: bool,
    /// Counter of correction iterations done on this run. Capped via
    /// [`WizardSession::MAX_CORRECTION_ITERS`] so a misbehaving model can't
    /// loop the wizard indefinitely.
    pub correction_iterations: u32,
    /// Reference-image worker ids currently in flight. Enables batching
    /// (multiple concurrent image generations) without re-launching the same
    /// object before its file lands.
    pub running_ref_ids: HashSet<String>,
    /// Per-object module worker ids currently in flight.
    pub running_object_ids: HashSet<String>,
    /// When true, `poll_wizard` keeps topping the reference pool back up to
    /// [`WizardSession::BATCH_SIZE`] until every manifest entry has a file
    /// or an error stops the run.
    pub auto_continue_refs: bool,
    /// When true, same as `auto_continue_refs` but for per-object modules.
    pub auto_continue_objects: bool,
}

/// Which stage's worker is in-flight (used for spinner / disable gating).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum WizardBusy {
    None,
    Brief,
    Manifest,
    ReviewObject(usize),
    SceneReview,
    Assemble,
    Build,
}

impl WizardSession {
    pub const MAX_CORRECTION_ITERS: u32 = 3;
    /// Max concurrent reference-image / per-object workers during bulk runs.
    /// Chosen to stay polite with provider rate limits while still hiding
    /// most of the per-call latency.
    pub const BATCH_SIZE: usize = 3;
}
