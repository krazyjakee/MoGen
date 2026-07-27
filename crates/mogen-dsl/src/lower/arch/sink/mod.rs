//! Turning solved geometry into something usable.
//!
//! A sink only translates. Every decision about *what shape a thing is* has
//! already been made by [`super::resolve::solve`], and a sink that starts doing
//! arithmetic is the beginning of the two implementations drifting apart.
//!
//! - [`mog_text`] writes `.mog` source. Phase 1 ships here: an import produces
//!   a file the user can open and edit, which is the point of importing into
//!   this project rather than just rendering the JSON.
//! - A mesh sink follows in phase 2, when the `building` generator becomes the
//!   second producer and wants `SceneGraph` nodes directly.

pub(crate) mod mog_text;
