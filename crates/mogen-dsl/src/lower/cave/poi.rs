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
//! Four kinds:
//! - `dead_end_chamber` — a chamber the passage graph touches exactly once
//!   (one way in/out): a natural treasure room or ambush spot.
//! - `column_base` — the floor anchor of each stone column.
//! - `ladder_anchor` — the foot of every passage too steep to walk
//!   (`> max_slope`): a ladder / rope placement point.
//! - `mushroom_spot` — random points on chamber floors for scattered props.

use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::icosphere_mesh;

use crate::ast::Node;

use super::config::CaveCfg;
use super::decorate::{march_limit, pick_xz, surface_y};
use super::generate::{rock_field, CaveLayout};
use super::materials::{ensure_poi_debug, poi_debug_mat_name};
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

    // Gather (kind, position) for every marker before touching the graph, so we
    // can skip emitting the group entirely when there is nothing to mark.
    // Most markers are placement-only (identity rotation); ladder anchors carry
    // a yaw so a prefab faces back out of the climb wall.
    let mut markers: Vec<(&'static str, Vec3, Quat)> = Vec::new();

    // Dead-end chambers: one passage connection. Marker sits on the chamber
    // floor centre (field-marched, geometric fallback).
    for (i, c) in layout.chambers.iter().enumerate() {
        if layout.chamber_degree.get(i).copied() == Some(1) {
            let y = surface_y(&field, blend, c.center.x, c.center.z, c.center.y, -0.12, march_limit(c, blend))
                .unwrap_or_else(|| c.floor_y());
            markers.push(("dead_end_chamber", Vec3::new(c.center.x, y, c.center.z), Quat::IDENTITY));
        }
    }

    // Column bases (already floor-anchored by the decoration pass).
    for &base in column_bases {
        markers.push(("column_base", base, Quat::IDENTITY));
    }

    // Ladder / rope anchors at the foot of every climb passage. `generate`
    // gives the floor-disc-edge XZ; re-march the carved floor for the height so
    // the marker rests on the ground like the others.
    let ladder_limit = cfg.passage_radius * 3.0 + blend + 1.0;
    for &(anchor, yaw) in &layout.steep_links {
        let y = surface_y(&field, blend, anchor.x, anchor.z, anchor.y + cfg.passage_radius, -0.12, ladder_limit)
            .unwrap_or(anchor.y);
        markers.push((
            "ladder_anchor",
            Vec3::new(anchor.x, y, anchor.z),
            Quat::from_rotation_y(yaw),
        ));
    }

    // Mushroom spots: random points on random chamber floors.
    let mut state = sub_seed(cfg.seed, 0x0E01_5EED);
    for _ in 0..cfg.mushrooms {
        let c = &layout.chambers[rand_range(&mut state, layout.chambers.len() as u32) as usize];
        let (x, z) = pick_xz(&field, blend, c, &mut state);
        let y = surface_y(&field, blend, x, z, c.center.y, -0.12, march_limit(c, blend))
            .unwrap_or_else(|| c.floor_y());
        markers.push(("mushroom_spot", Vec3::new(x, y, z), Quat::IDENTITY));
    }

    if markers.is_empty() {
        return;
    }

    let origin = node.origin.clone();
    let group = graph.add_child(parent, "points_of_interest".to_string(), "group", Transform::IDENTITY);
    graph.nodes[group.0 as usize].origin = origin.clone();
    graph.nodes[group.0 as usize]
        .tags
        .extend(["cave".to_string(), "points_of_interest".to_string()]);

    // Stable per-kind suffixes so two markers of the same kind get distinct
    // names (column_base_0, column_base_1, …).
    let mut counts: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    for (kind, pos, rot) in markers {
        let idx = counts.entry(kind).or_default();
        let xform = Transform::from_trs(pos, rot, Vec3::ONE);
        let id = graph.add_child(group, format!("{kind}_{idx}"), "poi", xform);
        *idx += 1;
        graph.nodes[id.0 as usize].origin = origin.clone();
        graph.nodes[id.0 as usize].role = Some(kind.to_string());
        graph.nodes[id.0 as usize]
            .tags
            .extend(["cave".to_string(), "poi".to_string(), kind.to_string()]);
        // Debug viz: a small emissive sphere per marker so the otherwise-empty
        // POIs are visible in a glTF preview, colour-coded per kind.
        if cfg.debug_show_poi {
            ensure_poi_debug(graph, origin.as_deref(), kind);
            if let Some(mid) = graph.find_material_scoped(&poi_debug_mat_name(kind), origin.as_deref()) {
                graph.set_mesh(id, icosphere_mesh(0.18, 1, UvMode::Tile));
                graph.set_material(id, mid);
            }
        }
    }
}
