//! Emit pass — turns a `Floorplate` plus a `BuildingCfg` into actual
//! SceneGraph nodes. Splits responsibilities across submodules:
//!
//! - `openings`: compute the list of doors / entrances / windows / skylights.
//! - `shell`: floor + ceiling slabs, perimeter walls with holes.
//! - `rooms`: per-room groups, interior walls with door holes, room marker
//!   connectors at room centres.
//! - `modules`: synthesise + instantiate the user-supplied door / window /
//!   skylight modules at each opening position.
//! - `roof`: T1 flat roof (slab); stubs for later tranches.

mod openings;
mod shell;
mod rooms;
mod modules;
mod roof;

use anyhow::Result;

use mogen_core::{NodeId, SceneGraph, Transform};

use crate::ast::Node;

use super::config::BuildingCfg;
use super::layout::Floorplate;

pub(super) fn emit_floor(
    node: &Node,
    cfg: &BuildingCfg,
    plate: &Floorplate,
    floor_group: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let origin = node.origin.clone();

    // Compute the opening list first — both the shell and the interior walls
    // need to know where holes go before they author wall geometry.
    let plan = openings::plan_openings(cfg, plate);

    let shell_group = graph.add_child(
        floor_group,
        "shell".to_string(),
        "group",
        Transform::IDENTITY,
    );
    graph.nodes[shell_group.0 as usize].origin = origin.clone();
    shell::emit_shell(node, cfg, plate, &plan, shell_group, graph)?;

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

    roof::emit_roof(node, cfg, plate, floor_group, graph)?;
    Ok(())
}
