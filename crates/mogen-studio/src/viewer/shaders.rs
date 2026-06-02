//! GLSL shader sources for the viewport renderer, grouped by pass.
//!
//! Each submodule holds the `#version 330 core` source strings for one
//! rendering concern. They're re-exported flat below so call sites keep
//! referencing `shaders::VS_SRC`, `shaders::GRID_FS`, etc. unchanged.

mod gizmo;
mod grid;
mod imposter;
mod mesh;
mod shadow;

pub(super) use gizmo::{GIZMO_FS, GIZMO_VS};
pub(super) use grid::{GRID_FS, GRID_VS};
pub(super) use imposter::{IMPOSTER_FS, IMPOSTER_VS};
pub(super) use mesh::{FS_SRC, VS_SRC};
pub(super) use shadow::{SHADOW_DIR_FS, SHADOW_DIR_VS, SHADOW_POINT_FS, SHADOW_POINT_VS};
