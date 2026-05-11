//! Multi-storey circulation emission. Spans the entire building (not a
//! single storey), so it lives in its own subtree under the wrapper and
//! emits with absolute Y coordinates.
//!
//! T2 model:
//!
//! - **Staircase**: one straight flight between each pair of consecutive
//!   storeys, occupying the staircase's reserved XY cell. Each flight is
//!   a series of `box` tread meshes climbing from y = s*(h+ct) to
//!   y = (s+1)*(h+ct). The next flight (s+1 → s+2) sits in the same XY
//!   but at the higher Y range — i.e., a stack of straight flights, the
//!   simplest scheme that satisfies "user can walk between storeys".
//! - **Elevator**: a vertical shaft spanning the full Y range with one
//!   `use "<elevator_shaft_simple>"` instance at the ground floor's
//!   level. Future tranches can add per-storey cab stamps.
//!
//! No CSG carving here — the slab cutouts are handled in `shell.rs`'s
//! per-storey emission. This module only authors the stair / shaft
//! geometry that fills the carved column.

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Span, Transform, UvMode};
use mogen_geom::box_mesh;

use crate::ast::{Node, Value};
use crate::module::expand_modules;

use super::super::circulation::{CirculationCell, CirculationKind};
use super::super::config::BuildingCfg;
use super::super::layout::{BuildingLayout, Rect2};

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

    let flight_group = graph.add_child(
        parent,
        format!("flight_{}", storey_label(storey)),
        "group",
        Transform::from_trs(
            Vec3::new(0.0, y_start, 0.0),
            Quat::IDENTITY,
            Vec3::ONE,
        ),
    );
    graph.nodes[flight_group.0 as usize].origin = origin.clone();
    graph.nodes[flight_group.0 as usize].role = Some("stair_flight".into());
    graph.nodes[flight_group.0 as usize]
        .tags
        .extend(["building".into(), "stair_flight".into()]);

    let flight_w = cell.rect.width();
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
        emit_synthetic_stair(cell, rise, flight_group, graph, &origin);
    }
    Ok(())
}

fn emit_synthetic_stair(
    cell: &CirculationCell,
    rise: f32,
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
) {
    // Straight stair along +Z (depth axis), starting at z = -flight_d/2.
    let flight_w = cell.rect.width();
    let flight_d = cell.rect.depth();
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

    let reg = crate::lower::MODULE_REGISTRY
        .with(|s| s.borrow().clone())
        .unwrap_or_default();
    if reg.contains("elevator_shaft_simple") {
        let synth = synth_use(node, "elevator_shaft_simple", &[
            ("width", cell.rect.width()),
            ("depth", cell.rect.depth()),
            ("height", total_h),
        ]);
        let (expanded, use_parents) = expand_modules(&[synth], &reg)?;
        for (k, v) in use_parents {
            graph.use_parents.insert(k, v);
        }
        for n in &expanded {
            crate::lower::node::lower_into(n, Some(shaft_group), graph)?;
        }
    } else {
        // Fallback: a thin frame outline so the shaft is visible.
        let outline = box_mesh(
            [cell.rect.width(), total_h, cell.rect.depth()],
            UvMode::Tile,
        );
        let id = graph.add_child(
            shaft_group,
            "shaft_volume",
            "box",
            Transform::IDENTITY,
        );
        graph.set_mesh(id, outline);
        graph.nodes[id.0 as usize].origin = origin.clone();
        inherit_material_from_chain(id, graph);
    }
    Ok(())
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
