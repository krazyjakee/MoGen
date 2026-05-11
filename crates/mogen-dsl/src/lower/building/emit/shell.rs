//! Emit the floor slab, ceiling slab, and the four perimeter walls of one
//! storey. Perimeter walls carry entrance and window cutouts via the
//! existing `wall` primitive's `holes=[[x,y,w,h], …]` spec — the geometry
//! is one watertight mesh per side.

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{Mesh, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{box_mesh, clean_csg_output, difference_many, transform_mesh};

use crate::ast::Node;

use super::super::config::BuildingCfg;
use super::super::layout::{Floorplate, Rect2};
use super::openings::{Opening, OpeningPlan, WallSide};

pub(super) fn emit_shell(
    node: &Node,
    cfg: &BuildingCfg,
    plate: &Floorplate,
    plan: &OpeningPlan,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let origin = node.origin.clone();
    let bounds = &plate.bounds;
    let h = cfg.ceiling_height;
    let wt = cfg.wall_thickness;

    emit_slab(
        parent,
        graph,
        &origin,
        "slab_floor",
        bounds,
        -cfg.ceiling_thickness * 0.5,
        cfg.ceiling_thickness,
        "floor",
        wt,
    );
    emit_slab(
        parent,
        graph,
        &origin,
        "slab_ceiling",
        bounds,
        h + cfg.ceiling_thickness * 0.5,
        cfg.ceiling_thickness,
        "ceiling",
        wt,
    );

    let perimeter = [
        (WallSide::North, "wall_N"),
        (WallSide::East, "wall_E"),
        (WallSide::South, "wall_S"),
        (WallSide::West, "wall_W"),
    ];
    for (side, name) in perimeter {
        emit_perimeter_wall(parent, graph, &origin, cfg, plate, plan, side, name)?;
    }
    Ok(())
}

fn emit_slab(
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
    name: &str,
    bounds: &Rect2,
    y_centre: f32,
    thickness: f32,
    role: &str,
    wt: f32,
) {
    // Include the wall thickness in the slab footprint so the perimeter walls
    // sit atop / under the slab without a visible seam.
    let pad = wt;
    let w = bounds.width() + 2.0 * pad;
    let d = bounds.depth() + 2.0 * pad;
    let cx = 0.5 * (bounds.x_min + bounds.x_max);
    let cz = 0.5 * (bounds.z_min + bounds.z_max);
    let mesh = box_mesh([w, thickness, d], UvMode::Tile);
    let id = graph.add_child(
        parent,
        name.to_string(),
        "slab",
        Transform::from_trs(Vec3::new(cx, y_centre, cz), Quat::IDENTITY, Vec3::ONE),
    );
    graph.set_mesh(id, mesh);
    graph.nodes[id.0 as usize].origin = origin.clone();
    graph.nodes[id.0 as usize].role = Some(role.to_string());
    graph.nodes[id.0 as usize]
        .tags
        .extend(["building".into(), role.to_string()]);
    inherit_material_from_chain(id, graph);
}

fn emit_perimeter_wall(
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
    cfg: &BuildingCfg,
    plate: &Floorplate,
    plan: &OpeningPlan,
    side: WallSide,
    name: &str,
) -> Result<()> {
    let bounds = &plate.bounds;
    let h = cfg.ceiling_height;
    let wt = cfg.wall_thickness;

    // Wall geometry: a thin box in local space with X along the wall, Y up,
    // and Z into the wall (matching the existing `wall` primitive's
    // convention). We compute the local-frame holes for each opening on
    // this side, build the watertight mesh manually, and place the result
    // with a transform.
    let (length, mid_pos, rot) = wall_frame(bounds, side, wt);

    let mut local_holes: Vec<[f32; 4]> = Vec::new();
    for op in plan.entrances.iter().chain(plan.windows.iter()) {
        if op.side != Some(side) {
            continue;
        }
        if let Some(local) = opening_local(op, side, bounds, length, h) {
            local_holes.push(local);
        }
    }

    let mesh = build_wall_mesh([length, h, wt], &local_holes);
    let role = "exterior_wall";
    let id = graph.add_child(
        parent,
        name.to_string(),
        "wall",
        Transform::from_trs(
            Vec3::new(mid_pos[0], 0.5 * h, mid_pos[1]),
            rot,
            Vec3::ONE,
        ),
    );
    graph.set_mesh(id, mesh);
    graph.nodes[id.0 as usize].origin = origin.clone();
    graph.nodes[id.0 as usize].role = Some(role.into());
    graph.nodes[id.0 as usize].tags.extend([
        "building".into(),
        role.into(),
        format!("side={}", side_tag(side)),
    ]);
    inherit_material_from_chain(id, graph);
    Ok(())
}

/// Returns (length-along-wall, midpoint-xz, rotation) for a perimeter side.
/// The wall's local frame is X along the wall, Y up, Z into the wall normal.
fn wall_frame(bounds: &Rect2, side: WallSide, wt: f32) -> (f32, [f32; 2], Quat) {
    let mid_x = 0.5 * (bounds.x_min + bounds.x_max);
    let mid_z = 0.5 * (bounds.z_min + bounds.z_max);
    let half_pad = wt * 0.5;
    match side {
        // +Z side. Wall's X axis is world X. Local Z points to +Z (outside).
        WallSide::North => (
            bounds.width() + 2.0 * wt,
            [mid_x, bounds.z_max + half_pad],
            Quat::IDENTITY,
        ),
        // -Z side. Rotate 180° around Y so local Z points to -Z (outside).
        WallSide::South => (
            bounds.width() + 2.0 * wt,
            [mid_x, bounds.z_min - half_pad],
            Quat::from_rotation_y(std::f32::consts::PI),
        ),
        // +X side. Rotate -90° around Y so local X follows world Z (running
        // along the wall) and local Z points to +X.
        WallSide::East => (
            bounds.depth() + 2.0 * wt,
            [bounds.x_max + half_pad, mid_z],
            Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2),
        ),
        // -X side. Rotate +90° around Y.
        WallSide::West => (
            bounds.depth() + 2.0 * wt,
            [bounds.x_min - half_pad, mid_z],
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
        ),
    }
}

