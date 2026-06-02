//! Auto-created default materials for caves.
//!
//! A cave needs a rock finish for its shell + dry decorations and a water
//! finish for pools / lakes. The user can theme either by attaching `mat=` to
//! the `cave` node (rock) / `water_mat=` (water) or by declaring their own
//! `material "cave_rock"` / `material "cave_water"` before the node — anything
//! already declared on the same origin wins via `find_material_scoped`.

use std::path::Path;

use mogen_core::{AlphaMode, Material, MaterialShader, SceneGraph};

/// Default rock material for the cave shell and dry decorations.
pub(super) const ROCK_MAT: &str = "cave_rock";
/// Default water material for pools and lakes.
pub(super) const WATER_MAT: &str = "cave_water";
/// Distinct emissive debug colour per POI kind, so the `debug_show_poi` marker
/// spheres are colour-coded in a glTF preview rather than all reading as one
/// yellow blob. Unknown kinds fall back to the original amber.
pub(super) fn poi_debug_color(kind: &str) -> [f32; 3] {
    match kind {
        "dead_end_chamber" => [1.0, 0.1, 0.7], // magenta — treasure / ambush room
        "column_base" => [0.2, 0.4, 1.0],      // deep blue — foot of a stone column
        "ladder_anchor" => [0.6, 1.0, 0.0],    // lime — ladder / rope climb point
        "mushroom_spot" => [1.0, 0.55, 0.0],   // amber — scattered floor props
        _ => [1.0, 0.85, 0.1],
    }
}

/// Material name for a POI kind's debug marker (`cave_poi_<kind>`).
pub(super) fn poi_debug_mat_name(kind: &str) -> String {
    format!("cave_poi_{kind}")
}

/// Create the per-kind POI debug material on demand (only when `debug_show_poi`
/// actually emits marker geometry). A user-declared `cave_poi_<kind>` wins, same
/// as the rock/water defaults.
pub(super) fn ensure_poi_debug(graph: &mut SceneGraph, origin: Option<&Path>, kind: &str) {
    let name = poi_debug_mat_name(kind);
    if graph.find_material_scoped(&name, origin).is_some() {
        return;
    }
    let [r, g, b] = poi_debug_color(kind);
    let mut m = Material::new(&name);
    m.base_color = [r, g, b, 1.0];
    m.emissive = [r, g, b];
    m.emissive_strength = 2.0;
    m.roughness = 0.5;
    m.origin = origin.map(|p| p.to_path_buf());
    graph.add_material(m);
}

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
        // visible tint where they don't. The `Water` shader override drives
        // Studio's animated waves preview; the exported .glb keeps the PBR
        // scalars above since glTF can't carry the custom shader.
        (WATER_MAT, || {
            let mut m = Material::new(WATER_MAT);
            m.base_color = [0.12, 0.22, 0.28, 0.55];
            m.roughness = 0.08;
            m.alpha_mode = AlphaMode::Blend;
            m.transmission = 0.9;
            m.double_sided = true;
            m.shader = MaterialShader::Water;
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
