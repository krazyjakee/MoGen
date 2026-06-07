//! Points-of-interest pass: emits transform-only marker nodes a game engine
//! reads from the glTF to place gameplay content the generator leaves out.
//!
//! Each POI is parented under a `points_of_interest` group, carrying
//! `role=<kind>` and `tags=["dungeon", "poi", <kind>]`, and is a deterministic
//! function of the same `seed=` as the geometry (unaffected by `lod_scale`).
//!
//! Kinds:
//! - `entrance` — the exterior doorway carved through the perimeter wall,
//!   oriented so its forward (-Z) faces out through the door.
//! - `spawn` — where the player starts: the room the entrance leads into (a
//!   dead end when one exists, so the way in is a defensible cul-de-sac).
//! - `treasure_room` — every other dead-end room (one corridor in/out): a
//!   natural reward or ambush room.
//! - `stair_top` / `stair_bottom` — the head and foot of each flight.
//! - `prop_spot` — random points on room floors for scattered props.

use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Transform};

use crate::ast::Node;
use crate::lower::poi::{emit_poi_group, PoiDebug, PoiMarker};

use super::config::DungeonCfg;
use super::generate::DungeonLayout;
use super::materials::{poi_debug_color, poi_debug_mat_name};
use super::rng::{rand_range, sub_seed};

pub(super) fn emit_pois(
    node: &Node,
    cfg: &DungeonCfg,
    layout: &DungeonLayout,
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    if layout.rooms.is_empty() {
        return;
    }
    let cell = cfg.cell;
    let pitch = cfg.size[1] + cfg.floor_thickness;
    let half_w = layout.gw as f32 * cell * 0.5;
    let half_d = layout.gd as f32 * cell * 0.5;
    let cx = |i: i32| -half_w + (i as f32 + 0.5) * cell;
    let cz = |j: i32| -half_d + (j as f32 + 0.5) * cell;
    // Walking surface of a level = top of its floor deck.
    let floor_y = |level: usize| level as f32 * pitch + cfg.floor_thickness;

    let mut markers: Vec<PoiMarker> = Vec::new();
    let mut push = |kind: &str, pos: Vec3| {
        markers.push(PoiMarker {
            name_key: kind.to_string(),
            role: kind.to_string(),
            tags: vec!["dungeon".to_string(), "poi".to_string(), kind.to_string()],
            transform: Transform::from_trs(pos, Quat::IDENTITY, Vec3::ONE),
            debug: Some(PoiDebug {
                mat_name: poi_debug_mat_name(kind),
                color: poi_debug_color(kind),
                radius: 0.3,
            }),
        });
    };

    // Spawn: the room the entrance leads into (so the player starts just inside
    // the door), else a ground-level dead end, else the first ground-level room.
    let ground: Vec<usize> = layout
        .rooms
        .iter()
        .enumerate()
        .filter(|(_, r)| r.level == 0)
        .map(|(i, _)| i)
        .collect();
    let spawn_room = layout
        .entrance
        .map(|e| e.room)
        .or_else(|| ground.iter().copied().find(|&i| layout.room_degree[i] == 1))
        .or_else(|| ground.first().copied());
    if let Some(si) = spawn_room {
        let (ci, cj) = layout.rooms[si].center_cell();
        push("spawn", Vec3::new(cx(ci), floor_y(0), cz(cj)));
    }

    // Treasure rooms: every other dead-end room.
    for (i, r) in layout.rooms.iter().enumerate() {
        if Some(i) == spawn_room {
            continue;
        }
        if layout.room_degree[i] == 1 {
            let (ci, cj) = r.center_cell();
            push("treasure_room", Vec3::new(cx(ci), floor_y(r.level), cz(cj)));
        }
    }

    // Stair heads / feet.
    for stair in &layout.stairs {
        if let Some(&(fi, fj)) = stair.cells.first() {
            push(
                "stair_bottom",
                Vec3::new(cx(fi), floor_y(stair.lower_level), cz(fj)),
            );
        }
        if let Some(&(hi, hj)) = stair.cells.last() {
            push(
                "stair_top",
                Vec3::new(cx(hi), floor_y(stair.lower_level + 1), cz(hj)),
            );
        }
    }

    // Prop spots: random cells inside random rooms.
    if cfg.prop_spots > 0 {
        let mut state = sub_seed(cfg.seed, 0x9209_5807);
        for _ in 0..cfg.prop_spots {
            let r = layout.rooms[rand_range(&mut state, layout.rooms.len() as u32) as usize];
            let i = r.x0 + rand_range(&mut state, r.w.max(1) as u32) as i32;
            let j = r.z0 + rand_range(&mut state, r.d.max(1) as u32) as i32;
            push("prop_spot", Vec3::new(cx(i), floor_y(r.level), cz(j)));
        }
    }

    drop(push);

    // Entrance: at the exterior threshold, oriented so its forward (-Z) faces
    // outward through the doorway.
    if let Some(e) = layout.entrance {
        let yaw = (-(e.di as f32)).atan2(-(e.dj as f32));
        markers.push(PoiMarker {
            name_key: "entrance".to_string(),
            role: "entrance".to_string(),
            tags: vec![
                "dungeon".to_string(),
                "poi".to_string(),
                "entrance".to_string(),
            ],
            transform: Transform::from_trs(
                Vec3::new(cx(e.i), floor_y(0), cz(e.j)),
                Quat::from_rotation_y(yaw),
                Vec3::ONE,
            ),
            debug: Some(PoiDebug {
                mat_name: poi_debug_mat_name("entrance"),
                color: poi_debug_color("entrance"),
                radius: 0.3,
            }),
        });
    }

    emit_poi_group(
        graph,
        parent,
        node.origin.as_deref(),
        "points_of_interest",
        &["dungeon".to_string(), "points_of_interest".to_string()],
        cfg.debug_show_poi,
        markers,
    );
}
