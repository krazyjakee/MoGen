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

use anyhow::Result;
use glam::{Mat4, Quat, Vec3, Vec4};

use mogen_core::{NodeId, SceneGraph, Span, Transform, UvMode};
use mogen_geom::{box_mesh, transform_mesh};

use crate::ast::{Node, Value};
use crate::module::expand_modules;

use super::super::circulation::{
    CirculationCell, CirculationKind, STAIR_ENTRY_DEPTH, STAIR_LANDING_DEPTH,
};
use super::super::config::BuildingCfg;
use super::super::layout::{BuildingLayout, CellKind, Floorplate, Rect2, StoreyPlate};
use super::modules::emit_interior_door_slot;
use super::openings::elevator_door_z;
use super::wall_build::wall_with_holes;

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
const STAIR_CENTRAL_SPINE: f32 = 0.05;

/// Slab thickness of the mid-landing and entry/exit platforms. Kept
/// thin so head clearance below remains close to `ceiling_height`.
const STAIR_PLATFORM_THICKNESS: f32 = 0.08;

/// Handrail dimensions. A single capped post on the open side of every
/// flight and around the cutout edge on every upper storey.
const RAILING_HEIGHT: f32 = 0.95;
const RAILING_THICKNESS: f32 = 0.04;

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

/// Direction the half-flight ascends within the cell's plan view. The
/// lower flight (east half) ascends south→north; the upper flight (west
/// half) ascends north→south so the user lands back at the south entry
/// zone after the 180° turn at the mid-landing.
#[derive(Clone, Copy, Debug)]
enum FlightDir {
    SouthToNorth,
    NorthToSouth,
}

