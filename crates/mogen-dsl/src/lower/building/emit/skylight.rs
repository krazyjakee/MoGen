//! Skylight emission. Top-storey only — the ceiling slab there is the
//! roof for a flat-roof building, so cutting holes through it is the
//! same operation as cutting any other slab.
//!
//! Two-step flow:
//!
//! 1. `plan_skylights` is called from `emit/mod.rs` *before* `shell::emit_shell`
//!    so the slab can be carved with the skylight cutouts in one CSG pass.
//! 2. `emit_skylights` runs after the shell and stamps the
//!    `skylight_simple` module instances at the carved positions.

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::box_mesh;

use crate::ast::{Node, Value};
use crate::module::expand_modules;

use super::super::config::{BuildingCfg, Roof};
use super::super::layout::{CellKind, Floorplate, Rect2};
use super::super::materials::SKYLIGHT_GLASS_MAT;
use super::super::rng::{attempt_seed, rand_f01};
use super::StoreyCtx;

/// Plan the XY rectangles of every skylight on the top storey. Returns an
/// empty vec if `cfg.skylights == 0` or `!ctx.has_skylights()`.
///
/// Distributes skylights deterministically across room cells: pick rooms
/// in turn, place a centred square skylight sized to `min(window_w,
/// window_h)`. Skips circulation cells (those get hard ceilings).
pub(super) fn plan_skylights(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    ctx: StoreyCtx,
) -> Vec<Rect2> {
    if cfg.skylights == 0 || !ctx.has_skylights() {
        return Vec::new();
    }
    // Non-flat roofs replace the top ceiling slab with a roof mesh — the
    // skylight planner assumes a flat slab to carve, so skylights are
    // currently ignored under non-flat roofs. The validator emits W1114
    // ahead of time so the author sees the silent drop.
    if !matches!(cfg.roof, Roof::Flat) {
        return Vec::new();
    }
    let mut state = attempt_seed(cfg.seed, 1337u32);
    let size = cfg.window_w.min(cfg.window_h);
    // Skylight cells: rooms only (skip stairs/elevators — the column has
    // its own daylight strategy in later tranches).
    let room_cells: Vec<&super::super::layout::RoomCell> = plate
        .rooms
        .iter()
        .filter(|c| matches!(c.kind, CellKind::Room))
        .collect();
    if room_cells.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..(cfg.skylights as usize) {
        let cell = room_cells[i % room_cells.len()];
        let r = &cell.rect;
        // Pick a position that fits the skylight with a small margin from
        // any room wall — keeps the carved opening clear of the wall mesh
        // it would otherwise overlap.
        let usable_w = (r.width() - size - 0.4).max(0.0);
        let usable_d = (r.depth() - size - 0.4).max(0.0);
        let cx = r.x_min + 0.2 + 0.5 * size + usable_w * rand_f01(&mut state);
        let cz = r.z_min + 0.2 + 0.5 * size + usable_d * rand_f01(&mut state);
        out.push(Rect2 {
            x_min: cx - 0.5 * size,
            x_max: cx + 0.5 * size,
            z_min: cz - 0.5 * size,
            z_max: cz + 0.5 * size,
        });
    }
    out
}

pub(super) fn emit_skylights_at(
    node: &Node,
    cfg: &BuildingCfg,
    rects: &[Rect2],
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    if rects.is_empty() {
        return Ok(());
    }
    let origin = node.origin.clone();
    let h = cfg.ceiling_height;
    let ct = cfg.ceiling_thickness;
    // Skylights sit atop the ceiling slab — y = h (top of room space).
    let y_centre = h + 0.5 * ct;

    let reg = crate::lower::MODULE_REGISTRY
        .with(|s| s.borrow().clone())
        .unwrap_or_default();

    for (i, r) in rects.iter().enumerate() {
        let cx = 0.5 * (r.x_min + r.x_max);
        let cz = 0.5 * (r.z_min + r.z_max);
        let sky_id = graph.add_child(
            parent,
            format!("skylight_{i}"),
            "group",
            Transform::from_trs(
                Vec3::new(cx, y_centre, cz),
                Quat::IDENTITY,
                Vec3::ONE,
            ),
        );
        graph.nodes[sky_id.0 as usize].origin = origin.clone();
        graph.nodes[sky_id.0 as usize].role = Some("skylight".into());
        graph.nodes[sky_id.0 as usize]
            .tags
            .extend(["building".into(), "skylight".into()]);

        // Bind glass onto the wrapping group so the stdlib skylight pane
        // (which has no explicit `mat=`) inherits a transparent pane
        // instead of falling through to the wall material.
        if let Some(mid) =
            graph.find_material_scoped(SKYLIGHT_GLASS_MAT, origin.as_deref())
        {
            graph.set_material(sky_id, mid);
        }

        if reg.contains(&cfg.skylight_mod) {
            let synth = Node {
                kind: "use".into(),
                name: Some(cfg.skylight_mod.clone()),
                attrs: vec![
                    ("width".into(), Value::Number(r.width())),
                    ("height".into(), Value::Number(r.depth())),
                ],
                children: Vec::new(),
                span: node.span,
                kind_span: node.kind_span,
                use_id: None,
                origin: origin.clone(),
            };
            let (expanded, use_parents) = expand_modules(&[synth], &reg)?;
            for (k, v) in use_parents {
                graph.use_parents.insert(k, v);
            }
            for n in &expanded {
                crate::lower::node::lower_into(n, Some(sky_id), graph)?;
            }
        } else {
            // Fallback: a thin pane filling the skylight rect.
            let mesh = box_mesh([r.width(), 0.04, r.depth()], UvMode::Fit);
            let panel_id = graph.add_child(
                sky_id,
                "pane".to_string(),
                "panel",
                Transform::IDENTITY,
            );
            graph.set_mesh(panel_id, mesh);
            graph.nodes[panel_id.0 as usize].origin = origin.clone();
        }
    }
    Ok(())
}
