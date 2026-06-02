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

use std::path::Path;

use mogen_core::{Material, NodeId, SceneGraph, Slot, Transform, UvMode};
use mogen_geom::{box_mesh, icosphere_mesh};

use crate::ast::{Node, Value};
use crate::module::{expand_modules, ModuleRegistry};

use super::super::config::BuildingCfg;
use super::super::materials::{EXT_DOOR_MAT, INT_DOOR_MAT, SKYLIGHT_GLASS_MAT, WINDOW_GLASS_MAT};
use super::openings::{Opening, OpeningKind, OpeningPlan, WindowClass};

/// Spawn a single interior-door slot at an explicit world pose. Used by the
/// column-filler walls in `emit/circulation.rs`, which cut their own door
/// holes outside the per-storey `OpeningPlan` and would otherwise leave a
/// hole with no door panel and no slot metadata. The wrapper sits at
/// `(x, sill, z)` in the parent's frame and faces along `facing`; everything
/// else reuses the same plumbing as the openings emitter so the resulting
/// node carries `role`, `tags`, the `Slot`, and the inherited interior-door
/// material.
pub(super) fn emit_interior_door_slot(
    parent_node: &Node,
    cfg: &BuildingCfg,
    parent: NodeId,
    graph: &mut SceneGraph,
    x: f32,
    z: f32,
    sill: f32,
    facing: [f32; 3],
) -> Result<()> {
    let reg = crate::lower::MODULE_REGISTRY
        .with(|s| s.borrow().clone())
        .unwrap_or_default();
    let op = Opening {
        kind: OpeningKind::InteriorDoor,
        x,
        z,
        sill,
        width: cfg.door_w,
        height: cfg.door_h,
        side: None,
        facing,
    };
    emit_one(
        parent_node, parent, graph, &reg, &op,
        &cfg.internal_door, "int_door", "door", Some(INT_DOOR_MAT),
    )?;
    // The circulation-filler door is cut outside the per-storey `OpeningPlan`,
    // so `emit_opening_pois` never sees it. Drop a matching `door` POI here so
    // every doorway in the building — plan-driven or circulation-driven —
    // carries the same marker for custom-door tooling. Name keys off the world
    // pose to stay distinct from the plan-indexed `door_<n>` markers.
    emit_opening_poi(
        graph,
        parent,
        parent_node.origin.as_deref(),
        cfg,
        "door",
        format!("door_{}_{}", encode(x), encode(z)),
        Vec3::new(x, sill, z),
        facing,
    );
    Ok(())
}

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

/// Emit transform-only POI markers (`kind="poi"`) at every door and window
/// so a downstream tool can drop in its own custom door/window prefabs without
/// parsing the generated panel geometry. Mirrors the furniture / cave POI
/// contract: each marker carries `role` + `tags` and the opening's outward
/// pose (position at the threshold/sill, local +Z aligned with the wall's
/// outward normal — identical to the module wrapper in `emit_one`, so a prefab
/// authored facing +Z at its base lands flush). Markers are always emitted (so
/// the role/tags round-trip into `node.extras` for importers); they only gain
/// a small bright debug sphere when `debug_show_poi` is on.
pub(super) fn emit_opening_pois(
    parent_node: &Node,
    cfg: &BuildingCfg,
    plan: &OpeningPlan,
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    // Exterior entrances and interior doors are both "door" POIs (entrances
    // get an extra `entrance` tag); windows are "window" POIs. Skylights keep
    // their existing slot wrapper and are intentionally left out here — the
    // request is doors and windows.
    let groups: [(&[Opening], &str); 3] = [
        (plan.entrances.as_slice(), "entrance"),
        (plan.interior_doors.as_slice(), "door"),
        (plan.windows.as_slice(), "window"),
    ];
    if groups.iter().all(|(ops, _)| ops.is_empty()) {
        return;
    }

    let origin = parent_node.origin.clone();
    let group = graph.add_child(
        parent,
        "opening_pois".to_string(),
        "group",
        Transform::IDENTITY,
    );
    graph.nodes[group.0 as usize].origin = origin.clone();
    graph.nodes[group.0 as usize]
        .tags
        .extend(["building".to_string(), "points_of_interest".to_string()]);

    // Stable per-role suffixes (door_0, window_0, …) so two markers of the
    // same role get distinct, snapshot-stable names.
    let mut counts: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    for (ops, role) in groups {
        for op in ops {
            let idx = counts.entry(role).or_default();
            emit_opening_poi(
                graph,
                group,
                origin.as_deref(),
                cfg,
                role,
                format!("{role}_{idx}"),
                Vec3::new(op.x, op.sill, op.z),
                op.facing,
            );
            *idx += 1;
        }
    }
}

/// Spawn one transform-only POI marker (`kind="poi"`) at `pos` facing
/// `facing`, with `role`/`tags` set per the door/window contract. Shared by
/// the plan-driven `emit_opening_pois` and the circulation-filler door slot in
/// `emit_interior_door_slot` so every opening — wherever it's cut — gets the
/// same marker. The marker stays geometry-free unless `debug_show_poi` is set,
/// in which case it gains a small role-coloured emissive sphere.
fn emit_opening_poi(
    graph: &mut SceneGraph,
    parent: NodeId,
    origin: Option<&Path>,
    cfg: &BuildingCfg,
    role: &str,
    name: String,
    pos: Vec3,
    facing: [f32; 3],
) -> NodeId {
    let rot = quat_facing(facing);
    let id = graph.add_child(parent, name, "poi", Transform::from_trs(pos, rot, Vec3::ONE));
    graph.nodes[id.0 as usize].origin = origin.map(|p| p.to_path_buf());
    graph.nodes[id.0 as usize].role = Some(role.to_string());
    let mut tags = vec!["building".to_string(), "poi".to_string()];
    if role == "entrance" {
        tags.push("door".to_string());
        tags.push("entrance".to_string());
    } else {
        tags.push(role.to_string());
    }
    graph.nodes[id.0 as usize].tags.extend(tags);
    // Debug viz: a small emissive sphere per marker, colour-coded per role, so
    // the otherwise-empty POIs are visible in a glTF preview.
    if cfg.debug_show_poi {
        ensure_opening_poi_mat(graph, origin, role);
        if let Some(mid) = graph.find_material_scoped(&opening_poi_mat_name(role), origin) {
            graph.set_mesh(id, icosphere_mesh(0.12, 1, UvMode::Tile));
            graph.set_material(id, mid);
        }
    }
    id
}

fn opening_poi_mat_name(role: &str) -> String {
    format!("building_poi_{role}")
}

fn ensure_opening_poi_mat(graph: &mut SceneGraph, origin: Option<&Path>, role: &str) {
    let name = opening_poi_mat_name(role);
    if graph.find_material_scoped(&name, origin).is_some() {
        return;
    }
    let [r, g, b] = match role {
        "entrance" => [0.95, 0.40, 0.20], // orange
        "door" => [0.95, 0.85, 0.20],     // yellow
        "window" => [0.30, 0.80, 1.00],   // cyan
        _ => [0.80, 0.80, 0.80],
    };
    let mut m = Material::new(&name);
    m.base_color = [r, g, b, 1.0];
    m.emissive = [r, g, b];
    m.emissive_strength = 2.0;
    m.roughness = 0.5;
    m.origin = origin.map(|p| p.to_path_buf());
    graph.add_material(m);
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

