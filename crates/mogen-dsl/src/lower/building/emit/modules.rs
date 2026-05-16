//! Module instantiation: stamp the user-supplied door / window / skylight
//! modules at each opening position.
//!
//! We synthesise a `use "<module>"(width=…, height=…)` AST node per
//! opening, run it through `expand_modules` against the registry published
//! by `lower_with_loader`, then lower each expanded node as a child of an
//! `openings` group. The wrapper synthesised here carries `pos=` / `rot=`
//! so the door / window lands correctly in the floor's local frame.
//!
//! If the referenced module is missing from the registry, we fall back to
//! a simple synthetic box panel so the build still produces visible
//! geometry — the validator already flagged the missing module name with a
//! span-tagged diagnostic when the AST was checked.

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{NodeId, SceneGraph, Slot, Transform, UvMode};
use mogen_geom::box_mesh;

use crate::ast::{Node, Value};
use crate::module::{expand_modules, ModuleRegistry};

use super::super::config::BuildingCfg;
use super::super::materials::{EXT_DOOR_MAT, INT_DOOR_MAT, SKYLIGHT_GLASS_MAT, WINDOW_GLASS_MAT};
use super::openings::{Opening, OpeningKind, OpeningPlan, WindowClass};

pub(super) fn emit_module_instances(
    parent_node: &Node,
    cfg: &BuildingCfg,
    plan: &OpeningPlan,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    // Snapshot the registry once per emit pass. Cheap clone (Arc-y vec of
    // ModuleDefs); cost only matters for huge stdlibs which we don't have.
    let reg = crate::lower::MODULE_REGISTRY
        .with(|s| s.borrow().clone())
        .unwrap_or_default();

    for op in &plan.entrances {
        emit_one(
            parent_node, parent, graph, &reg, op,
            &cfg.external_door, "ext_door", "door", Some(EXT_DOOR_MAT),
        )?;
    }
    for op in &plan.interior_doors {
        emit_one(
            parent_node, parent, graph, &reg, op,
            &cfg.internal_door, "int_door", "door", Some(INT_DOOR_MAT),
        )?;
    }
    for op in &plan.windows {
        let module_name = match op.kind {
            OpeningKind::Window(WindowClass::Small) => &cfg.windows_mod.small,
            OpeningKind::Window(WindowClass::Medium) => &cfg.windows_mod.medium,
            OpeningKind::Window(WindowClass::Large) => &cfg.windows_mod.large,
            _ => continue,
        };
        emit_one(
            parent_node, parent, graph, &reg, op,
            module_name, "window", "window", Some(WINDOW_GLASS_MAT),
        )?;
    }
    for op in &plan.skylights {
        emit_one(
            parent_node, parent, graph, &reg, op,
            &cfg.skylight_mod, "skylight", "skylight", Some(SKYLIGHT_GLASS_MAT),
        )?;
    }
    Ok(())
}

fn emit_one(
    parent_node: &Node,
    parent: NodeId,
    graph: &mut SceneGraph,
    reg: &ModuleRegistry,
    op: &Opening,
    module_name: &str,
    label: &str,
    slot_kind: &str,
    inherit_mat: Option<&str>,
) -> Result<()> {
    // The opening group carries the pose; the instantiated module's body is
    // authored at the origin facing +Z. We rotate the group so the module's
    // +Z aligns with the opening's outward normal (`op.facing`).
    let pos = Vec3::new(op.x, op.sill, op.z);
    let rot = quat_facing(op.facing);
    let group_id = graph.add_child(
        parent,
        format!("{label}_{}_{}", encode(op.x), encode(op.z)),
        "group",
        Transform::from_trs(pos, rot, Vec3::ONE),
    );
    graph.nodes[group_id.0 as usize].origin = parent_node.origin.clone();
    graph.nodes[group_id.0 as usize].role = Some(label.into());
    graph.nodes[group_id.0 as usize]
        .tags
        .extend(["building".into(), label.into()]);
    // Game-engine importers (Godot etc.) read this slot block out of
    // `extras.slot` to find every doorway / window and substitute their own
    // prefab at the wrapper's transform. The wrapper TRS already encodes
    // position + outward-facing rotation; width / height live here because
    // the transform alone can't carry size.
    graph.nodes[group_id.0 as usize].slot = Some(Slot {
        kind: slot_kind.into(),
        width: op.width,
        height: op.height,
        depth: 0.0,
    });

    // Bind the per-opening-kind material onto the wrapping group so any
    // child mesh without an explicit `mat=` (the stdlib door slab, the
    // window/skylight pane) picks up `ext_door` / `int_door` /
    // `window_glass` via ancestor inheritance instead of the wall plaster.
    if let Some(mat_name) = inherit_mat {
        if let Some(mid) = graph.find_material_scoped(
            mat_name,
            parent_node.origin.as_deref(),
        ) {
            graph.set_material(group_id, mid);
        }
    }

    if reg.contains(module_name) {
        // Synthesise `use "<module>" (width=op.width, height=op.height)`
        // and expand it through the registry. The result is a list of
        // expanded AST nodes; lower each one as a child of the group.
        let use_node = synth_use_node(module_name, op.width, op.height, parent_node);
        let synth_ast = vec![use_node];
        let (expanded, use_parents) = expand_modules(&synth_ast, reg)?;
        // Merge the new use_parents into the graph's table so attach /
        // anim resolution can walk through the building-spawned frames.
        for (k, v) in use_parents {
            graph.use_parents.insert(k, v);
        }
        for n in &expanded {
            crate::lower::node::lower_into(n, Some(group_id), graph)?;
        }
    } else {
        // Fallback: a thin box panel sized to the opening so the
        // visualisation still reads. The validator has flagged the
        // missing module separately.
        let mesh = box_mesh([op.width, op.height, 0.04], UvMode::Fit);
        let panel_id = graph.add_child(
            group_id,
            format!("{label}_panel"),
            "panel",
            Transform::from_trs(
                Vec3::new(0.0, 0.5 * op.height, 0.0),
                Quat::IDENTITY,
                Vec3::ONE,
            ),
        );
        graph.set_mesh(panel_id, mesh);
        graph.nodes[panel_id.0 as usize].origin = parent_node.origin.clone();
        inherit_material_from_chain(panel_id, graph);
    }
    Ok(())
}

/// Synthesise a `use "<name>" (width=<w>, height=<h>)` AST node so the
/// module-expansion pipeline can resolve it as if the author wrote it.
/// Carries the building wrapper's `origin` so MoGen Studio's per-import
/// sidebar groups these instances under the active source.
fn synth_use_node(name: &str, w: f32, h: f32, parent_node: &Node) -> Node {
    Node {
        kind: "use".to_string(),
        name: Some(name.to_string()),
        attrs: vec![
            ("width".to_string(), Value::Number(w)),
            ("height".to_string(), Value::Number(h)),
        ],
        children: Vec::new(),
        span: parent_node.span,
        kind_span: parent_node.kind_span,
        use_id: None,
        origin: parent_node.origin.clone(),
    }
}

fn quat_facing(facing: [f32; 3]) -> Quat {
    let f = Vec3::from_array(facing);
    let n = f.length();
    let f = if n < 1e-3 { Vec3::Z } else { f / n };
    // Build a Y-up rotation that maps local +Z onto `f`.
    Quat::from_rotation_arc(Vec3::Z, f)
}

/// Stable opening-id suffix derived from `x`/`z`. We mint deterministic
/// names so test snapshots survive across rebuilds.
fn encode(v: f32) -> String {
    // Round to 1mm and stamp as a signed integer.
    let i = (v * 1000.0).round() as i32;
    format!("{i}")
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

