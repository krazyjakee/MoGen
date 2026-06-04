//! Points-of-interest pass: emits empty marker nodes a game engine reads from
//! the glTF to place gameplay content the generator deliberately leaves out.
//!
//! Each POI is a transform-only node (no mesh, no collider) parented under a
//! `points_of_interest` group, carrying `role=<kind>` and `tags=["cave",
//! "poi", <kind>]`. The exporter stamps both into `node.extras`, so a Godot
//! importer can find every marker by role and drop a prefab at its transform.
//! Markers are a deterministic function of the same `seed=` as the geometry,
//! and are unaffected by `lod_scale` — a low-detail bake keeps the same POIs.
//!
//! Five kinds:
//! - `entrance` — the floor under each mouth punched out to a side face: where a
//!   game drops a gate / door, oriented so its forward axis faces the open world.
//! - `dead_end_chamber` — a chamber the passage graph touches exactly once
//!   (one way in/out): a natural treasure room or ambush spot.
//! - `column_base` — the floor anchor of each stone column.
//! - `ladder_anchor` — the foot of every passage too steep to walk
//!   (`> max_slope`): a ladder / rope placement point.
//! - `mushroom_spot` — random points on chamber floors for scattered props.

use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Transform};

use crate::ast::Node;
use crate::lower::poi::{emit_poi_group, PoiDebug, PoiMarker};

use super::config::CaveCfg;
use super::decorate::{march_limit, pick_xz, surface_y};
use super::generate::{rock_field, CaveLayout};
use super::materials::{poi_debug_color, poi_debug_mat_name};
use super::rng::{rand_range, sub_seed};

pub(super) fn emit_points_of_interest(
    node: &Node,
    cfg: &CaveCfg,
    layout: &CaveLayout,
    column_bases: &[Vec3],
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    if layout.chambers.is_empty() {
        return;
    }
    let field = rock_field(layout);
    let blend = cfg.blend;

    // Build one marker per point of interest, then hand the batch to the shared
    // POI harness (grouping, naming, tags, optional debug spheres). Most markers
    // are placement-only (identity rotation); ladder anchors carry a yaw so a
    // prefab faces back out of the climb wall.
    let mut markers: Vec<PoiMarker> = Vec::new();
    let mut push = |kind: &'static str, pos: Vec3, rot: Quat| {
        markers.push(PoiMarker {
            name_key: kind.to_string(),
            role: kind.to_string(),
            tags: vec!["cave".to_string(), "poi".to_string(), kind.to_string()],
            transform: Transform::from_trs(pos, rot, Vec3::ONE),
            debug: Some(PoiDebug {
                mat_name: poi_debug_mat_name(kind),
                color: poi_debug_color(kind),
                radius: 0.18,
            }),
        });
    };

    // Entrances: the floor under each side-face mouth. The mouth was punched at
    // the chamber centre XZ, so march that column for the carved floor height —
    // same as a dead-end marker — and carry the outward-facing yaw.
    for &(ci, yaw) in &layout.entrances {
        if let Some(c) = layout.chambers.get(ci) {
            let y = surface_y(&field, blend, c.center.x, c.center.z, c.center.y, -0.12, march_limit(c, blend))
                .unwrap_or_else(|| c.floor_y());
            push("entrance", Vec3::new(c.center.x, y, c.center.z), Quat::from_rotation_y(yaw));
        }
    }

    // Dead-end chambers: one passage connection. Marker sits on the chamber
    // floor centre (field-marched, geometric fallback).
    for (i, c) in layout.chambers.iter().enumerate() {
        if layout.chamber_degree.get(i).copied() == Some(1) {
            let y = surface_y(&field, blend, c.center.x, c.center.z, c.center.y, -0.12, march_limit(c, blend))
                .unwrap_or_else(|| c.floor_y());
            push("dead_end_chamber", Vec3::new(c.center.x, y, c.center.z), Quat::IDENTITY);
        }
    }

    // Column bases (already floor-anchored by the decoration pass).
    for &base in column_bases {
        push("column_base", base, Quat::IDENTITY);
    }

    // Ladder / rope anchors at the foot of every climb passage. `generate`
    // gives the floor-disc-edge XZ; re-march the carved floor for the height so
    // the marker rests on the ground like the others.
    let ladder_limit = cfg.passage_radius * 3.0 + blend + 1.0;
    for &(anchor, yaw) in &layout.steep_links {
        let y = surface_y(&field, blend, anchor.x, anchor.z, anchor.y + cfg.passage_radius, -0.12, ladder_limit)
            .unwrap_or(anchor.y);
        push("ladder_anchor", Vec3::new(anchor.x, y, anchor.z), Quat::from_rotation_y(yaw));
    }

    // Mushroom spots: random points on random chamber floors.
    let mut state = sub_seed(cfg.seed, 0x0E01_5EED);
    for _ in 0..cfg.mushrooms {
        let c = &layout.chambers[rand_range(&mut state, layout.chambers.len() as u32) as usize];
        let (x, z) = pick_xz(&field, blend, c, &mut state);
        let y = surface_y(&field, blend, x, z, c.center.y, -0.12, march_limit(c, blend))
            .unwrap_or_else(|| c.floor_y());
        push("mushroom_spot", Vec3::new(x, y, z), Quat::IDENTITY);
    }

    drop(push);
    emit_poi_group(
        graph,
        parent,
        node.origin.as_deref(),
        "points_of_interest",
        &["cave".to_string(), "points_of_interest".to_string()],
        cfg.debug_show_poi,
        markers,
    );
}
