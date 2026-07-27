//! Architectural IR and the geometry solving that turns it into shapes.
//!
//! # Why this exists
//!
//! The `building` generator describes a building as axis-aligned rectangles, so
//! it cannot express an L-shaped room, a wall at 30°, or a curved bay. This
//! module holds a richer vocabulary — walls as centrelines, slabs as polygons,
//! roofs as a type plus a pitch — modelled on pascalorg/editor's data model
//! (MIT), which was designed for exactly this job.
//!
//! Two producers fill in an [`ir::ArchModel`]: the Pascal-editor importer, and
//! (later) the `building` generator. One solver turns it into geometry, and two
//! sinks emit that geometry as either `.mog` source text or scene-graph meshes.
//!
//! **The rule that makes this worth doing: producers only map fields. Every
//! piece of geometry maths lives here.** Bury the mitre solve in the importer
//! and retargeting the generator becomes a second rewrite rather than a
//! deletion.
//!
//! # Invariants
//!
//! - **No RNG.** Nothing here may reach for [`crate::lower::rng`]. Ties break by
//!   deterministic index, and ids are `Vec` indices precisely so the solver
//!   never needs a hash map — hash iteration order is how determinism dies.
//! - **Watertight by construction.** The solver's output shapes are all closed
//!   solids; there is deliberately no open-surface variant for a sink to
//!   mishandle. This matters because `extrude_mesh` swallows triangulation
//!   failures and returns a capless mesh with no error, so an invalid polygon
//!   becomes a silent hole. [`plan::ring_is_simple`] is the guard in front of
//!   that.

// The solver is built bottom-up and nothing outside `arch/` consumes it until
// the sinks land, so most of it is legitimately unreferenced right now. Without
// this the crate emits ~40 dead-code warnings that would drown anything real.
// Remove once `sink/mog_text.rs` is wired in.
#![allow(dead_code)]

pub(crate) mod consts;
pub(crate) mod curve;
pub(crate) mod height;
pub(crate) mod ir;
pub(crate) mod junction;
pub(crate) mod miter;
pub(crate) mod openings;
pub(crate) mod plan;
pub(crate) mod resolve;
pub(crate) mod resolved;
pub(crate) mod roof;
pub(crate) mod sink;
pub(crate) mod validate;

#[cfg(test)]
mod tests;
