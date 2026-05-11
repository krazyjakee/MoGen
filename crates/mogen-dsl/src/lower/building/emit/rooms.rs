//! Per-room emission: a `room_<n>` group per cell carrying interior walls
//! with door cutouts, and a `centre` connector at the room's centroid for
//! downstream furnishing modules to anchor to.
//!
//! Interior walls are deduplicated across rooms — the wall between rooms A
//! and B is emitted once, as a child of A (the room with the lower index).
//! This matches the convention adjacent to the visible mesh.

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{Connector, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{box_mesh, clean_csg_output, difference_many, transform_mesh};

use crate::ast::Node;

use super::super::config::BuildingCfg;
use super::super::layout::{cell_type, Floorplate};
use super::openings::OpeningPlan;

pub(super) fn emit_rooms(
    node: &Node,
    cfg: &BuildingCfg,
    plate: &Floorplate,
    plan: &OpeningPlan,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let origin = node.origin.clone();
    let h = cfg.ceiling_height;
    let wt = cfg.wall_thickness;

    // Pre-build the unique shared-edge wall list. Each entry is
    // (lower-index room, higher-index room, edge axis, fixed-coord, range).
    let walls = collect_interior_walls(plate);

    // Build per-room groups so we can stamp room-type metadata + a centre
    // connector. Interior walls belong to the lower-index room of the pair.
    let mut room_ids: Vec<NodeId> = Vec::with_capacity(plate.rooms.len());
    for (i, cell) in plate.rooms.iter().enumerate() {
        let typ = cell_type(cfg, cell);
        let centre = cell.rect.centre();
        let room_id = graph.add_child(
            parent,
            format!("room_{i}"),
            "group",
            Transform::from_trs(
                Vec3::new(centre[0], 0.0, centre[1]),
                Quat::IDENTITY,
                Vec3::ONE,
            ),
        );
        graph.nodes[room_id.0 as usize].origin = origin.clone();
        graph.nodes[room_id.0 as usize].role = Some("room".into());
        graph.nodes[room_id.0 as usize]
            .tags
            .extend(["building".into(), format!("room_type={}", typ.name)]);
        // Centre connector — anchor for downstream furnishing modules.
        graph.nodes[room_id.0 as usize].connectors.push(Connector::from_at_dir(
            "centre".to_string(),
            Vec3::ZERO,
            Vec3::Y,
            "room_centre".to_string(),
            None,
        ));
        // Apply per-room material override if the room type carries one.
        if let Some(mat_name) = typ.mat.as_deref() {
            if let Some(mid) = graph.find_material_scoped(mat_name, origin.as_deref()) {
                graph.set_material(room_id, mid);
            }
        }
        if graph.nodes[room_id.0 as usize].material.is_none() {
            inherit_material_from_chain(room_id, graph);
        }
        room_ids.push(room_id);
    }

    // Emit each unique shared wall once, parented to the lower-index room.
    for (idx, (lo, hi, axis, fixed, range)) in walls.iter().enumerate() {
        let length = range.1 - range.0;
        if length <= wt * 1.5 {
            continue;
        }
        let mid_along = 0.5 * (range.0 + range.1);
        let (centre_xyz, rot) = match axis {
            WallAxis::Vertical => (Vec3::new(*fixed, 0.5 * h, mid_along), Quat::IDENTITY),
            WallAxis::Horizontal => (
                Vec3::new(mid_along, 0.5 * h, *fixed),
                Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            ),
        };
        let parent_room = room_ids[*lo];

        // Translate world-space centre into room-local space.
        let room_world = graph.nodes[parent_room.0 as usize].transform.translation;
        let local = centre_xyz - room_world;

        let mut local_holes: Vec<[f32; 4]> = Vec::new();
        for door in &plan.interior_doors {
            if !door_belongs_to_wall(door, axis, *fixed, range) {
                continue;
            }
            let along = match axis {
                WallAxis::Vertical => door.z - mid_along,
                WallAxis::Horizontal => door.x - mid_along,
            };
            let cy = 0.5 * door.height - 0.5 * h;
            local_holes.push([along, cy, door.width, door.height]);
        }

        let mesh = build_wall_mesh([length, h, wt], &local_holes);
        let wall_id = graph.add_child(
            parent_room,
            format!("interior_wall_{idx}_{lo}_{hi}"),
            "wall",
            Transform::from_trs(local, rot, Vec3::ONE),
        );
        graph.set_mesh(wall_id, mesh);
        graph.nodes[wall_id.0 as usize].origin = origin.clone();
        graph.nodes[wall_id.0 as usize].role = Some("wall".into());
        graph.nodes[wall_id.0 as usize].tags.extend([
            "building".into(),
            "interior_wall".into(),
        ]);
        inherit_material_from_chain(wall_id, graph);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WallAxis {
    /// Wall lying along the Z axis (variable Z, fixed X).
    Vertical,
    /// Wall lying along the X axis (variable X, fixed Z).
    Horizontal,
}

fn collect_interior_walls(
    plate: &Floorplate,
) -> Vec<(usize, usize, WallAxis, f32, (f32, f32))> {
    let mut out = Vec::new();
    let n = plate.rooms.len();
    for i in 0..n {
        for j in (i + 1)..n {
            let a = &plate.rooms[i].rect;
            let b = &plate.rooms[j].rect;
            // Vertical wall: matching x edge.
            if (a.x_max - b.x_min).abs() < 1e-3 {
                let lo = a.z_min.max(b.z_min);
                let hi = a.z_max.min(b.z_max);
                if hi > lo {
                    out.push((i, j, WallAxis::Vertical, a.x_max, (lo, hi)));
                }
            } else if (b.x_max - a.x_min).abs() < 1e-3 {
                let lo = a.z_min.max(b.z_min);
                let hi = a.z_max.min(b.z_max);
                if hi > lo {
                    out.push((i, j, WallAxis::Vertical, b.x_max, (lo, hi)));
                }
            } else if (a.z_max - b.z_min).abs() < 1e-3 {
                let lo = a.x_min.max(b.x_min);
                let hi = a.x_max.min(b.x_max);
                if hi > lo {
                    out.push((i, j, WallAxis::Horizontal, a.z_max, (lo, hi)));
                }
            } else if (b.z_max - a.z_min).abs() < 1e-3 {
                let lo = a.x_min.max(b.x_min);
                let hi = a.x_max.min(b.x_max);
                if hi > lo {
                    out.push((i, j, WallAxis::Horizontal, b.z_max, (lo, hi)));
                }
            }
        }
    }
    out
}

fn door_belongs_to_wall(
    door: &super::openings::Opening,
    axis: &WallAxis,
    fixed: f32,
    range: &(f32, f32),
) -> bool {
    match axis {
        WallAxis::Vertical => {
            (door.x - fixed).abs() < 0.05
                && door.z >= range.0 - 1e-3
                && door.z <= range.1 + 1e-3
        }
        WallAxis::Horizontal => {
            (door.z - fixed).abs() < 0.05
                && door.x >= range.0 - 1e-3
                && door.x <= range.1 + 1e-3
        }
    }
}

fn build_wall_mesh(size: [f32; 3], holes: &[[f32; 4]]) -> mogen_core::Mesh {
    let base = box_mesh(size, UvMode::Tile);
    if holes.is_empty() {
        return base;
    }
    let cutouts: Vec<mogen_core::Mesh> = holes
        .iter()
        .map(|&[hx, hy, hw, hh]| {
            let c = box_mesh(
                [hw.max(1e-4), hh.max(1e-4), size[2] + 0.02],
                UvMode::Tile,
            );
            transform_mesh(&c, glam::Mat4::from_translation(Vec3::new(hx, hy, 0.0)))
        })
        .collect();
    clean_csg_output(&difference_many(&base, &cutouts))
}

fn inherit_material_from_chain(id: NodeId, graph: &mut SceneGraph) {
    if graph.nodes[id.0 as usize].material.is_some() {
        return;
    }
    let mut cur = graph.nodes[id.0 as usize].parent;
    while let Some(p) = cur {
        if let Some(m) = graph.nodes[p.0 as usize].material {
            graph.set_material(id, m);
            return;
        }
        cur = graph.nodes[p.0 as usize].parent;
    }
}
