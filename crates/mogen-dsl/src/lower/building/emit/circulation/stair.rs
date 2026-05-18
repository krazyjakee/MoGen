//! Staircase and handrail emission for multi-storey switchback stairs.

use anyhow::Result;
use glam::{Mat4, Quat, Vec3, Vec4};

use mogen_core::{NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{box_mesh, transform_mesh};

use crate::ast::Node;
use crate::lower::building::circulation::{CirculationCell, STAIR_ENTRY_DEPTH, STAIR_LANDING_DEPTH};
use crate::lower::building::config::BuildingCfg;
use crate::lower::building::layout::StoreyPlate;

use super::{
    emit_shaft_enclosure, inherit_material_from_chain, shaft_face_adjacencies, storey_label,
    synth_use,
};

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
/// Width of the central spine between the two parallel half-flights.
/// Just enough to keep the flights from sharing a tread vertex and to
/// leave a visible slot for the inner handrail to live in.
const STAIR_CENTRAL_SPINE: f32 = 0.05;

/// Slab thickness of the mid-landing and entry/exit platforms. Kept
/// thin so head clearance below remains close to `ceiling_height`.
const STAIR_PLATFORM_THICKNESS: f32 = 0.08;

const RAILING_HEIGHT: f32 = 0.95;
const RAILING_THICKNESS: f32 = 0.04;

/// Direction the half-flight ascends within the cell's plan view. The
/// lower flight (east half) ascends south→north; the upper flight (west
/// half) ascends north→south so the user lands back at the south entry
/// zone after the 180° turn at the mid-landing.
#[derive(Clone, Copy, Debug)]
enum FlightDir {
    SouthToNorth,
    NorthToSouth,
}

pub(super) fn emit_staircase(
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

    // Shaft enclosure: skip any face where an adjacent Room cell already
    // provides closure (its per-cell wall carries the door cutout and a
    // redundant shaft_wall would shadow it). The west face is always
    // omitted — the per-storey cell wall from `rooms.rs` handles it.
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
        let (expanded, use_parents) = crate::module::expand_modules(&[synth], &reg)?;
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
