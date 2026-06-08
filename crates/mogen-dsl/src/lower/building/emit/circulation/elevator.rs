//! Elevator shafts and the shared shaft-enclosure walls. [`emit_elevator`]
//! builds a full-height N/E/S enclosure plus per-storey west wall pieces
//! (each carrying its own door cutout). [`emit_shaft_enclosure`] and
//! [`shaft_face_adjacencies`] are also reused by the staircase emitter to
//! wall its column, so they live here alongside the elevator that owns the
//! west-face handling.

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::box_mesh;

use crate::ast::Node;
use crate::lower::building::circulation::CirculationCell;
use crate::lower::building::config::BuildingCfg;
use crate::lower::building::emit::openings::elevator_door_z;
use crate::lower::building::emit::wall_build::wall_with_holes;
use crate::lower::building::layout::{CellKind, Rect2, StoreyPlate};
use crate::lower::poi::{emit_poi_group, PoiDebug, PoiMarker};

/// Thickness of the N/E/S solid shaft walls and the per-storey west pieces.
/// Kept thin to minimise visual weight while still providing a watertight
/// enclosure.
const SHAFT_WALL_THICKNESS: f32 = 0.05;

use super::{inherit_material_from_chain, storey_label};

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_elevator(
    node: &Node,
    cfg: &BuildingCfg,
    cell: &CirculationCell,
    idx: usize,
    bottom_storey: i32,
    top_storey: i32,
    storeys: &[StoreyPlate],
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let origin = node.origin.clone();
    let centre = cell.rect.centre();
    let step = cfg.ceiling_height + cfg.ceiling_thickness;
    let storeys_spanned = (top_storey - bottom_storey + 1).max(1) as f32;
    let total_h = storeys_spanned * step;
    let y_centre = bottom_storey as f32 * step + 0.5 * total_h - 0.5 * cfg.ceiling_thickness;

    let shaft_group = graph.add_child(
        parent,
        format!("elevator_{idx}"),
        "group",
        Transform::from_trs(
            Vec3::new(centre[0], y_centre, centre[1]),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
    );
    graph.nodes[shaft_group.0 as usize].origin = origin.clone();
    graph.nodes[shaft_group.0 as usize].role = Some("elevator".into());
    graph.nodes[shaft_group.0 as usize]
        .tags
        .extend(["building".into(), "elevator".into()]);

    // N/E/S solid walls, full-height. The W face is split into one piece
    // per storey below so each storey's door cutout can shift along Z to
    // match its own room layout — a single full-height wall could only
    // hold one X column per door (wall_with_holes merges X-overlapping
    // spans), so the per-storey shifts would smear into one giant hole.
    let (adj_n, adj_e, adj_s) = shaft_face_adjacencies(&cell.rect, storeys);
    emit_shaft_enclosure(
        cell.rect.width(),
        cell.rect.depth(),
        total_h,
        adj_n,
        adj_e,
        adj_s,
        shaft_group,
        graph,
        &origin,
    );
    emit_elevator_west_walls(
        cfg,
        cell,
        storeys,
        bottom_storey,
        top_storey,
        y_centre,
        shaft_group,
        graph,
        &origin,
    );
    emit_elevator_stop_pois(
        cfg,
        storeys,
        bottom_storey,
        top_storey,
        y_centre,
        shaft_group,
        graph,
        &origin,
    );
    Ok(())
}

/// One transform-only POI per served storey marking where the elevator cab
/// stops on that floor. Lets an engine drop its own elevator model and know
/// each floor's stop height. Anchored at the shaft centre on the storey's
/// floor surface, facing the (west) shaft door onto the adjacent room.
#[allow(clippy::too_many_arguments)]
fn emit_elevator_stop_pois(
    cfg: &BuildingCfg,
    storeys: &[StoreyPlate],
    bottom_storey: i32,
    top_storey: i32,
    y_centre: f32,
    group: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
) {
    let step = cfg.ceiling_height + cfg.ceiling_thickness;
    // Door is on the west (−X) face; point the marker's +Z forward at it.
    let facing = Quat::from_rotation_arc(Vec3::Z, Vec3::NEG_X);
    let mut markers: Vec<PoiMarker> = Vec::new();
    for sp in storeys {
        let s = sp.storey;
        if s < bottom_storey || s > top_storey {
            continue;
        }
        // Floor surface (world) sits at s*step; the group is centred at
        // y_centre, so the marker's group-local Y is s*step − y_centre.
        let local_y = s as f32 * step - y_centre;
        markers.push(PoiMarker {
            name_key: "elevator_stop".into(),
            role: "elevator_stop".into(),
            tags: vec![
                "building".into(),
                "poi".into(),
                "elevator".into(),
                "elevator_stop".into(),
                format!("floor={s}"),
            ],
            transform: Transform::from_trs(Vec3::new(0.0, local_y, 0.0), facing, Vec3::ONE),
            debug: Some(PoiDebug {
                mat_name: "building_poi_elevator_stop".into(),
                color: [0.80, 0.25, 0.95],
                radius: 0.12,
            }),
        });
    }
    emit_poi_group(
        graph,
        group,
        origin.as_deref(),
        "elevator_stops",
        &[
            "building".into(),
            "elevator".into(),
            "points_of_interest".into(),
        ],
        cfg.debug_show_poi,
        markers,
    );
}

/// Per-storey west wall pieces for the elevator. Each piece is one
/// `step` (ceiling + slab) tall, stacks flush against its neighbours, and
/// carries this storey's door cutout at the Z chosen by
/// `openings::elevator_door_z` — shifted off-centre when a room-room
/// interior wall would T-junction the elevator's west face inside the
/// cutout volume.
#[allow(clippy::too_many_arguments)]
fn emit_elevator_west_walls(
    cfg: &BuildingCfg,
    cell: &CirculationCell,
    storeys: &[StoreyPlate],
    bottom_storey: i32,
    top_storey: i32,
    y_centre: f32,
    group: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
) {
    let cell_w = cell.rect.width();
    let cell_d = cell.rect.depth();
    let step = cfg.ceiling_height + cfg.ceiling_thickness;
    let thickness = SHAFT_WALL_THICKNESS;
    let half = 0.5 * thickness;
    let elev_centre_z = 0.5 * (cell.rect.z_min + cell.rect.z_max);
    let door_w_default = cfg.door_w * 1.5;
    let door_h = cfg.door_h;
    let w_length = cell_d + 0.1;

    for sp in storeys {
        let s = sp.storey;
        if s < bottom_storey || s > top_storey {
            continue;
        }
        let elev_cell = sp.plate.rooms.iter().find(|c| {
            matches!(c.kind, CellKind::Elevator)
                && (c.rect.x_min - cell.rect.x_min).abs() < 1e-3
                && (c.rect.z_min - cell.rect.z_min).abs() < 1e-3
        });
        let z_world = match elev_cell {
            Some(ec) => elevator_door_z(cfg, &sp.plate, ec),
            None => elev_centre_z,
        };
        // Wall rotation = +π/2 about Y, so wall local +X maps to parent
        // local -Z. Door at parent (elevator-group) local Z =
        // z_world - elev_centre_z ⇒ wall local x = elev_centre_z - z_world.
        let along = elev_centre_z - z_world;

        let storey_y = s as f32 * step;
        let piece_centre_y = storey_y + 0.5 * cfg.ceiling_height - y_centre;
        let piece_height = step;
        // Door bottom flush with the storey floor (= piece bottom + ct/2).
        let cy_local = -0.5 * piece_height + 0.5 * door_h + 0.5 * cfg.ceiling_thickness;
        let hole = [along, cy_local, door_w_default, door_h];
        let mesh = wall_with_holes([w_length, piece_height, thickness], &[hole]);
        let pos = Vec3::new(-0.5 * cell_w - half, piece_centre_y, 0.0);
        let rot = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let id = graph.add_child(
            group,
            format!("shaft_wall_w_{}", storey_label(s)),
            "wall",
            Transform::from_trs(pos, rot, Vec3::ONE),
        );
        graph.set_mesh(id, mesh);
        graph.nodes[id.0 as usize].origin = origin.clone();
        graph.nodes[id.0 as usize].role = Some("shaft_wall".into());
        inherit_material_from_chain(id, graph);
    }
}

/// Emit the N/E/S solid walls of a shaft enclosure, parented to `group`
/// (which the caller has already centred on the cell in XZ and at the
/// cell's mid-Y). The west face is always left to the caller:
/// staircases let the per-storey cell-shared wall from `rooms.rs` carry
/// the door cutout, while elevators emit per-storey west pieces via
/// `emit_elevator_west_walls`.
///
/// `skip_n` / `skip_e` / `skip_s` extend that same exemption to the other
/// faces whenever an adjacent `Room` cell already provides closure: the
/// room's per-cell wall carries any door cutout the BFS placed there, and
/// a solid shaft_wall on top would shadow the cutout (visually hiding
/// the door slab and adding a trimesh collider that blocks passage).
pub(super) fn emit_shaft_enclosure(
    cell_w: f32,
    cell_d: f32,
    total_h: f32,
    skip_n: bool,
    skip_e: bool,
    skip_s: bool,
    group: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
) {
    let thickness = SHAFT_WALL_THICKNESS;
    let half = 0.5 * thickness;
    // Center each wall so its inner face sits flush with the cell
    // boundary — the wall body lives just *outside* the cell. The stair
    // body's first/last step ends a few mm short of the cell boundary,
    // so this keeps the enclosure from z-fighting with the steps while
    // still presenting a flush wall to anyone inside the cell.
    let walls: [(&str, bool, [f32; 3], [f32; 3]); 3] = [
        (
            "shaft_wall_n",
            skip_n,
            [0.0, 0.0, 0.5 * cell_d + half],
            [cell_w + 0.1, total_h, thickness],
        ),
        (
            "shaft_wall_e",
            skip_e,
            [0.5 * cell_w + half, 0.0, 0.0],
            [thickness, total_h, cell_d + 0.1],
        ),
        (
            "shaft_wall_s",
            skip_s,
            [0.0, 0.0, -0.5 * cell_d - half],
            [cell_w + 0.1, total_h, thickness],
        ),
    ];
    for (name, skip, pos, size) in walls {
        if skip {
            continue;
        }
        let mesh = box_mesh(size, UvMode::Tile);
        let id = graph.add_child(
            group,
            name.to_string(),
            "box",
            Transform::from_trs(
                Vec3::new(pos[0], pos[1], pos[2]),
                Quat::IDENTITY,
                Vec3::ONE,
            ),
        );
        graph.set_mesh(id, mesh);
        graph.nodes[id.0 as usize].origin = origin.clone();
        graph.nodes[id.0 as usize].role = Some("shaft_wall".into());
        inherit_material_from_chain(id, graph);
    }
}

/// Compute which of the N/E/S faces of `cell_rect` are abutted by a
/// `Room` cell on at least one storey. The west face is excluded —
/// the caller handles it directly.
pub(super) fn shaft_face_adjacencies(
    cell_rect: &Rect2,
    storeys: &[StoreyPlate],
) -> (bool, bool, bool) {
    let mut adj_n = false;
    let mut adj_e = false;
    let mut adj_s = false;
    for sp in storeys {
        for r in &sp.plate.rooms {
            if !matches!(r.kind, CellKind::Room) {
                continue;
            }
            let rr = &r.rect;
            // x ranges overlap (any positive intersection)?
            let x_overlap = rr.x_min < cell_rect.x_max - 1e-3
                && rr.x_max > cell_rect.x_min + 1e-3;
            // z ranges overlap?
            let z_overlap = rr.z_min < cell_rect.z_max - 1e-3
                && rr.z_max > cell_rect.z_min + 1e-3;
            if x_overlap && (rr.z_min - cell_rect.z_max).abs() < 1e-3 {
                adj_n = true;
            }
            if x_overlap && (rr.z_max - cell_rect.z_min).abs() < 1e-3 {
                adj_s = true;
            }
            if z_overlap && (rr.x_min - cell_rect.x_max).abs() < 1e-3 {
                adj_e = true;
            }
        }
    }
    (adj_n, adj_e, adj_s)
}
