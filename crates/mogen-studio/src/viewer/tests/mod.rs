//! Unit tests for the viewer pipeline. Split by concern so each file
//! stays under ~500 lines.
//!
//! Submodule names are suffixed with `_tests` to avoid colliding with
//! sibling `viewer::*` modules (e.g. `viewer::flatten`) when sub-files
//! reach back up via `super::super::*`.

mod flatten_tests;
mod gizmo_commit_tests;
mod selection_tests;
mod track_tests;

use mogen_core::{Material, Mesh, TextureRef};
use std::path::PathBuf;

/// Unit quad on the XY plane with one face up +Z. Reused by every
/// flatten / batch test.
pub(super) fn quad_mesh() -> Mesh {
    let mut m = Mesh::new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        vec![[0.0, 0.0, 1.0]; 4],
        vec![0, 1, 2, 0, 2, 3],
    );
    m.uvs = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    m
}

pub(super) fn material_with_texture(name: &str, path: Option<&str>) -> Material {
    let mut m = Material::new(name);
    if let Some(p) = path {
        m.base_color_texture = Some(TextureRef::new(PathBuf::from(p)));
    }
    m
}