/// Convert an opening to wall-local `[cx, cy, w, h]` (the `wall` primitive's
/// hole convention).
fn opening_local(
    op: &Opening,
    side: WallSide,
    bounds: &Rect2,
    length: f32,
    height: f32,
) -> Option<[f32; 4]> {
    let along = match side {
        WallSide::North => op.x - 0.5 * (bounds.x_min + bounds.x_max),
        WallSide::South => -(op.x - 0.5 * (bounds.x_min + bounds.x_max)),
        WallSide::East => -(op.z - 0.5 * (bounds.z_min + bounds.z_max)),
        WallSide::West => op.z - 0.5 * (bounds.z_min + bounds.z_max),
    };
    let cy = op.sill + 0.5 * op.height - 0.5 * height;
    // Reject openings that overflow the wall — keeps the carve operation
    // from producing degenerate slivers.
    if op.width >= length - 0.2 || op.height >= height - 0.1 {
        return None;
    }
    Some([along, cy, op.width, op.height])
}

/// Build a `wall`-shaped mesh manually: a thin box with rectangular holes
/// carved through the Z axis. Mirrors the existing `wall` primitive lowering
/// but we author it inline so we don't have to round-trip through synthetic
/// AST construction. The output is cleaned for export.
fn build_wall_mesh(size: [f32; 3], holes: &[[f32; 4]]) -> Mesh {
    let base = box_mesh(size, UvMode::Tile);
    if holes.is_empty() {
        return base;
    }
    let cutouts: Vec<Mesh> = holes
        .iter()
        .map(|&[hx, hy, hw, hh]| {
            let c = box_mesh(
                [hw.max(1e-4), hh.max(1e-4), size[2] + 0.02],
                UvMode::Tile,
            );
            transform_mesh(&c, glam::Mat4::from_translation(Vec3::new(hx, hy, 0.0)))
        })
        .collect();
    clean_csg_output(&difference_many(&base, &cutouts))
}

fn side_tag(side: WallSide) -> &'static str {
    match side {
        WallSide::North => "north",
        WallSide::East => "east",
        WallSide::South => "south",
        WallSide::West => "west",
    }
}

/// Inherit material from the nearest ancestor that has one. The building
/// wrapper picks up its `mat=` via `apply_metadata`, so this propagates
/// downward into the shell.
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
