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
use crate::lower::arch;

use super::super::circulation::{CirculationKind, CirculationPlan, STAIR_ENTRY_DEPTH};
use super::super::config::BuildingCfg;
use super::super::layout::{Floorplate, Rect2, WallSide};
use super::openings::OpeningPlan;
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
    //
    // For staircases the cutout preserves a south entry zone on every
    // storey — that's the platform the user steps onto from the adjacent
    // room (door cutout in the west wall lands here) and the landing the
    // descending flight delivers them to. The rest of the cell is cut so
    // both half-flights and the mid-landing have clearance to span the
    // full storey height. See `emit/circulation.rs` for the layout.
    let floor_holes: Vec<Rect2> = if ctx.is_bottom {
        Vec::new()
    } else {
        circ.cells
            .iter()
            .map(|c| match c.kind {
                CirculationKind::Staircase => staircase_slab_hole(c.rect),
                CirculationKind::Elevator => c.rect,
            })
            .collect()
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

    // Ceiling slab: only the topmost storey emits one — and only when the
    // roof is `flat`, in which case the slab IS the roof. For every non-
    // flat roof (gabled/pitched/hipped/mansard/shed) `roof::emit_roof`
    // replaces the slab with a proper roof mesh, so emitting a slab here
    // would leave a flat plane visible from below through the attic.
    // Every other storey's ceiling IS the floor slab of the storey above.
    // Skylight rects (planned upstream so shell + skylight emission see
    // identical XY) are carved into it here so the slab geometry is one
    // watertight mesh.
    if ctx.is_top && cfg.roof.is_flat() {
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
    // All four solved together: the corners are mitres, and a mitre is a
    // property of the junction rather than of either wall. Solved one at a time
    // they would each square off and the four would double-cover every corner,
    // which is what the box builder did.
    let requests: Vec<arch::WallRequest> = perimeter
        .iter()
        .map(|(side, _)| wall_request(cfg, plate, plan, *side))
        .collect();
    let meshes = arch::solve_wall_meshes(&requests);
    for ((side, name), mesh) in perimeter.into_iter().zip(meshes) {
        emit_perimeter_wall(parent, graph, &origin, cfg, plate, side, name, mesh)?;
    }
    Ok(())
}

/// Hole rect for a staircase cell on an upper storey's floor slab. The
/// switchback occupies the full width north of the entry zone — both
/// half-flights and the mid-landing need clearance to span a full
/// storey — so the cutout is the cell minus the south entry strip. The
/// preserved strip is the platform the door from the adjacent room
/// opens onto on every floor.
fn staircase_slab_hole(cell: Rect2) -> Rect2 {
    let entry = STAIR_ENTRY_DEPTH.min(cell.depth() * 0.6);
    Rect2 {
        x_min: cell.x_min,
        x_max: cell.x_max,
        z_min: cell.z_min + entry,
        z_max: cell.z_max,
    }
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
        // The cutout is inflated by a tiny epsilon on every XY edge — just
        // enough to avoid coplanar-face artefacts in the CSG difference —
        // and never more, so the cutout stops at the cell boundary.
        //
        // Crucially we must NOT extend the cutout outward through the
        // perimeter wall thickness, even when a hole edge is flush with the
        // floorplate bounds (e.g. the east-column staircase, whose east edge
        // sits on `bounds.x_max` — the interior face of the east wall). The
        // slab footprint runs a wall-thickness past `bounds` so its rim sits
        // under the perimeter walls; that under-wall strip is the only thing
        // bridging the gap between one storey's wall and the next, so its
        // exposed edge is what seals the exterior envelope at each floor
        // division. Punching the cutout through it (the old `exit_pad`)
        // removed that strip wherever a stair/elevator touched a wall,
        // opening a ceiling_thickness-tall hole straight through the
        // building's outer shell — visible as a gap in the floor-line band.
        // The strip is hidden behind the perimeter wall from inside the
        // shaft, so keeping it costs nothing and keeps the shell watertight.
        let eps = 1e-3f32;
        let cutouts: Vec<Mesh> = holes_xz
            .iter()
            .map(|r| {
                let pad_xmin = eps;
                let pad_xmax = eps;
                let pad_zmin = eps;
                let pad_zmax = eps;
                let hw = (r.width() + pad_xmin + pad_xmax).max(1e-4);
                let hd = (r.depth() + pad_zmin + pad_zmax).max(1e-4);
                let hcx = 0.5 * ((r.x_min - pad_xmin) + (r.x_max + pad_xmax)) - cx;
                let hcz = 0.5 * ((r.z_min - pad_zmin) + (r.z_max + pad_zmax)) - cz;
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

/// One perimeter wall as a centreline the mitre solver can join.
///
/// The four centrelines run corner to corner and share their endpoints exactly,
/// which is what turns four independent walls into one closed loop. Note what
/// this does *not* change: the centre of each centreline is the same point the
/// box builder used for the node, so the node transform is untouched and every
/// test keyed to it keeps passing.
///
/// What does change is the length — `width + wt` rather than `width + 2·wt`.
/// The box builder padded each wall by a full thickness at both ends so the
/// corners were covered twice; mitring covers each corner once. The outer
/// envelope is identical, the inner faces are identical, and the difference is
/// exactly the four `wt × wt` corner columns.
fn wall_request(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    plan: &OpeningPlan,
    side: WallSide,
) -> arch::WallRequest {
    let bounds = &plate.bounds;
    let h = cfg.ceiling_height;
    let wt = cfg.wall_thickness;
    let half = 0.5 * wt;

    // The four points where the centrelines meet.
    let (x0, x1) = (bounds.x_min - half, bounds.x_max + half);
    let (z0, z1) = (bounds.z_min - half, bounds.z_max + half);

    // `axis_x` is each wall's local +X restated as an exact world vector, and
    // matches the `rot` that `wall_frame` hands the node. Start and end are
    // listed in that same direction so the wall's own parameterisation runs
    // forwards.
    let (start, end, axis_x, axis_z) = match side {
        WallSide::North => ([x0, z1], [x1, z1], [1.0, 0.0], [0.0, 1.0]),
        WallSide::South => ([x1, z0], [x0, z0], [-1.0, 0.0], [0.0, -1.0]),
        WallSide::East => ([x1, z0], [x1, z1], [0.0, 1.0], [-1.0, 0.0]),
        WallSide::West => ([x0, z1], [x0, z0], [0.0, -1.0], [1.0, 0.0]),
    };

    let (length, mid_pos, _) = wall_frame(bounds, side, wt);
    let mut req = arch::WallRequest {
        start,
        end,
        thickness: wt,
        height: h,
        axis_x,
        axis_z,
        centre: mid_pos,
        holes: Vec::new(),
    };

    // The wall projects its own openings -- see `WallRequest::hole`. This used
    // to be a four-armed match on the compass side, and two of the four arms
    // had been wrong at some point.
    req.holes = plan
        .entrances
        .iter()
        .chain(plan.windows.iter())
        .filter(|op| op.side == Some(side))
        // An opening as wide as the wall or as tall as the storey is not an
        // opening, it is a missing wall. Left out rather than cut, so the wall
        // stays solid and the window model sits against it.
        .filter(|op| op.width < length - 0.2 && op.height < h - 0.1)
        .map(|op| req.hole([op.x, op.z], op.sill, op.width, op.height))
        .collect();
    req
}

fn emit_perimeter_wall(
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
    cfg: &BuildingCfg,
    plate: &Floorplate,
    side: WallSide,
    name: &str,
    mesh: mogen_core::Mesh,
) -> Result<()> {
    let bounds = &plate.bounds;
    let h = cfg.ceiling_height;
    let wt = cfg.wall_thickness;

    let (_, mid_pos, rot) = wall_frame(bounds, side, wt);
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

/// A perimeter wall's centreline length, node origin and node rotation.
///
/// The length is `width + wt`, i.e. from corner *centreline* to corner
/// centreline: half a thickness past the plate at each end, which is where the
/// adjacent wall's centreline runs. The box builder used `width + 2·wt` so that
/// each wall reached the far side of both corners and every corner was built
/// twice; mitring builds each one once and the envelope is unchanged.
fn wall_frame(bounds: &Rect2, side: WallSide, wt: f32) -> (f32, [f32; 2], Quat) {
    let mid_x = 0.5 * (bounds.x_min + bounds.x_max);
    let mid_z = 0.5 * (bounds.z_min + bounds.z_max);
    let half_pad = wt * 0.5;
    match side {
        WallSide::North => (
            bounds.width() + wt,
            [mid_x, bounds.z_max + half_pad],
            Quat::IDENTITY,
        ),
        WallSide::South => (
            bounds.width() + wt,
            [mid_x, bounds.z_min - half_pad],
            Quat::from_rotation_y(std::f32::consts::PI),
        ),
        WallSide::East => (
            bounds.depth() + wt,
            [bounds.x_max + half_pad, mid_z],
            Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2),
        ),
        WallSide::West => (
            bounds.depth() + wt,
            [bounds.x_min - half_pad, mid_z],
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
        ),
    }
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
