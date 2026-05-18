//! MoGen renderer — flattens a [`mogen_core::SceneGraph`] into a glow-based
//! PBR draw stream and exposes both an in-process renderer (used by Studio)
//! and a headless thumbnail entry point (used by the `mogen render` CLI).
//!
//! Crate layout mirrors the directory the renderer used to live in inside
//! `mogen-studio/src/viewer`:
//!
//! - [`anim`] / [`flatten`] / [`camera`] are pure-math, no GL dependency.
//! - [`shaders`] holds the GLSL strings.
//! - [`gl_util`] / [`grid_gl`] / [`gizmo_gl`] / [`renderer`] are the glow
//!   pipeline.
//! - [`headless`] wraps glutin + winit so a CLI without a windowing toolkit
//!   can still produce a thumbnail.

pub mod anim;
pub mod camera;
pub mod flatten;
pub mod gizmo_gl;
mod gl_util;
pub mod grid_gl;
pub mod headless;
pub mod imposter;
pub mod renderer;
pub mod shaders;

pub use camera::{CameraSnapshot, OrbitCamera};
pub use flatten::{
    flatten, flatten_with_worlds, update_palettes, ClipSummary, DrawBatch, FlatMesh,
    PaletteSource, SkinPalette, FLOATS_PER_VERTEX, MAX_JOINTS,
};
pub use renderer::Renderer;

/// Which gizmo overlay variant the renderer should draw. Kept here (rather
/// than in `mogen-studio`) so the renderer's draw API doesn't depend on the
/// Studio crate; Studio re-exports this from its own `gizmo` module.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GizmoMode {
    #[default]
    Translate,
    Rotate,
    Scale,
}
