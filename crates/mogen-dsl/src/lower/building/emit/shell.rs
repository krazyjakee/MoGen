//! Emit the floor slab, ceiling slab, and the four perimeter walls of one
//! storey. Perimeter walls carry entrance and window cutouts via the
//! existing `wall` primitive's `holes=[[x,y,w,h], …]` spec — the geometry
//! is one watertight mesh per side. Slabs are carved by CSG when a
//! staircase or elevator passes through them.

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{Mesh, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{box_mesh, clean_csg_output, difference_many, transform_mesh};

use crate::ast::Node;

use super::super::circulation::CirculationPlan;
use super::super::config::BuildingCfg;
use super::super::layout::{Floorplate, Rect2};
use super::openings::{Opening, OpeningPlan, WallSide};
use super::wall_build::wall_with_holes;
use super::StoreyCtx;

pub(super) fn emit_shell(
    node: &Node,
    cfg: &BuildingCfg,
    plate: &Floorplate,
    plan: &OpeningPlan,
    circ: &CirculationPlan,
    skylight_rects: &[Rect2],
    ctx: StoreyCtx,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let origin = node.origin.clone();
    let bounds = &plate.bounds;
    let h = cfg.ceiling_height;
    let wt = cfg.wall_thickness;

    // Floor slab. Carved with circulation holes unless this is the
    // bottommost storey (the foundation/basement floor stays intact —
    // stairs and elevators start here).
    let floor_holes = if ctx.is_bottom {
        Vec::new()
    } else {
        circ.cells.iter().map(|c| c.rect).collect()
    };
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
        &floor_holes,
    );

    // Ceiling slab: only the topmost storey emits one (it doubles as the
    // roof for flat-roof buildings). Every other storey's ceiling IS the
    // floor slab of the storey above. Skylight rects (planned upstream so
    // shell + skylight emission see identical XY) are carved into it here
    // so the slab geometry is one watertight mesh.
    if ctx.is_top {
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
            skylight_rects,
        );
    }

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
    holes_xz: &[Rect2],
) {
    // Include the wall thickness in the slab footprint so the perimeter
    // walls sit atop / under the slab without a visible seam.
    let pad = wt;
    let w = bounds.width() + 2.0 * pad;
    let d = bounds.depth() + 2.0 * pad;
    let cx = 0.5 * (bounds.x_min + bounds.x_max);
    let cz = 0.5 * (bounds.z_min + bounds.z_max);
    let base = box_mesh([w, thickness, d], UvMode::Tile);
    let mesh = if holes_xz.is_empty() {
        base
    } else {
        // Cutout boxes are inflated by `pad_xy` on every in-plane axis they
        // reach, so a hole that ends exactly at the slab edge (e.g. a
        // staircase carved out of the east-side circulation column) still
        // emerges as a cleanly cut U-shape rather than as a manifold-
        // degenerate coplanar face. The vertical inflation is small and
        // symmetric so the cutout pokes through the top and bottom of the
        // slab.
        let pad_xy = 0.05f32;
        let cutouts: Vec<Mesh> = holes_xz
            .iter()
            .map(|r| {
                let hw = (r.width() + 2.0 * pad_xy).max(1e-4);
                let hd = (r.depth() + 2.0 * pad_xy).max(1e-4);
                let hcx = 0.5 * (r.x_min + r.x_max) - cx;
                let hcz = 0.5 * (r.z_min + r.z_max) - cz;
                let cutout = box_mesh([hw, thickness + 0.1, hd], UvMode::Tile);
                transform_mesh(
                    &cutout,
                    glam::Mat4::from_translation(Vec3::new(hcx, 0.0, hcz)),
                )
            })
            .collect();
        clean_csg_output(&difference_many(&base, &cutouts))
    };
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

    let mesh = wall_with_holes([length, h, wt], &local_holes);
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

fn wall_frame(bounds: &Rect2, side: WallSide, wt: f32) -> (f32, [f32; 2], Quat) {
    let mid_x = 0.5 * (bounds.x_min + bounds.x_max);
    let mid_z = 0.5 * (bounds.z_min + bounds.z_max);
    let half_pad = wt * 0.5;
    match side {
        WallSide::North => (
            bounds.width() + 2.0 * wt,
            [mid_x, bounds.z_max + half_pad],
            Quat::IDENTITY,
        ),
        WallSide::South => (
            bounds.width() + 2.0 * wt,
            [mid_x, bounds.z_min - half_pad],
            Quat::from_rotation_y(std::f32::consts::PI),
        ),
        WallSide::East => (
            bounds.depth() + 2.0 * wt,
            [bounds.x_max + half_pad, mid_z],
            Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2),
        ),
        WallSide::West => (
            bounds.depth() + 2.0 * wt,
            [bounds.x_min - half_pad, mid_z],
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
        ),
    }
}

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
    if op.width >= length - 0.2 || op.height >= height - 0.1 {
        return None;
    }
    Some([along, cy, op.width, op.height])
}

fn side_tag(side: WallSide) -> &'static str {
    match side {
        WallSide::North => "north",
        WallSide::East => "east",
        WallSide::South => "south",
        WallSide::West => "west",
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
