//! Per-cell emission: one group per cell carrying interior walls with
//! door cutouts and a `centre` connector at the cell's centroid for
//! downstream furnishing modules to anchor to.
//!
//! Cells are room cells, staircases, or elevators (see `CellKind`). The
//! group name reflects the kind so the outliner reads "room_0" vs
//! "staircase_0" vs "elevator_0" — and so the lookup for the cab module
//! (in emit/circulation.rs) can find a stable parent group.
//!
//! Interior walls are deduplicated across cells — the wall between cells
//! A and B is emitted once, as a child of A (the cell with the lower
//! index).

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{Connector, NodeId, SceneGraph, Transform};

use crate::ast::Node;

use super::super::config::BuildingCfg;
use super::super::layout::{cell_kind_label, cell_kind_role, cell_type, CellKind, Floorplate};
use super::openings::OpeningPlan;
use super::wall_build::wall_with_holes;

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

    let walls = collect_interior_walls(plate);

    let mut cell_ids: Vec<NodeId> = Vec::with_capacity(plate.rooms.len());
    for (i, cell) in plate.rooms.iter().enumerate() {
        let centre = cell.rect.centre();
        let kind_label = cell_kind_label(cell);
        let cell_id = graph.add_child(
            parent,
            format!("{kind_label}_{i}"),
            "group",
            Transform::from_trs(
                Vec3::new(centre[0], 0.0, centre[1]),
                Quat::IDENTITY,
                Vec3::ONE,
            ),
        );
        graph.nodes[cell_id.0 as usize].origin = origin.clone();
        graph.nodes[cell_id.0 as usize].role = Some(cell_kind_role(cell).into());
        let mut tags = vec!["building".to_string(), kind_label.to_string()];
        if let Some(typ) = cell_type(cfg, cell) {
            tags.push(format!("room_type={}", typ.name));
        }
        graph.nodes[cell_id.0 as usize].tags.extend(tags);
        graph.nodes[cell_id.0 as usize].connectors.push(Connector::from_at_dir(
            "centre".to_string(),
            Vec3::ZERO,
            Vec3::Y,
            format!("{kind_label}_centre"),
            None,
        ));
        // Material override: rooms get their type's `mat=`, circulation
        // cells inherit from the building wrapper.
        if let Some(typ) = cell_type(cfg, cell) {
            if let Some(mat_name) = typ.mat.as_deref() {
                if let Some(mid) = graph.find_material_scoped(mat_name, origin.as_deref()) {
                    graph.set_material(cell_id, mid);
                }
            }
        }
        if graph.nodes[cell_id.0 as usize].material.is_none() {
            inherit_material_from_chain(cell_id, graph);
        }
        cell_ids.push(cell_id);
    }

    for (idx, (lo, hi, axis, fixed, range)) in walls.iter().enumerate() {
        let length = range.1 - range.0;
        if length <= wt * 1.5 {
            continue;
        }
        let mid_along = 0.5 * (range.0 + range.1);
        // Wall mesh has its long axis on local X. A "Vertical" wall (a
        // vertical line on the floor plan, running along world Z at fixed
        // world X) therefore needs a 90° Y rotation to map its local X axis
        // onto world Z. A "Horizontal" wall already runs along world X so it
        // stays at IDENTITY. The opposite assignment was the source of the
        // chaotic interior — every interior wall sat perpendicular to where
        // its containing cells expected it.
        let (centre_xyz, rot) = match axis {
            WallAxis::Vertical => (
                Vec3::new(*fixed, 0.5 * h, mid_along),
                Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            ),
            WallAxis::Horizontal => (Vec3::new(mid_along, 0.5 * h, *fixed), Quat::IDENTITY),
        };
        let parent_cell = cell_ids[*lo];

        let cell_world = graph.nodes[parent_cell.0 as usize].transform.translation;
        let local = centre_xyz - cell_world;

        let mut local_holes: Vec<[f32; 4]> = Vec::new();
        for door in &plan.interior_doors {
            if !door_belongs_to_wall(door, axis, *fixed, range) {
                continue;
            }
            // Vertical interior walls are rotated +90° around Y (local +X
            // → world -Z), so a door at world Z = `door.z` maps to local
            // X = -(door.z - mid_along). Horizontal walls stay at
            // identity so local X = door.x - mid_along. Currently doors
            // always sit at the shared edge midpoint, masking the bug,
            // but the formula needs to be correct for off-centre cases
            // (T-junction clamping).
            let along = match axis {
                WallAxis::Vertical => mid_along - door.z,
                WallAxis::Horizontal => door.x - mid_along,
            };
            let cy = 0.5 * door.height - 0.5 * h;
            local_holes.push([along, cy, door.width, door.height]);
        }

        let mesh = wall_with_holes([length, h, wt], &local_holes);
        let lo_kind = plate.rooms[*lo].kind;
        let hi_kind = plate.rooms[*hi].kind;
        let role = if matches!(lo_kind, CellKind::Room) && matches!(hi_kind, CellKind::Room) {
            "interior_wall"
        } else {
            "service_wall"
        };
        let wall_id = graph.add_child(
            parent_cell,
            format!("interior_wall_{idx}_{lo}_{hi}"),
            "wall",
            Transform::from_trs(local, rot, Vec3::ONE),
        );
        graph.set_mesh(wall_id, mesh);
        graph.nodes[wall_id.0 as usize].origin = origin.clone();
        graph.nodes[wall_id.0 as usize].role = Some(role.into());
        graph.nodes[wall_id.0 as usize].tags.extend([
            "building".into(),
            role.into(),
        ]);
        inherit_material_from_chain(wall_id, graph);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WallAxis {
    Vertical,
    Horizontal,
}

fn collect_interior_walls(
    plate: &Floorplate,
) -> Vec<(usize, usize, WallAxis, f32, (f32, f32))> {
    let mut out = Vec::new();
    let n = plate.rooms.len();
    for i in 0..n {
        for j in (i + 1)..n {
            // Elevators emit their own four-sided shaft enclosure in
            // `emit/circulation.rs` (N/E/S solid + W with one cutout
            // per storey at the elevator's centred opening). Adding a
            // per-storey cell-shared wall on the elevator face here
            // would double the wall and let the cell-wall door cutout
            // — which lands on the overlap midpoint, not the elevator
            // centre — block the shaft's correctly-placed cutout.
            if matches!(plate.rooms[i].kind, CellKind::Elevator)
                || matches!(plate.rooms[j].kind, CellKind::Elevator)
            {
                continue;
            }
            let a = &plate.rooms[i].rect;
            let b = &plate.rooms[j].rect;
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
