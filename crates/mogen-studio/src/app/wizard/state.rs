//! Persistable wizard state plus the message envelopes background workers
//! post back to the UI.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One position-correction proposal emitted by the full-scene visual review.
/// Either or both of `new_position` / `new_rotation_y_deg` may be `None` when
/// only one needs adjusting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionCorrection {
    pub object_id: String,
    #[serde(default)]
    pub new_position: Option<[f32; 3]>,
    #[serde(default)]
    pub new_rotation_y_deg: Option<f32>,
    #[serde(default)]
    pub rationale: String,
}

/// LLM critique of a single rendered object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectReview {
    pub pass: bool,
    #[serde(default)]
    pub notes: String,
}

/// LLM critique of the full assembled scene plus suggested corrections.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SceneReview {
    #[serde(default)]
    pub corrections: Vec<PositionCorrection>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub iteration: u32,
}

/// Lightweight metadata that travels alongside each per-object `.mog`. Lets
/// the assembler place the object deterministically (anchor + footprint) and
/// gives the correction loop named connectors to reference. Computed from the
/// lowered `SceneGraph` after each per-object generate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionGuide {
    pub anchor: [f32; 3],
    pub up: [f32; 3],
    pub footprint_min: [f32; 3],
    pub footprint_max: [f32; 3],
    #[serde(default)]
    pub connectors: Vec<String>,
}

/// One object the LLM proposed for the scene. Mutable as the wizard advances —
/// `reference_image`, `mog_path`, `thumb_path`, and `position_guide` are
/// filled in by their respective stages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectEntry {
    pub id: String,
    pub name: String,
    pub role: String,
    pub prompt: String,
    #[serde(default = "default_size")]
    pub size: [f32; 3],
    #[serde(default)]
    pub position: [f32; 3],
    #[serde(default)]
    pub rotation_y_deg: f32,
    #[serde(default)]
    pub reference_image: Option<PathBuf>,
    #[serde(default)]
    pub mog_path: Option<PathBuf>,
    #[serde(default)]
    pub thumb_path: Option<PathBuf>,
    #[serde(default)]
    pub position_guide: Option<PositionGuide>,
}

fn default_size() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

/// Output of one per-object generate pass. Carried back through
/// [`WizardMessage::ObjectDone`] so the UI can stamp `mog_path` /
/// `position_guide` onto the matching `ObjectEntry`.
#[derive(Debug, Clone)]
pub struct ObjectGenResult {
    pub mog_path: PathBuf,
    pub guide: PositionGuide,
}

/// Linear stage progression. Each stage gates the next behind an explicit
/// user "Next" so the manifest, references, and per-object outputs can be
/// reviewed before spending more LLM/image calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Stage {
    /// User picks the save location that every wizard artefact lives under.
    /// Gates everything else so no LLM/image calls run before a folder is
    /// confirmed.
    #[default]
    Location,
    /// User edits the prompt and reviews cost estimates.
    Prompt,
    /// Brief generated; user reviews / can regenerate.
    Brief,
    /// Object manifest generated; user can edit individual entries.
    Manifest,
    /// Generating per-object reference images. Progress visible per object.
    References,
    /// Generating per-object `.mog` modules from the reference images.
    Objects,
    /// Assemble the per-object modules into a scene `.mog` and build to `.glb`.
    Assemble,
    /// Per-object visual checks against the original prompt; flag failures.
    ReviewObjects,
    /// Full-scene visual check; suggest and apply position corrections.
    ReviewScene,
    /// Wizard finished; user can re-run any stage or open the result.
    Done,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Location => "Save location",
            Stage::Prompt => "Prompt",
            Stage::Brief => "Brief",
            Stage::Manifest => "Object manifest",
            Stage::References => "Reference images",
            Stage::Objects => "Per-object models",
            Stage::Assemble => "Assemble & build",
            Stage::ReviewObjects => "Per-object review",
            Stage::ReviewScene => "Scene review",
            Stage::Done => "Done",
        }
    }

    pub fn order(self) -> u32 {
        match self {
            Stage::Location => 0,
            Stage::Prompt => 1,
            Stage::Brief => 2,
            Stage::Manifest => 3,
            Stage::References => 4,
            Stage::Objects => 5,
            Stage::Assemble => 6,
            Stage::ReviewObjects => 7,
            Stage::ReviewScene => 8,
            Stage::Done => 9,
        }
    }
}

/// The whole wizard state, serialised to `<project>/wizard/state.json` after
/// every meaningful update so a Studio restart picks up exactly where the
/// user left off.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WizardState {
    pub prompt: String,
    #[serde(default)]
    pub stage: Stage,
    /// Root directory under which every wizard artefact lives. Computed once
    /// when the wizard opens and never changes for the run.
    pub project_dir: PathBuf,
    #[serde(default)]
    pub brief: Option<String>,
    #[serde(default)]
    pub manifest: Vec<ObjectEntry>,
    #[serde(default)]
    pub assembly_path: Option<PathBuf>,
    #[serde(default)]
    pub built_glb: Option<PathBuf>,
    #[serde(default)]
    pub scene_thumb: Option<PathBuf>,
    #[serde(default)]
    pub per_object_reviews: HashMap<String, ObjectReview>,
    #[serde(default)]
    pub scene_review: Option<SceneReview>,
    /// Stable seed used for every LLM/image call in this run. Set once at
    /// open time so a wizard rerun against the same prompt reproduces.
    #[serde(default)]
    pub seed: u64,
    /// Optional source image driving the run. When set, the brief/manifest are
    /// derived from this image (vision input) and per-object references are cut
    /// out of it (image-to-image). Copied into `project_dir` at pick time so
    /// the run is self-contained and resumes after a restart.
    #[serde(default)]
    pub source_image: Option<PathBuf>,
}

impl WizardState {
    pub fn objects_dir(&self) -> PathBuf {
        self.project_dir.join("objects")
    }
    pub fn references_dir(&self) -> PathBuf {
        self.project_dir.join("references")
    }

    pub fn find_object(&self, id: &str) -> Option<&ObjectEntry> {
        self.manifest.iter().find(|o| o.id == id)
    }
    pub fn find_object_mut(&mut self, id: &str) -> Option<&mut ObjectEntry> {
        self.manifest.iter_mut().find(|o| o.id == id)
    }
}

/// First object that still needs a per-object visual review. Used to drive
/// the ReviewObjects stage's "next" button.
pub fn next_pending_review_object(state: &WizardState) -> Option<&ObjectEntry> {
    state
        .manifest
        .iter()
        .find(|o| !state.per_object_reviews.contains_key(&o.id))
}

/// Messages background workers post back to the UI thread.
#[derive(Debug)]
pub enum WizardMessage {
    BriefDone(Result<String, String>),
    ManifestDone(Result<Vec<ObjectEntry>, String>),
    ReferenceDone {
        id: String,
        result: Result<PathBuf, String>,
    },
    ObjectDone {
        id: String,
        result: Result<ObjectGenResult, String>,
    },
    ObjectReviewDone {
        id: String,
        result: Result<ObjectReview, String>,
    },
    SceneReviewDone(Result<SceneReview, String>),
    AssemblyDone(Result<PathBuf, String>),
    BuildDone(Result<PathBuf, String>),
}
