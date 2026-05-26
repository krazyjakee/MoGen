//! AI Scene Wizard — multi-stage pipeline that turns a single prompt into a
//! fully populated, isometric `.glb` scene by chaining the existing LLM, image,
//! validation, and build pipelines.
//!
//! Stages run forward through `Stage`; each completes asynchronously on a
//! worker thread and posts back through [`WizardMessage`]. The wizard owns
//! its own persistent state under `<project>/wizard/` so a partial run
//! survives Studio restarts.
//!
//! Submodule layout:
//! - `state` — `WizardState`, `Stage`, `ObjectEntry`, `PositionGuide`,
//!   `WizardMessage`, all the persistable data.
//! - `persist` — JSON load/save under the project's `wizard/` folder.
//! - `pipeline` — worker entry points for brief, manifest, references,
//!   per-object generation, and visual reviews.
//! - `assemble` — pure builder that emits the assembly `.mog` from the
//!   manifest plus generated per-object modules.
//! - `iso_preset` — fixed isometric camera/lighting preset shared by the
//!   viewer and the headless thumbnail path.
//! - `corrections` — apply LLM-suggested position/rotation deltas to the
//!   assembly source via the span-aware `edit.rs` helpers.

pub mod assemble;
pub mod corrections;
pub mod iso_preset;
pub mod persist;
pub mod pipeline;
pub mod state;

pub use assemble::write_assembly;
pub use iso_preset::iso_camera;
