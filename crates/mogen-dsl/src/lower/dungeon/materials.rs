//! Auto-created default materials for dungeons.
//!
//! A dungeon needs a floor finish (decks + steps a player walks on) and a stone
//! finish (walls + ceilings). The user can theme the whole dungeon by attaching
//! `mat=` to the `dungeon` node, or override either by declaring their own
//! `material "dungeon_floor"` / `material "dungeon_stone"` before the node —
//! anything already declared on the same origin wins via `find_material_scoped`.

use std::path::Path;

use mogen_core::{Material, SceneGraph};

use crate::lower::material::ensure_named_defaults;

/// Default floor material for decks and staircase steps.
pub(super) const FLOOR_MAT: &str = "dungeon_floor";
/// Default stone material for walls and ceilings.
pub(super) const STONE_MAT: &str = "dungeon_stone";

/// Distinct emissive debug colour per POI kind, so `debug_show_poi` marker
/// spheres are colour-coded rather than all one colour.
pub(super) fn poi_debug_color(kind: &str) -> [f32; 3] {
    match kind {
        "entrance" => [1.0, 0.95, 0.6],     // warm gold — exterior doorway
        "spawn" => [0.2, 1.0, 0.4],         // green — player start
        "treasure_room" => [1.0, 0.1, 0.7], // magenta — dead-end reward room
        "stair_top" => [0.0, 0.9, 1.0],     // cyan — head of a flight
        "stair_bottom" => [0.2, 0.4, 1.0],  // blue — foot of a flight
        "prop_spot" => [1.0, 0.55, 0.0],    // amber — scattered floor props
        _ => [1.0, 0.85, 0.1],
    }
}

/// Material name for a POI kind's debug marker (`dungeon_poi_<kind>`).
pub(super) fn poi_debug_mat_name(kind: &str) -> String {
    format!("dungeon_poi_{kind}")
}

pub(super) fn ensure_defaults(graph: &mut SceneGraph, origin: Option<&Path>) {
    let defaults: &[(&str, fn() -> Material)] = &[
        // Worn flagstone floor — mid grey, slightly warm, matte.
        (FLOOR_MAT, || {
            let mut m = Material::new(FLOOR_MAT);
            m.base_color = [0.40, 0.38, 0.35, 1.0];
            m.roughness = 0.9;
            m.metallic = 0.0;
            m
        }),
        // Darker dressed-stone wall, matte.
        (STONE_MAT, || {
            let mut m = Material::new(STONE_MAT);
            m.base_color = [0.30, 0.29, 0.27, 1.0];
            m.roughness = 0.95;
            m.metallic = 0.0;
            m
        }),
    ];
    ensure_named_defaults(graph, origin, defaults);
}
