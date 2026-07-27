//! Auto-created default materials for terrain.
//!
//! Terrain needs a ground finish for its chunks and (when `sea_level>0`) a
//! water finish for the surface plane. The user can theme the ground by
//! attaching `mat=` to the `terrain` node, or override either by declaring their
//! own `material "terrain_ground"` / `material "terrain_water"` — anything
//! already declared on the same origin wins via `find_material_scoped`.

use std::path::Path;

use mogen_core::{AlphaMode, Material, SceneGraph};

use crate::lower::material::ensure_named_defaults;

/// Default ground material for terrain chunks.
pub(super) const GROUND_MAT: &str = "terrain_ground";
/// Default water material for the sea-level plane.
pub(super) const WATER_MAT: &str = "terrain_water";

/// Distinct emissive debug colour per POI kind, so `debug_show_poi` marker
/// spheres are colour-coded rather than all one colour.
pub(super) fn poi_debug_color(kind: &str) -> [f32; 3] {
    match kind {
        "peak" => [1.0, 0.2, 0.2],      // red — summits
        "flat_spot" => [0.2, 1.0, 0.3], // green — buildable flats
        "shoreline" => [0.1, 0.6, 1.0], // blue — water's edge
        _ => [1.0, 0.85, 0.1],
    }
}

/// Material name for a POI kind's debug marker (`terrain_poi_<kind>`).
pub(super) fn poi_debug_mat_name(kind: &str) -> String {
    format!("terrain_poi_{kind}")
}

pub(super) fn ensure_defaults(graph: &mut SceneGraph, origin: Option<&Path>) {
    let defaults: &[(&str, fn() -> Material)] = &[
        // White base, high roughness. The terrain bake writes the real
        // grass/rock/sand/mud tones into per-vertex COLOR_0, which multiplies
        // this base — so it must stay white for the baked colours to show true.
        (GROUND_MAT, || {
            let mut m = Material::new(GROUND_MAT);
            m.base_color = [1.0, 1.0, 1.0, 1.0];
            m.roughness = 0.95;
            m.metallic = 0.0;
            m
        }),
        // Translucent water; same treatment as the cave water finish.
        (WATER_MAT, || {
            let mut m = Material::new(WATER_MAT);
            m.base_color = [0.12, 0.28, 0.34, 0.55];
            m.roughness = 0.08;
            m.alpha_mode = AlphaMode::Blend;
            m.transmission = 0.9;
            m.double_sided = true;
            m.shader_name = Some(mogen_core::shader::WATER.to_string());
            m
        }),
    ];
    ensure_named_defaults(graph, origin, defaults);
}
