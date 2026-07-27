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
use crate::lower::arch;

use super::super::config::BuildingCfg;
use super::super::layout::adjacency::{self, WallAxis};
use super::super::layout::{cell_kind_label, cell_kind_role, cell_type, CellKind, Floorplate};
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

    let walls = adjacency::interior_walls(plate);

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

        // Furnishing POI markers: only real rooms get props (circulation
        // cells are walked-through, not furnished). Cloned name / copied
        // kind end the `cfg` borrow before the `&mut graph` call.
        if cfg.furnish {
            if let Some(typ) = cell_type(cfg, cell) {
                let room_name = typ.name.clone();
                let room_kind = typ.kind;
                super::furnish::emit_room_furnishings(
                    cfg,
                    cell.rect,
                    &room_name,
                    room_kind,
                    plan,
                    cell_id,
                    origin.as_deref(),
                    graph,
                );
            }
        }

        cell_ids.push(cell_id);
    }

    // Every interior wall on the storey is solved in one batch, because a
    // mitre is a property of a junction rather than of a wall: two walls
    // meeting at a T only know to cut into each other if they are solved
    // together. Walls too short to be worth emitting are left out of the batch
    // entirely, so they cannot pull a corner towards geometry that will never
    // exist — but they still consume an index, because the emitted node names
    // are numbered over the full run list and renaming geometry is not a
    // change this port is allowed to make.
    let mut requests: Vec<arch::WallRequest> = Vec::new();
    let mut request_of_run: Vec<Option<usize>> = Vec::with_capacity(walls.len());
    for run in &walls {
        if run.length() <= wt * 1.5 {
            request_of_run.push(None);
            continue;
        }
        let f = wall_frame(run, h);
        request_of_run.push(Some(requests.len()));
        requests.push(arch::WallRequest {
            start: f.start,
            end: f.end,
            thickness: wt,
            height: h,
            axis_x: f.axis_x,
            axis_z: f.axis_z,
            centre: [f.centre.x, f.centre.z],
            holes: door_holes(run, plan, h),
        });
    }
    let meshes = arch::solve_wall_meshes(&requests);

    for (idx, run) in walls.iter().enumerate() {
        let (lo, hi) = (&run.a, &run.b);
        let Some(slot) = request_of_run[idx] else { continue };
        let f = wall_frame(run, h);
        let (centre_xyz, rot) = (f.centre, f.rot);
        let parent_cell = cell_ids[*lo];

        let cell_world = graph.nodes[parent_cell.0 as usize].transform.translation;
        let local = centre_xyz - cell_world;

        let mesh = meshes[slot].clone();
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

/// Where one interior wall's node sits, and how its local frame maps to the
/// world.
///
/// The mesh has its long axis on local X. A "Vertical" wall — a vertical line
/// on the floor *plan*, running along world Z at a fixed world X — therefore
/// needs a 90° Y rotation to bring its local X onto world Z. A "Horizontal"
/// wall already runs along world X and stays at identity. The opposite
/// assignment was the source of the chaotic interior: every wall sat
/// perpendicular to where its containing cells expected it.
///
/// `axis_x` / `axis_z` restate that same rotation as exact unit vectors.
/// [`arch::solve_wall_meshes`] projects onto them rather than inverting the
/// quaternion, because `from_rotation_y(FRAC_PI_2)` has a cosine of −4.4e-8
/// and the generator promises byte-identical geometry from a seed.
struct WallFrame {
    centre: Vec3,
    rot: Quat,
    start: [f32; 2],
    end: [f32; 2],
    axis_x: [f32; 2],
    axis_z: [f32; 2],
}

fn wall_frame(run: &adjacency::WallRun, h: f32) -> WallFrame {
    let mid = run.midpoint();
    let (at, span) = (run.at, run.span);
    match run.axis {
        WallAxis::Vertical => WallFrame {
            centre: Vec3::new(at, 0.5 * h, mid),
            rot: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            // Local +X points at world −Z, so the centreline is listed in
            // decreasing Z to match. The solver copes either way, but keeping
            // them aligned means `sense` is +1 and the arithmetic reads
            // straight.
            start: [at, span.1],
            end: [at, span.0],
            axis_x: [0.0, -1.0],
            axis_z: [1.0, 0.0],
        },
        WallAxis::Horizontal => WallFrame {
            centre: Vec3::new(mid, 0.5 * h, at),
            rot: Quat::IDENTITY,
            start: [span.0, at],
            end: [span.1, at],
            axis_x: [1.0, 0.0],
            axis_z: [0.0, 1.0],
        },
    }
}

/// The doorways cut into one interior wall, in its own centred elevation.
fn door_holes(run: &adjacency::WallRun, plan: &OpeningPlan, h: f32) -> Vec<[f32; 4]> {
    let mid_along = run.midpoint();
    plan.interior_doors
        .iter()
        .filter(|d| door_belongs_to_wall(d, &run.axis, run.at, &run.span))
        .map(|door| {
            // Vertical interior walls are rotated +90° around Y (local +X →
            // world −Z), so a door at world Z maps to local X =
            // −(door.z − mid_along). Horizontal walls stay at identity so
            // local X = door.x − mid_along. Doors currently always sit at the
            // shared edge midpoint, which masks the difference, but the
            // formula has to be right for the off-centre cases T-junction
            // clamping produces.
            let along = match run.axis {
                WallAxis::Vertical => mid_along - door.z,
                WallAxis::Horizontal => door.x - mid_along,
            };
            [
                along,
                0.5 * door.height - 0.5 * h,
                door.width,
                door.height,
            ]
        })
        .collect()
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
