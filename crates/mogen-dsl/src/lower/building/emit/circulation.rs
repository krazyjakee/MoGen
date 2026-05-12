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
use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Span, Transform, UvMode};
use mogen_geom::box_mesh;

use crate::ast::{Node, Value};
use crate::module::expand_modules;

use super::super::circulation::{CirculationCell, CirculationKind};
use super::super::config::BuildingCfg;
use super::super::layout::{BuildingLayout, Rect2};

/// Fraction of a staircase cell's width occupied by the stair body. The
/// remaining width is reserved as a landing on every storey above the
/// bottom — so floor N+1 has somewhere to stand when you reach the top
/// of a flight. Shell.rs reads this to compute the matching slab cutout.
pub(in super::super) const STAIR_HALF_FRACTION: f32 = 0.5;

/// Depth of the landing strip preserved at the south end of every
/// staircase cell on floor N+1. The user steps west off the topmost
/// stair onto the west-half landing, walks south, then crosses this
/// strip at floor level to reach the bottom of the next flight on the
/// east half. Sized so a person can stand and turn (≥ one tread).
pub(in super::super) const STAIR_TRANSIT_STRIP_DEPTH: f32 = 1.0;

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
                    circ_group,
                    graph,
                )?;
            }
            CirculationKind::Elevator => {
                emit_elevator(node, cfg, cell, i, bottom_storey, top_storey, circ_group, graph)?;
            }
        }
    }
    Ok(())
}

fn emit_staircase(
    node: &Node,
    cfg: &BuildingCfg,
    cell: &CirculationCell,
    idx: usize,
    bottom_storey: i32,
    top_storey: i32,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    if top_storey == bottom_storey {
        // Single-storey building: a staircase has nowhere to go. Skip
        // emission rather than producing a flight with zero rise.
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
    let step = h + ct;

    // Stair runs from min storey up to max — one flight per storey-pair.
    for s in bottom_storey..top_storey {
        let y_start = s as f32 * step;
        emit_stair_flight(node, cfg, cell, s, y_start, h + ct, stair_group, graph)?;
    }

    // Stair cell enclosure: same N/E/S three-sided wall set the elevator
    // uses, so the staircase has a real wall on the elevator side and
    // doesn't bleed open into the (otherwise unwalled) inset gap or the
    // strip behind the south perimeter wall. The west wall is left open
    // for the door from the adjacent room. The stair_group is at y=0,
    // so the walls go into a child group offset to the shaft's mid-Y.
    let storeys_spanned = (top_storey - bottom_storey + 1).max(1) as f32;
    let total_h = storeys_spanned * step;
    let y_centre =
        bottom_storey as f32 * step + 0.5 * total_h - 0.5 * cfg.ceiling_thickness;
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
    emit_shaft_enclosure(
        cell.rect.width(),
        cell.rect.depth(),
        total_h,
        enclosure,
        graph,
        &origin,
    );
    Ok(())
}

fn emit_stair_flight(
    node: &Node,
    cfg: &BuildingCfg,
    cell: &CirculationCell,
    storey: i32,
    y_start: f32,
    rise: f32,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let _ = cfg; // T2 takes geometry directly from the cell+rise; cfg is
                 // retained on the signature so T3's variable-tread
                 // configuration can read it without a churning diff.
    let origin = node.origin.clone();
    // Try to instantiate the stair_simple module sized to this flight; fall
    // back to a synthetic straight flight if the module is missing.
    let reg = crate::lower::MODULE_REGISTRY
        .with(|s| s.borrow().clone())
        .unwrap_or_default();

    // Stair body occupies the east half of the cell; the west half is
    // reserved as a landing on every upper floor (slab preserved there).
    // `STAIR_HALF_FRACTION` keeps both halves the same width so the door
    // planner and slab cutout can locate the divide by the cell's centre.
    let flight_w = cell.rect.width() * STAIR_HALF_FRACTION;
    let east_offset = cell.rect.width() * (0.5 - 0.5 * STAIR_HALF_FRACTION);

    let flight_group = graph.add_child(
        parent,
        format!("flight_{}", storey_label(storey)),
        "group",
        Transform::from_trs(
            Vec3::new(east_offset, y_start, 0.0),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
    );
    graph.nodes[flight_group.0 as usize].origin = origin.clone();
    graph.nodes[flight_group.0 as usize].role = Some("stair_flight".into());
    graph.nodes[flight_group.0 as usize]
        .tags
        .extend(["building".into(), "stair_flight".into()]);

    let flight_d = cell.rect.depth();
    // Target ~18 cm per tread; clamp so a low rise still gets enough steps
    // for a sane stair (8 minimum keeps the visual readable).
    let target_rise = 0.18;
    let steps = ((rise / target_rise).round() as i32).max(8) as f32;
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
    let target_rise = 0.18; // ~18 cm per step is comfortable.
    let steps = (rise / target_rise).round() as u32;
    let steps = steps.max(8);
    let tread = flight_d / steps as f32;
    let actual_rise = rise / steps as f32;
    for i in 0..steps {
        let step_height = actual_rise * (i as f32 + 1.0);
        let step_centre_y = 0.5 * step_height;
        let step_centre_z = -0.5 * flight_d + (i as f32 + 0.5) * tread;
        let mesh = box_mesh([flight_w - 0.1, step_height, tread - 0.01], UvMode::Tile);
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

fn emit_elevator(
    node: &Node,
    cfg: &BuildingCfg,
    cell: &CirculationCell,
    idx: usize,
    bottom_storey: i32,
    top_storey: i32,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let origin = node.origin.clone();
    let centre = cell.rect.centre();
    let storeys_spanned = (top_storey - bottom_storey + 1).max(1) as f32;
    let total_h = storeys_spanned * (cfg.ceiling_height + cfg.ceiling_thickness);
    let y_centre = bottom_storey as f32 * (cfg.ceiling_height + cfg.ceiling_thickness)
        + 0.5 * total_h
        - 0.5 * cfg.ceiling_thickness;

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

    // Three-sided shaft enclosure (N/E/S). The west side is intentionally
    // left open — the per-storey cell-shared wall on that face (emitted
    // by `rooms.rs`) already carries the door cutout placed by the door
    // BFS. Adding a continuous shaft wall there would sit just behind
    // the cell wall and visually plug the door from inside the shaft.
    emit_shaft_enclosure(
        cell.rect.width(),
        cell.rect.depth(),
        total_h,
        shaft_group,
        graph,
        &origin,
    );
    Ok(())
}

/// Emit N/E/S walls forming a three-sided enclosure for a circulation
/// cell, parented to `group` (which the caller has already centred on
/// the cell in XZ and at the cell's mid-Y). Skips the west wall — the
/// per-storey cell walls on that face carry the door cutout to the
/// adjacent room, so this enclosure stays open on the door side.
fn emit_shaft_enclosure(
    cell_w: f32,
    cell_d: f32,
    total_h: f32,
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
    let walls: [(&str, [f32; 3], [f32; 3]); 3] = [
        (
            "shaft_wall_n",
            [0.0, 0.0, 0.5 * cell_d + half],
            [cell_w + 0.1, total_h, thickness],
        ),
        (
            "shaft_wall_e",
            [0.5 * cell_w + half, 0.0, 0.0],
            [thickness, total_h, cell_d + 0.1],
        ),
        (
            "shaft_wall_s",
            [0.0, 0.0, -0.5 * cell_d - half],
            [cell_w + 0.1, total_h, thickness],
        ),
    ];
    for (name, pos, size) in walls {
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
