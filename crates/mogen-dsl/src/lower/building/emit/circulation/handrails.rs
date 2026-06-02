//! Handrails for the switchback stair: the sloped solid-panel rail along
//! each flight's open (spine-facing) edge, the flat rail filling the
//! mid-landing's south-edge spine gap, and the cutout-edge rail on the top
//! storey. All three are driven by [`super::stairs::emit_staircase`].

use glam::{Mat4, Quat, Vec3, Vec4};

use mogen_core::{NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{box_mesh, transform_mesh};

use crate::ast::Node;

use super::{
    inherit_material_from_chain, storey_label, FlightDir, RAILING_HEIGHT, RAILING_THICKNESS,
};

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
pub(super) fn emit_flight_handrail(
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
pub(super) fn emit_landing_handrail(
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
pub(super) fn emit_cutout_edge_railing(
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
