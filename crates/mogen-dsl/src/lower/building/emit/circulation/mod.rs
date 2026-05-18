//! Multi-storey circulation emission. Spans the entire building (not a
//! single storey), so it lives in its own subtree under the wrapper and
//! emits with absolute Y coordinates.
//!
//! Model:
//!
//! - **Staircase**: one straight flight between each pair of consecutive
//!   storeys, occupying the **east half** of the staircase's reserved XY
//!   cell. The west half stays clear at every storey and becomes a
//!   landing on floor N+1 (its slab is preserved while the east half is
//!   carved). Each flight is a series of `box` tread meshes climbing
//!   south→north from y = s*(h+ct) to y = (s+1)*(h+ct). On the upper
//!   storey, the user steps off the topmost step westward onto the
//!   landing, walks south, crosses a narrow strip of preserved slab at
//!   the cell's south end, and reaches the bottom of the next flight.
//! - **Elevator**: a vertical shaft formed by the per-storey cell walls
//!   (with door cutouts already carved by the interior-door planner)
//!   stacked with slab holes between them. The shaft "module" emission
//!   stays as an empty marker group; the actual enclosure comes from
//!   the cell walls so door openings line up.
//!
//! No CSG carving here — the slab cutouts are handled in `shell.rs`'s
//! per-storey emission. This module only authors the stair geometry
//! that fills the carved column.

mod elevator;
mod stair;

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Span, Transform, UvMode};
use mogen_geom::box_mesh;

use crate::ast::{Node, Value};

use super::super::circulation::CirculationKind;
use super::super::config::BuildingCfg;
use super::super::layout::{BuildingLayout, CellKind, Floorplate, Rect2, StoreyPlate};
use super::modules::emit_interior_door_slot;
use super::wall_build::wall_with_holes;

pub(in super::super) fn emit_circulation(
    node: &Node,
    cfg: &BuildingCfg,
    layout: &BuildingLayout,
    bottom_storey: i32,
    top_storey: i32,
    wrapper_id: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    if !layout.circulation.has_any() {
        return Ok(());
    }
    let origin = node.origin.clone();

    let circ_group = graph.add_child(
        wrapper_id,
        "circulation".to_string(),
        "group",
        Transform::IDENTITY,
    );
    graph.nodes[circ_group.0 as usize].origin = origin.clone();
    graph.nodes[circ_group.0 as usize]
        .tags
        .extend(["building".into(), "circulation".into()]);

    for (i, cell) in layout.circulation.cells.iter().enumerate() {
        match cell.kind {
            CirculationKind::Staircase => {
                stair::emit_staircase(
                    node,
                    cfg,
                    cell,
                    i,
                    bottom_storey,
                    top_storey,
                    &layout.storeys,
                    circ_group,
                    graph,
                )?;
            }
            CirculationKind::Elevator => {
                elevator::emit_elevator(
                    node,
                    cfg,
                    cell,
                    i,
                    bottom_storey,
                    top_storey,
                    &layout.storeys,
                    circ_group,
                    graph,
                )?;
            }
        }
    }

    emit_column_fillers(node, cfg, layout, circ_group, graph, &origin)?;
    Ok(())
}

