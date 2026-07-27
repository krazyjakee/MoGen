//! Turning solved geometry into something usable.
//!
//! A sink only translates. Every decision about *what shape a thing is* has
//! already been made by [`super::resolve::solve`], and a sink that starts doing
//! arithmetic is the beginning of the two implementations drifting apart.
//!
//! - [`mog_text`] writes `.mog` source. Phase 1 ships here: an import produces
//!   a file the user can open and edit, which is the point of importing into
//!   this project rather than just rendering the JSON.
//! - [`mesh`] builds `Mesh` values. Phase 2 uses it: the `building` generator
//!   already lives inside the lowering pipeline, so it cannot round-trip
//!   through source text and wants the solved shapes as geometry directly.

pub(crate) mod mesh;
pub(crate) mod mog_text;
