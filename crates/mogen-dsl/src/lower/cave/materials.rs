//! Auto-created default materials for caves.
//!
//! A cave needs a rock finish for its shell + dry decorations and a water
//! finish for pools / lakes. The user can theme either by attaching `mat=` to
//! the `cave` node (rock) / `water_mat=` (water) or by declaring their own
//! `material "cave_rock"` / `material "cave_water"` before the node — anything
//! already declared on the same origin wins via `find_material_scoped`.

use std::path::Path;

use mogen_core::{AlphaMode, Material, SceneGraph};

/// Default rock material for the cave shell and dry decorations.
pub(super) const ROCK_MAT: &str = "cave_rock";
/// Default water material for pools and lakes.
pub(super) const WATER_MAT: &str = "cave_water";

pub(super) fn ensure_defaults(graph: &mut SceneGraph, origin: Option<&Path>) {
    let defaults: &[(&str, fn() -> Material)] = &[
        // Matt grey-brown stone. High roughness, no specular — reads as dry
        // cave rock under most lighting.
        (ROCK_MAT, || {
            let mut m = Material::new(ROCK_MAT);
            m.base_color = [0.34, 0.31, 0.28, 1.0];
            m.roughness = 0.95;
            m.metallic = 0.0;
            m
        }),
        // Dark translucent water. Blend + transmission so it reads as a still
        // pool in viewers that honour KHR_materials_transmission, with a
        // visible tint where they don't.
        (WATER_MAT, || {
            let mut m = Material::new(WATER_MAT);
            m.base_color = [0.12, 0.22, 0.28, 0.55];
            m.roughness = 0.08;
            m.alpha_mode = AlphaMode::Blend;
            m.transmission = 0.9;
            m.double_sided = true;
            m
        }),
    ];
    for (name, factory) in defaults {
        if graph.find_material_scoped(name, origin).is_some() {
            continue;
        }
        let mut mat = factory();
        mat.origin = origin.map(|p| p.to_path_buf());
        graph.add_material(mat);
    }
}
