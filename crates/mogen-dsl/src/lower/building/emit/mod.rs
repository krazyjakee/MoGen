//! Emit pass — turns a `Floorplate` plus a `BuildingCfg` into actual
//! SceneGraph nodes. Splits responsibilities across submodules:
//!
//! - `openings`: compute the list of doors / entrances / windows.
//! - `skylight`: top-floor ceiling cutout planner + module stamps. Plans
//!   are computed first so `shell` can carve the same rects out of the
//!   roof slab.
//! - `shell`: floor + ceiling slabs (with circulation/skylight holes
//!   carved), perimeter walls with holes.
//! - `rooms`: per-cell groups (room/staircase/elevator), interior walls
//!   with door holes, cell-centre connectors.
//! - `modules`: synthesise + instantiate the user-supplied door / window
//!   modules at each opening position.
//! - `circulation`: multi-storey stair flights and elevator shaft walls.
//! - `roof`: T1 flat roof; stubs for non-flat shapes in later tranches.

pub(super) mod circulation;
mod openings;
mod shell;
mod rooms;
mod modules;
mod skylight;
mod roof;
mod wall_build;

use anyhow::Result;

use mogen_core::{NodeId, SceneGraph, Transform};

use crate::ast::Node;

use super::circulation::CirculationPlan;
use super::config::BuildingCfg;
use super::layout::Floorplate;

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // `bottom_storey`/`top_storey` mirror what the wrapper
                    // computes; kept on the ctx so per-storey emit code
                    // can query absolute bounds without re-deriving.
pub(super) struct StoreyCtx {
    pub storey: i32,
    pub is_bottom: bool,
    pub is_top: bool,
    pub bottom_storey: i32,
    pub top_storey: i32,
}

impl StoreyCtx {
    pub fn has_entrances(&self) -> bool {
        self.storey == 0
    }
    pub fn has_skylights(&self) -> bool {
        self.is_top
    }
}

pub(super) fn emit_floor(
    node: &Node,
    cfg: &BuildingCfg,
    circulation: &CirculationPlan,
    plate: &Floorplate,
    ctx: StoreyCtx,
    floor_group: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let origin = node.origin.clone();

    let plan = openings::plan_openings(cfg, plate, ctx);
    let skylight_rects = skylight::plan_skylights(cfg, plate, ctx);

    let shell_group = graph.add_child(
        floor_group,
        "shell".to_string(),
        "group",
        Transform::IDENTITY,
    );
    graph.nodes[shell_group.0 as usize].origin = origin.clone();
    shell::emit_shell(
        node,
        cfg,
        plate,
        &plan,
        circulation,
        &skylight_rects,
        ctx,
        shell_group,
        graph,
    )?;

    let rooms_group = graph.add_child(
        floor_group,
        "rooms".to_string(),
        "group",
        Transform::IDENTITY,
    );
    graph.nodes[rooms_group.0 as usize].origin = origin.clone();
    rooms::emit_rooms(node, cfg, plate, &plan, rooms_group, graph)?;

    let openings_group = graph.add_child(
        floor_group,
        "openings".to_string(),
        "group",
        Transform::IDENTITY,
    );
    graph.nodes[openings_group.0 as usize].origin = origin.clone();
    modules::emit_module_instances(node, cfg, &plan, openings_group, graph)?;

    if !skylight_rects.is_empty() {
        let sky_group = graph.add_child(
            floor_group,
            "skylights".to_string(),
            "group",
            Transform::IDENTITY,
        );
        graph.nodes[sky_group.0 as usize].origin = origin.clone();
        skylight::emit_skylights_at(node, cfg, &skylight_rects, sky_group, graph)?;
    }

    roof::emit_roof(node, cfg, plate, ctx, floor_group, graph)?;
    Ok(())
}
