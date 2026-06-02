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
//!
//! The emitters are split by structure: [`stairs`] (flights, treads,
//! mid-landings), [`handrails`] (sloped + flat railings), and [`elevator`]
//! (shaft enclosure walls + per-storey west pieces). The cross-cutting
//! constants, the [`FlightDir`] enum, and the small scene-graph helpers
//! used by every emitter live here so each submodule can reach them.

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Transform};

use crate::ast::{Node, Value};
use crate::lower::building::circulation::CirculationKind;
use crate::lower::building::config::BuildingCfg;
use crate::lower::building::emit::modules::emit_interior_door_slot;
use crate::lower::building::emit::wall_build::wall_with_holes;
use crate::lower::building::layout::{BuildingLayout, CellKind, Floorplate};

mod elevator;
mod handrails;
mod stairs;

use elevator::emit_elevator;
use stairs::emit_staircase;

/// Switchback layout — depths along the cell's Z axis, measured from
/// the cell's south edge (`z_min`). The cell is split into three zones:
///
/// ```text
///        z_max ┌────────────────────┐      (north)
///              │   mid-landing      │  ── `STAIR_LANDING_DEPTH`
///              │   (half-height,    │
///              │    full width)     │
///              ├────────────────────┤
///              │  upper │  lower    │
///              │  flight│  flight   │  ── flight zone (the rest)
///              │  N→S   │  S→N      │
///              │  asc   │  asc      │
///              ├────────────────────┤
///              │  entry / exit      │
///              │  platform —        │  ── `STAIR_ENTRY_DEPTH`
///              │  intact slab on    │
///              │  every storey      │
///        z_min └────────────────────┘      (south)
///              x_min   x_mid   x_max
///              (west)         (east)
/// ```
///
/// The entry zone preserves the floor slab on every storey — it's the
/// landing the door from the adjacent room opens onto and the platform
/// the user steps off onto after climbing. Shell.rs reads these to
/// build the matching slab cutout (everything north of the entry zone);
/// the planner side (`super::super::circulation`) owns the constants so
/// `layout` can derive `RoomCell::door_slots` from the same source.
///
/// Width of the central spine between the two parallel half-flights.
/// Just enough to keep the flights from sharing a tread vertex and to
/// leave a visible slot for the inner handrail to live in.
pub(super) const STAIR_CENTRAL_SPINE: f32 = 0.05;

/// Slab thickness of the mid-landing and entry/exit platforms. Kept
/// thin so head clearance below remains close to `ceiling_height`.
pub(super) const STAIR_PLATFORM_THICKNESS: f32 = 0.08;

/// Handrail dimensions. A single capped post on the open side of every
/// flight and around the cutout edge on every upper storey.
pub(super) const RAILING_HEIGHT: f32 = 0.95;
pub(super) const RAILING_THICKNESS: f32 = 0.04;

/// Direction the half-flight ascends within the cell's plan view. The
/// lower flight (east half) ascends south→north; the upper flight (west
/// half) ascends north→south so the user lands back at the south entry
/// zone after the 180° turn at the mid-landing.
#[derive(Clone, Copy, Debug)]
pub(super) enum FlightDir {
    SouthToNorth,
    NorthToSouth,
}

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
                emit_staircase(
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
                emit_elevator(
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

fn synth_use(parent: &Node, name: &str, attrs: &[(&str, f32)]) -> Node {
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

fn storey_label(s: i32) -> String {
    if s >= 0 {
        s.to_string()
    } else {
        format!("b{}", -s)
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