fn emit_staircase(
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
    if top_storey == bottom_storey {
        // Single-storey building: a staircase has nowhere to go.
        return Ok(());
    }

    let origin = node.origin.clone();
    let centre = cell.rect.centre();
    let stair_group = graph.add_child(
        parent,
        format!("staircase_{idx}"),
        "group",
        Transform::from_trs(
            Vec3::new(centre[0], 0.0, centre[1]),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
    );
    graph.nodes[stair_group.0 as usize].origin = origin.clone();
    graph.nodes[stair_group.0 as usize].role = Some("staircase".into());
    graph.nodes[stair_group.0 as usize]
        .tags
        .extend(["building".into(), "staircase".into()]);

    let h = cfg.ceiling_height;
    let ct = cfg.ceiling_thickness;
    let step_h = h + ct;

    // Cell-local frame: x ∈ [-w/2, +w/2], z ∈ [-d/2, +d/2].
    let cell_w = cell.rect.width();
    let cell_d = cell.rect.depth();
    let spine = STAIR_CENTRAL_SPINE;
    let flight_z_min = -0.5 * cell_d + STAIR_ENTRY_DEPTH;
    let flight_z_max = 0.5 * cell_d - STAIR_LANDING_DEPTH;
    let flight_w = (0.5 * (cell_w - spine)).max(0.3);
    // East flight occupies +x half, west flight −x half. The half-flight
    // body sits at the centre of its half so the spine slot is symmetric
    // and the open (handrail) edge lines up with the cell's mid-x.
    let east_centre_x = 0.5 * spine + 0.5 * flight_w;
    let west_centre_x = -(0.5 * spine + 0.5 * flight_w);
    let half_rise = 0.5 * step_h;

    for s in bottom_storey..top_storey {
        let y_floor = s as f32 * step_h;
        let y_mid = y_floor + half_rise;

        // Lower half-flight: east half, ascending south→north from
        // floor s elevation to mid-height between s and s+1.
        emit_half_flight(
            node,
            s,
            "lower",
            east_centre_x,
            y_floor,
            flight_z_min,
            flight_z_max,
            flight_w,
            half_rise,
            FlightDir::SouthToNorth,
            stair_group,
            graph,
        )?;

        // Mid-landing slab: full width, north zone, at mid-height. Both
        // half-flights meet here so the user turns 180° on a flat slab.
        emit_mid_landing(node, s, cell_w, cell_d, y_mid, stair_group, graph);

        // Upper half-flight: west half, ascending north→south from
        // mid-height to floor s+1 elevation.
        emit_half_flight(
            node,
            s,
            "upper",
            west_centre_x,
            y_mid,
            flight_z_min,
            flight_z_max,
            flight_w,
            half_rise,
            FlightDir::NorthToSouth,
            stair_group,
            graph,
        )?;

        // Flight handrails sit on each flight's spine-facing edge. The
        // east flight's open edge is at +spine/2 (its west face), so the
        // rail centre sits at +(spine/2 + thickness/2) with its west
        // face flush against the spine. The west flight mirrors. This
        // puts each rail on the user's open hand side as they climb and
        // gives the mid-landing rail clean shared edges to butt against.
        emit_flight_handrail(
            node,
            s,
            0.5 * spine + 0.5 * RAILING_THICKNESS, // east flight's spine edge
            y_floor,
            flight_z_min,
            flight_z_max,
            half_rise,
            FlightDir::SouthToNorth,
            stair_group,
            graph,
        );
        emit_flight_handrail(
            node,
            s,
            -(0.5 * spine + 0.5 * RAILING_THICKNESS), // west flight's spine edge
            y_mid,
            flight_z_min,
            flight_z_max,
            half_rise,
            FlightDir::NorthToSouth,
            stair_group,
            graph,
        );

        // Mid-landing south-edge railing in the central gap between the
        // two flight access points. Spans the spine width so it never
        // crosses where the flights actually meet the landing.
        emit_landing_handrail(
            node,
            s,
            flight_z_max,
            y_mid,
            spine,
            stair_group,
            graph,
        );
    }

    // Cutout-edge handrail on the top storey only. The cutout's south
    // edge is the only fall hazard a user can walk up to from outside
    // the stairs. On the top storey the east half of that edge is a
    // one-storey drop straight onto the lower flight below (no flight
    // ascends from the top), so a railing belongs there; the west half
    // is the top step of the descending upper flight. On every
    // intermediate storey the east half is where the next pair's
    // lower flight begins — its first step sits at z = flight_z_min,
    // so a railing here would wall off the base of the next ascent.
    if top_storey > bottom_storey {
        let s = top_storey;
        let y_floor = s as f32 * step_h;
        emit_cutout_edge_railing(
            node,
            s,
            flight_z_min,
            cell_w,
            spine,
            y_floor,
            stair_group,
            graph,
        );
    }

    // Three-sided shaft enclosure (N/E/S). West face stays open so the
    // per-storey cell wall (with its door cutout from `place_interior_doors`)
    // is the boundary onto the adjacent room.
    let storeys_spanned = (top_storey - bottom_storey + 1).max(1) as f32;
    let total_h = storeys_spanned * step_h;
    let y_centre = bottom_storey as f32 * step_h + 0.5 * total_h - 0.5 * ct;
    let enclosure = graph.add_child(
        stair_group,
        "shaft_walls".to_string(),
        "group",
        Transform::from_trs(
            Vec3::new(0.0, y_centre, 0.0),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
    );
    graph.nodes[enclosure.0 as usize].origin = origin.clone();
    let (adj_n, adj_e, adj_s) = shaft_face_adjacencies(&cell.rect, storeys);
    emit_shaft_enclosure(
        cell_w, cell_d, total_h, adj_n, adj_e, adj_s, enclosure, graph, &origin,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_half_flight(
    node: &Node,
    storey: i32,
    label: &str,
    centre_x: f32,
    y_base: f32,
    flight_z_min: f32,
    flight_z_max: f32,
    flight_w: f32,
    rise: f32,
    dir: FlightDir,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let origin = node.origin.clone();
    let flight_d = flight_z_max - flight_z_min;
    let z_centre = 0.5 * (flight_z_min + flight_z_max);
    // The flight is authored along +Z (ascending south→north in its
    // local frame). For the upper flight the group is rotated 180° around
    // Y so the same authored geometry ascends from north to south.
    let rot = match dir {
        FlightDir::SouthToNorth => Quat::IDENTITY,
        FlightDir::NorthToSouth => Quat::from_rotation_y(std::f32::consts::PI),
    };

    let flight_group = graph.add_child(
        parent,
        format!("flight_{}_{}", storey_label(storey), label),
        "group",
        Transform::from_trs(Vec3::new(centre_x, y_base, z_centre), rot, Vec3::ONE),
    );
    graph.nodes[flight_group.0 as usize].origin = origin.clone();
    graph.nodes[flight_group.0 as usize].role = Some("stair_flight".into());
    graph.nodes[flight_group.0 as usize].tags.extend([
        "building".into(),
        "stair_flight".into(),
        label.into(),
    ]);

    // Target ~0.18 m rise per step. Half-flights have less rise than the
    // old full-storey flight, so the minimum step count drops to 4 — a
    // very low ceiling still gives a readable stair instead of being
    // padded up to 8.
    let target_rise = 0.18;
    let steps = ((rise / target_rise).round() as i32).max(4) as f32;

    let reg = crate::lower::MODULE_REGISTRY
        .with(|s| s.borrow().clone())
        .unwrap_or_default();
    if reg.contains("stair_simple") {
        let synth = synth_use(node, "stair_simple", &[
            ("width", flight_w),
            ("depth", flight_d),
            ("rise", rise),
            ("steps", steps),
        ]);
        let (expanded, use_parents) = expand_modules(&[synth], &reg)?;
        for (k, v) in use_parents {
            graph.use_parents.insert(k, v);
        }
        for n in &expanded {
            crate::lower::node::lower_into(n, Some(flight_group), graph)?;
        }
    } else {
        emit_synthetic_stair(flight_w, flight_d, rise, flight_group, graph, &origin);
    }
    Ok(())
}

fn emit_synthetic_stair(
    flight_w: f32,
    flight_d: f32,
    rise: f32,
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
) {
    let target_rise = 0.18;
    let steps = (rise / target_rise).round() as u32;
    let steps = steps.max(4);
    let tread = flight_d / steps as f32;
    let actual_rise = rise / steps as f32;
    for i in 0..steps {
        // Single-rise block at the step's true elevation — matches
        // stair_simple. The under-side traces a stepped diagonal so a
        // half-flight stacked directly above (same x-half on the next
        // storey) leaves clear headroom for the climber below instead
        // of capping it at half-rise.
        let step_centre_y = actual_rise * (i as f32 + 0.5);
        let step_centre_z = -0.5 * flight_d + (i as f32 + 0.5) * tread;
        let mesh = box_mesh([flight_w - 0.1, actual_rise, tread], UvMode::Tile);
        let step_id = graph.add_child(
            parent,
            format!("step_{i}"),
            "box",
            Transform::from_trs(
                Vec3::new(0.0, step_centre_y, step_centre_z),
                Quat::IDENTITY,
                Vec3::ONE,
            ),
        );
        graph.set_mesh(step_id, mesh);
        graph.nodes[step_id.0 as usize].origin = origin.clone();
        graph.nodes[step_id.0 as usize].role = Some("stair_step".into());
        inherit_material_from_chain(step_id, graph);
    }
}

/// Slab at half-storey height spanning the north landing zone. The
/// two half-flights meet at its south edge — the lower flight's top
/// step sits flush with the east end, the upper flight's bottom step
/// with the west end.
fn emit_mid_landing(
    node: &Node,
    storey: i32,
    cell_w: f32,
    cell_d: f32,
    y_mid: f32,
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    let origin = node.origin.clone();
    let z_south = 0.5 * cell_d - STAIR_LANDING_DEPTH;
    let z_north = 0.5 * cell_d;
    let z_centre = 0.5 * (z_south + z_north);
    let depth = z_north - z_south;
    let mesh = box_mesh(
        [cell_w, STAIR_PLATFORM_THICKNESS, depth],
        UvMode::Tile,
    );
    // Top surface at y_mid; centre sits half a thickness below.
    let y_centre = y_mid - 0.5 * STAIR_PLATFORM_THICKNESS;
    let id = graph.add_child(
        parent,
        format!("mid_landing_{}", storey_label(storey)),
        "box",
        Transform::from_trs(
            Vec3::new(0.0, y_centre, z_centre),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
    );
    graph.set_mesh(id, mesh);
    graph.nodes[id.0 as usize].origin = origin.clone();
    graph.nodes[id.0 as usize].role = Some("stair_landing".into());
    graph.nodes[id.0 as usize].tags.extend([
        "building".into(),
        "stair_landing".into(),
    ]);
    inherit_material_from_chain(id, graph);
}

/// Sloped solid-panel handrail along a flight's spine-facing edge.
///
/// Built as a sheared box — a parallelogram in YZ extruded by
/// `RAILING_THICKNESS` along X — rather than a rotated box. The
/// bottom and top edges slant with the flight's slope so the rail
/// tracks the treads, while the south and north end faces stay
/// vertical and align exactly with `flight_z_min` / `flight_z_max`.
/// A rotated box would push its top-south and bottom-north corners
/// past the flight footprint by ≈ RH/2·sin(α), leaving the rail
/// jutting into the entry zone and the landing instead of butting
/// flush against the mid-landing rail and the cutout-edge rail.
#[allow(clippy::too_many_arguments)]
fn emit_flight_handrail(
    node: &Node,
    storey: i32,
    x_pos: f32,
    y_base: f32,
    flight_z_min: f32,
    flight_z_max: f32,
    rise: f32,
    dir: FlightDir,
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    let origin = node.origin.clone();
    let flight_d = flight_z_max - flight_z_min;
    let z_centre = 0.5 * (flight_z_min + flight_z_max);
    // Slope of the panel's bottom/top edges, in world stair-local frame.
    // Positive = rises toward +Z (south→north). The upper flight
    // ascends in the opposite direction so its rail tracks the negative
    // slope of its own treads — y decreases as z grows.
    let slope = match dir {
        FlightDir::SouthToNorth => rise / flight_d,
        FlightDir::NorthToSouth => -rise / flight_d,
    };
    // Shear an axis-aligned box (thickness × RH × flight_d) by `slope`
    // along Y-by-Z. Each vertex's y is offset by `slope * z`, turning
    // the box into a parallelepiped whose ±z faces stay vertical at
    // ±flight_d/2 and whose ±y faces slant in lockstep.
    let base = box_mesh(
        [RAILING_THICKNESS, RAILING_HEIGHT, flight_d],
        UvMode::Tile,
    );
    let shear = Mat4::from_cols(
        Vec4::new(1.0, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 1.0, 0.0, 0.0),
        Vec4::new(0.0, slope, 1.0, 0.0),
        Vec4::new(0.0, 0.0, 0.0, 1.0),
    );
    let mesh = transform_mesh(&base, shear);
    // After shearing, the panel's bottom edge runs from
    // (z=-d/2, y=-h/2 - slope·d/2) to (z=+d/2, y=-h/2 + slope·d/2).
    // Translating by centre_y = y_base + rise/2 + RH/2 lands the
    // bottom-south corner at y_base and bottom-north at y_base + rise
    // for an ascending flight, and the mirror for a descending one.
    let centre_y = y_base + 0.5 * rise + 0.5 * RAILING_HEIGHT;
    let id = graph.add_child(
        parent,
        format!("flight_handrail_{}_{}", storey_label(storey), match dir {
            FlightDir::SouthToNorth => "lower",
            FlightDir::NorthToSouth => "upper",
        }),
        "box",
        Transform::from_trs(
            Vec3::new(x_pos, centre_y, z_centre),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
    );
    graph.set_mesh(id, mesh);
    graph.nodes[id.0 as usize].origin = origin.clone();
    graph.nodes[id.0 as usize].role = Some("handrail".into());
    graph.nodes[id.0 as usize].tags.extend([
        "building".into(),
        "handrail".into(),
    ]);
    inherit_material_from_chain(id, graph);
}

/// Mid-landing south-edge railing that fills the central spine gap. The
/// lower flight's top step covers the east portion of the landing's
/// south edge and the upper flight's bottom step covers the west
/// portion, so a railing across the spine fills the only segment where
/// someone could walk off the landing's south edge into the shaft.
fn emit_landing_handrail(
    node: &Node,
    storey: i32,
    flight_z_max: f32,
    y_mid: f32,
    spine: f32,
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    let origin = node.origin.clone();
    let length = spine.max(0.05);
    let size = [length, RAILING_HEIGHT, RAILING_THICKNESS];
    let mesh = box_mesh(size, UvMode::Tile);
    // Top surface of landing at y_mid; rail sits on top of it.
    let centre_y = y_mid + 0.5 * RAILING_HEIGHT;
    let id = graph.add_child(
        parent,
        format!("landing_handrail_{}", storey_label(storey)),
        "box",
        Transform::from_trs(
            Vec3::new(0.0, centre_y, flight_z_max + 0.5 * RAILING_THICKNESS),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
    );
    graph.set_mesh(id, mesh);
    graph.nodes[id.0 as usize].origin = origin.clone();
    graph.nodes[id.0 as usize].role = Some("handrail".into());
    graph.nodes[id.0 as usize].tags.extend([
        "building".into(),
        "handrail".into(),
    ]);
    inherit_material_from_chain(id, graph);
}

/// Railing along the south edge of the slab cutout on the top storey
/// only — the only storey where the east half of that edge is a true
/// drop (no flight ascends from the top). On intermediate storeys the
/// east half is the first step of the next pair's lower flight, so a
/// railing here would block the ascent. The west half of the edge is
/// always the top step of the descending upper flight (no railing
/// wanted).
fn emit_cutout_edge_railing(
    node: &Node,
    storey: i32,
    flight_z_min: f32,
    cell_w: f32,
    spine: f32,
    y_floor: f32,
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    let origin = node.origin.clone();
    // East-half segment, from the spine's east edge out to the cell's
    // east wall.
    let x_start = 0.5 * spine;
    let x_end = 0.5 * cell_w;
    let length = (x_end - x_start).max(0.05);
    let centre_x = 0.5 * (x_start + x_end);
    let size = [length, RAILING_HEIGHT, RAILING_THICKNESS];
    let mesh = box_mesh(size, UvMode::Tile);
    let centre_y = y_floor + 0.5 * RAILING_HEIGHT;
    let id = graph.add_child(
        parent,
        format!("cutout_handrail_{}", storey_label(storey)),
        "box",
        Transform::from_trs(
            Vec3::new(centre_x, centre_y, flight_z_min - 0.5 * RAILING_THICKNESS),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
    );
    graph.set_mesh(id, mesh);
    graph.nodes[id.0 as usize].origin = origin.clone();
    graph.nodes[id.0 as usize].role = Some("handrail".into());
    graph.nodes[id.0 as usize].tags.extend([
        "building".into(),
        "handrail".into(),
    ]);
    inherit_material_from_chain(id, graph);
}

#[allow(clippy::too_many_arguments)]
fn emit_elevator(
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
    Ok(())
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
    let thickness = 0.05;
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
fn emit_shaft_enclosure(
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
fn shaft_face_adjacencies(
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

// Suppress unused-import warning: Rect2/Span are used by paths above for
// constructing module-instantiation specs but the imports route through
// helpers; keep the binding so future T3 work that touches Rect2 doesn't
// need to reintroduce it.
#[allow(dead_code)]
fn _types_reachable(_: Rect2, _: Span) {}
