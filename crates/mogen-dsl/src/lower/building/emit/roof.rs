//! Roof emission. Tranche 1 only supports `flat`: the ceiling slab emitted
//! by `shell.rs` already covers the top floor, so this is a no-op.
//!
//! Future tranches add pitched / gabled / hipped / mansard / shed roof
//! geometry generated from the floorplate footprint. The dispatch landing
//! lives here so the orchestrator doesn't change shape when those land.

use anyhow::Result;

use mogen_core::{NodeId, SceneGraph};

use crate::ast::Node;

use super::super::config::{BuildingCfg, Roof};
use super::super::layout::Floorplate;

pub(super) fn emit_roof(
    _node: &Node,
    cfg: &BuildingCfg,
    _plate: &Floorplate,
    _floor_group: NodeId,
    _graph: &mut SceneGraph,
) -> Result<()> {
    match cfg.roof {
        Roof::Flat => {
            // No-op: the ceiling slab emitted by shell.rs IS the roof in
            // single-storey, flat-roof T1 buildings.
        }
    }
    Ok(())
}