/// Close the room-facing edge of the circulation column wherever no
/// circulation cell sits. The room's east wall (per-storey, emitted by
/// `rooms.rs`) only covers z ranges where a room cell shares an edge
/// with a circulation cell — the `COLUMN_INSET` strips between stacked
/// cells, and the column extent past the topmost / before the bottommost
/// cell, would otherwise leave a slit (and sometimes a metres-wide hole)
/// straight from the rooms into the unwalled column interior.
///
/// Fillers are emitted per-storey (not as one multi-storey slab) so each
/// gets its own door cutout when the gap is wide enough to be a usable
/// alcove and the storey has an adjacent room. A single multi-storey
/// slab can't carry per-floor doors at the same z midpoint because
/// `wall_with_holes` merges x-overlapping cutouts into one giant hole.
fn emit_column_fillers(
    node: &Node,
    cfg: &BuildingCfg,
    layout: &BuildingLayout,
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
) -> Result<()> {
    if !layout.circulation.has_any() {
        return Ok(());
    }
    let column_x = layout.bounds.x_max - layout.circulation.column_width;

    let mut intervals: Vec<(f32, f32)> = layout
        .circulation
        .cells
        .iter()
        .map(|c| (c.rect.z_min, c.rect.z_max))
        .collect();
    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut gaps: Vec<(f32, f32)> = Vec::new();
    let mut cursor = layout.bounds.z_min;
    for (a, b) in intervals {
        if a - cursor > 1e-3 {
            gaps.push((cursor, a));
        }
        if b > cursor {
            cursor = b;
        }
    }
    if layout.bounds.z_max - cursor > 1e-3 {
        gaps.push((cursor, layout.bounds.z_max));
    }
    if gaps.is_empty() {
        return Ok(());
    }

    let h = cfg.ceiling_height;
    let step = h + cfg.ceiling_thickness;
    let thickness = cfg.wall_thickness;
    // Door cutouts only on gaps wide enough to read as a real alcove
    // (door width plus a wall-thickness pier on each side, plus a bit of
    // slack). Below this we leave the strip sealed — a slit ≤ door_w is
    // wasted floorplan, not a habitable space.
    let door_min_length = cfg.door_w + 2.0 * cfg.wall_thickness + 0.1;

    for (idx, (z0, z1)) in gaps.iter().enumerate() {
        let length = z1 - z0;
        if length < 1e-3 {
            continue;
        }
        let z_centre = 0.5 * (z0 + z1);
        let allow_door = length >= door_min_length;

        for storey_plate in &layout.storeys {
            let s = storey_plate.storey;
            let storey_floor = s as f32 * step;
            let wall_centre_y = storey_floor + 0.5 * h;

            let mut local_holes: Vec<[f32; 4]> = Vec::new();
            let mut carved_door = false;
            if allow_door
                && storey_has_adjacent_room(&storey_plate.plate, column_x, *z0, *z1)
            {
                // Wall is rotated +π/2 around Y (local +X → world -Z), so
                // a door at the gap's z midpoint has along = 0. Wall y
                // centre = storey_floor + h/2; door y centre = storey_floor
                // + door_h/2; local cy = (door_h - h)/2.
                let cy = 0.5 * (cfg.door_h - h);
                local_holes.push([0.0, cy, cfg.door_w, cfg.door_h]);
                carved_door = true;
            }

            let mesh = wall_with_holes([length, h, thickness], &local_holes);
            let id = graph.add_child(
                parent,
                format!("column_filler_{idx}_{}", storey_label(s)),
                "wall",
                Transform::from_trs(
                    Vec3::new(column_x, wall_centre_y, z_centre),
                    Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                    Vec3::ONE,
                ),
            );
            graph.set_mesh(id, mesh);
            graph.nodes[id.0 as usize].origin = origin.clone();
            graph.nodes[id.0 as usize].role = Some("service_wall".into());
            graph.nodes[id.0 as usize].tags.extend([
                "building".into(),
                "service_wall".into(),
            ]);
            inherit_material_from_chain(id, graph);

            // Drop a door panel + slot into the cutout we just carved. The
            // BFS in `place_interior_doors` only sees `RoomCell`s, so the
            // column-filler gap is invisible to it and would otherwise leave
            // a hole in the wall with no door geometry and no slot metadata
            // for engine importers to swap.
            if carved_door {
                emit_interior_door_slot(
                    node,
                    cfg,
                    parent,
                    graph,
                    column_x,
                    z_centre,
                    storey_floor,
                    [-1.0, 0.0, 0.0],
                )?;
            }
        }
    }
    Ok(())
}

/// True when some `Room` cell on this storey shares its east edge with the
/// column (`x_max == column_x`) and overlaps the gap's z range. Circulation
/// cells are excluded — a door from a stair / elevator into the alcove
/// would let people walk out of a stairwell sideways, which is what
/// `place_interior_doors` was already careful to avoid.
fn storey_has_adjacent_room(plate: &Floorplate, column_x: f32, z0: f32, z1: f32) -> bool {
    for cell in &plate.rooms {
        if !matches!(cell.kind, CellKind::Room) {
            continue;
        }
        if (cell.rect.x_max - column_x).abs() > 1e-3 {
            continue;
        }
        if cell.rect.z_min < z1 - 1e-3 && cell.rect.z_max > z0 + 1e-3 {
            return true;
        }
    }
    false
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
    let thickness = 0.05;
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
    'outer: for sp in storeys {
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
            if adj_n && adj_e && adj_s {
                break 'outer;
            }
        }
    }
    (adj_n, adj_e, adj_s)
}

pub(super) fn synth_use(parent: &Node, name: &str, attrs: &[(&str, f32)]) -> Node {
    Node {
        kind: "use".to_string(),
        name: Some(name.to_string()),
        attrs: attrs
            .iter()
            .map(|(k, v)| (k.to_string(), Value::Number(*v)))
            .collect(),
        children: Vec::new(),
        span: parent.span,
        kind_span: parent.kind_span,
        use_id: None,
        origin: parent.origin.clone(),
    }
}

pub(super) fn storey_label(s: i32) -> String {
    if s >= 0 {
        s.to_string()
    } else {
        format!("b{}", -s)
    }
}

pub(super) fn inherit_material_from_chain(id: NodeId, graph: &mut SceneGraph) {
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

// Suppress unused-import warning: Rect2/Span are used by paths above for
// constructing module-instantiation specs but the imports route through
// helpers; keep the binding so future T3 work that touches Rect2 doesn't
// need to reintroduce it.
#[allow(dead_code)]
fn _types_reachable(_: Rect2, _: Span) {}
