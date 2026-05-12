//! Roof emission. Tranche 1 shipped `flat` (the top-storey ceiling slab
//! doubles as the roof). Tranche 4 adds the five non-flat shapes —
//! `pitched`, `gabled`, `hipped`, `mansard`, `shed` — built from the
//! existing `wedge_mesh`, `frustum_mesh`, and `extrude_mesh` primitives.
//!
//! Non-flat roofs **replace** the top-storey ceiling slab (see
//! `shell.rs:64` for the gating). The roof mesh provides the upper
//! closure of the volume; gable end-walls are emitted as separate
//! triangular extrusions flush with the perimeter walls.
//!
//! All roofs sit on top of the topmost rendered storey. The roof mesh's
//! local frame puts y=0 at the top-storey ceiling (i.e. one
//! `ceiling_height + ceiling_thickness` above the floor slab); meshes
//! built relative to y=0 then translate up by their own half-height so
//! the roof base is flush.

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{Mesh, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{extrude_mesh, frustum_mesh, wedge_mesh};

use crate::ast::Node;

use super::super::config::{BuildingCfg, Roof};
use super::super::layout::Floorplate;
use super::StoreyCtx;

/// Default pitch for sloped roofs. ~30° reads as "house roof" without
/// becoming a steeple. Exposed as a constant rather than an attr because
/// the surface area of `building` already balloons fast — every later
/// tweak can land as a separate attr if authoring demand appears.
const DEFAULT_PITCH_DEG: f32 = 30.0;

pub(super) fn emit_roof(
    node: &Node,
    cfg: &BuildingCfg,
    plate: &Floorplate,
    ctx: StoreyCtx,
    floor_group: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    if cfg.roof == Roof::Flat {
        return Ok(()); // shell.rs's top-storey ceiling slab IS the roof.
    }
    // Roof geometry only sits on the topmost storey, and only when that
    // storey is actually rendered. `is_top` already factors in
    // `debug_hide_roof` and the single-storey debug filter.
    if !ctx.is_top {
        return Ok(());
    }
    let origin = node.origin.clone();

    let bounds = plate.bounds;
    let pad = cfg.wall_thickness;
    let w = bounds.width() + 2.0 * pad;
    let d = bounds.depth() + 2.0 * pad;
    let cx = 0.5 * (bounds.x_min + bounds.x_max);
    let cz = 0.5 * (bounds.z_min + bounds.z_max);
    // Top of the perimeter walls (in the storey-local frame the floor
    // group is anchored at). Roof base sits **flush** with the wall top —
    // any gap larger than `CONNECTIVITY_SLOP` (2 mm) would make the roof
    // register as a disconnected cluster.
    let base_y = cfg.ceiling_height;
    let pitch = DEFAULT_PITCH_DEG.to_radians();
    let half_short = 0.5 * w.min(d);
    let roof_h = (half_short * pitch.tan()).max(0.4);

    let roof_group = graph.add_child(
        floor_group,
        "roof".to_string(),
        "group",
        Transform::from_trs(Vec3::new(cx, base_y, cz), Quat::IDENTITY, Vec3::ONE),
    );
    graph.nodes[roof_group.0 as usize].origin = origin.clone();
    graph.nodes[roof_group.0 as usize].tags.extend([
        "building".into(),
        format!("storey={}", ctx.storey),
        "roof".into(),
    ]);

    match cfg.roof {
        Roof::Flat => unreachable!(),
        Roof::Shed => emit_shed(roof_group, graph, &origin, w, d, roof_h)?,
        Roof::Pitched | Roof::Gabled => emit_gabled(
            roof_group,
            graph,
            &origin,
            w,
            d,
            roof_h,
            cfg.wall_thickness,
        )?,
        Roof::Hipped => emit_hipped(roof_group, graph, &origin, w, d, roof_h)?,
        Roof::Mansard => emit_mansard(roof_group, graph, &origin, w, d, pitch)?,
    }
    Ok(())
}

/// Single wedge sloping from south (low) to north (high). Slope rises
/// along +Z to match the existing `wall_S`/`wall_N` convention so authors
/// reading the source can place a clerestory toward the high side.
fn emit_shed(
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
    w: f32,
    d: f32,
    h: f32,
) -> Result<()> {
    let mesh = wedge_mesh([w, h, d], UvMode::Tile);
    emit_roof_mesh(parent, graph, origin, "roof_shed", mesh, 0.0, 0.5 * h, 0.0, Quat::IDENTITY);
    Ok(())
}

/// Two mirrored wedges meeting at a ridge along the longer axis, plus two
/// triangular gable end walls flush with the perimeter walls. When the
/// footprint is square the ridge defaults to the X axis (the wider /
/// equal-length axis comes first alphabetically — keeps the gabled house
/// example reproducible).
fn emit_gabled(
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
    w: f32,
    d: f32,
    h: f32,
    wall_thickness: f32,
) -> Result<()> {
    // Ridge runs along the longer axis. Two wedges face each other across
    // the ridge — each is half-width on the short axis and sits over its
    // half of the footprint.
    //
    // `wedge_mesh` has its tall back wall at local z=-hz; the body extends
    // toward +Z; the slope rises as Z DECREASES (from front-bottom at
    // +hz/−hy to back-top at −hz/+hy). For a gable, each half-wedge wants
    // its tall side (the ridge) toward the floorplate centre and its
    // short side (the eave) at the outer edge.
    let ridge_along_x = w >= d;
    let (wedge_size, slope_rot_left, slope_rot_right, gable_axis) = if ridge_along_x {
        // Two wedges side-by-side along Z. Each is [w, h, d/2]. Ridge at z=0.
        //  - North half (z ∈ [0, d/2]): slope rises from north-eave (high z) to
        //    ridge (z=0) — exactly the default wedge orientation, no rotation.
        //  - South half (z ∈ [-d/2, 0]): slope rises from south-eave (low z) to
        //    ridge — mirror the wedge along Z by rotating 180° around Y.
        let wedge = [w, h, 0.5 * d];
        let rot_north = Quat::IDENTITY;
        let rot_south = Quat::from_rotation_y(std::f32::consts::PI);
        (wedge, rot_south, rot_north, AxisAlong::X)
    } else {
        // Ridge along Z. Each half-wedge is [d, h, w/2] after the 90° Y
        // rotation that puts the wedge's z extent onto world X.
        //  - East half (x ∈ [0, w/2]): rotate -90° around Y so default +Z body
        //    swings to +X, slope rising from east-eave to ridge.
        //  - West half: rotate +90° around Y for the mirror.
        let wedge = [d, h, 0.5 * w];
        let rot_east = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2);
        let rot_west = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        (wedge, rot_west, rot_east, AxisAlong::Z)
    };

    // Position the two wedges centred on their half-footprints.
    let (left_offset, right_offset) = match gable_axis {
        AxisAlong::X => (
            // Left = negative Z half, centre at z = -d/4.
            Vec3::new(0.0, 0.5 * h, -0.25 * d),
            Vec3::new(0.0, 0.5 * h, 0.25 * d),
        ),
        AxisAlong::Z => (
            Vec3::new(-0.25 * w, 0.5 * h, 0.0),
            Vec3::new(0.25 * w, 0.5 * h, 0.0),
        ),
    };

    let left_mesh = wedge_mesh(wedge_size, UvMode::Tile);
    let right_mesh = wedge_mesh(wedge_size, UvMode::Tile);
    emit_roof_mesh(
        parent,
        graph,
        origin,
        "roof_slope_a",
        left_mesh,
        left_offset.x,
        left_offset.y,
        left_offset.z,
        slope_rot_left,
    );
    emit_roof_mesh(
        parent,
        graph,
        origin,
        "roof_slope_b",
        right_mesh,
        right_offset.x,
        right_offset.y,
        right_offset.z,
        slope_rot_right,
    );

    // Gable end walls: triangular extrusions flush with the perimeter
    // walls on the short axis. The contour is built in XZ (the extrude
    // pipeline's natural plane) with the apex at Z=h; we then rotate it
    // up via -90° around X so the apex ends up at +Y. For ridge_along_z
    // we additionally rotate 90° around Y to swap the gable into the
    // perpendicular plane.
    let up = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    let (gable_pos_a, gable_pos_b, gable_rot, gable_width) = match gable_axis {
        AxisAlong::X => (
            Vec3::new(0.0, 0.0, 0.5 * d),
            Vec3::new(0.0, 0.0, -0.5 * d),
            up,
            w,
        ),
        AxisAlong::Z => (
            Vec3::new(0.5 * w, 0.0, 0.0),
            Vec3::new(-0.5 * w, 0.0, 0.0),
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2) * up,
            d,
        ),
    };
    let triangle = gable_triangle(gable_width, h);
    let mesh_a = extrude_mesh(&triangle, &[], wall_thickness, 1.0, 0.0, true, UvMode::Tile);
    let mesh_b = extrude_mesh(&triangle, &[], wall_thickness, 1.0, 0.0, true, UvMode::Tile);
    emit_gable_wall(parent, graph, origin, "gable_a", mesh_a, gable_pos_a, gable_rot);
    emit_gable_wall(parent, graph, origin, "gable_b", mesh_b, gable_pos_b, gable_rot);
    Ok(())
}

/// Hipped roof — four sloping faces meeting at a ridge (rectangular
/// footprint) or apex (square footprint). Built as one truncated pyramid
/// (`frustum_mesh`) so the four faces are a single watertight mesh.
fn emit_hipped(
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
    w: f32,
    d: f32,
    h: f32,
) -> Result<()> {
    // Ridge length = |w - d|; ridge runs along the longer axis. Apex if
    // w == d, otherwise a non-zero top extent in the longer direction.
    let ridge_w = (w - d).max(0.0);
    let ridge_d = (d - w).max(0.0);
    let mesh = frustum_mesh([w, d], [ridge_w, ridge_d], h, UvMode::Tile);
    emit_roof_mesh(
        parent,
        graph,
        origin,
        "roof_hipped",
        mesh,
        0.0,
        0.5 * h,
        0.0,
        Quat::IDENTITY,
    );
    Ok(())
}

/// Mansard — two stacked frustums. The lower tier is steeply sloped
/// (≈60°), the upper tier is shallow (≈15°) and tapers to a short ridge.
/// Reads as a Second Empire / French roof when viewed in profile.
fn emit_mansard(
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
    w: f32,
    d: f32,
    pitch: f32,
) -> Result<()> {
    let _ = pitch;
    // Geometry tuned by inspection — total mansard height ≈ 60% of
    // footprint short side, lower tier carries most of it.
    let half_short = 0.5 * w.min(d);
    let lower_h = (0.9 * half_short).max(0.6);
    let lower_steep = 60f32.to_radians();
    let lower_inset = (lower_h / lower_steep.tan()).min(0.45 * half_short);
    let lower_top_w = (w - 2.0 * lower_inset).max(0.3);
    let lower_top_d = (d - 2.0 * lower_inset).max(0.3);
    let lower = frustum_mesh([w, d], [lower_top_w, lower_top_d], lower_h, UvMode::Tile);
    emit_roof_mesh(
        parent,
        graph,
        origin,
        "roof_mansard_lower",
        lower,
        0.0,
        0.5 * lower_h,
        0.0,
        Quat::IDENTITY,
    );

    let upper_h = (0.35 * half_short).max(0.25);
    // Upper tier tapers further to a short ridge along the longer axis.
    let upper_top_w = if lower_top_w >= lower_top_d {
        (lower_top_w - lower_top_d).max(0.05)
    } else {
        0.05
    };
    let upper_top_d = if lower_top_d > lower_top_w {
        (lower_top_d - lower_top_w).max(0.05)
    } else {
        0.05
    };
    let upper = frustum_mesh(
        [lower_top_w, lower_top_d],
        [upper_top_w, upper_top_d],
        upper_h,
        UvMode::Tile,
    );
    emit_roof_mesh(
        parent,
        graph,
        origin,
        "roof_mansard_upper",
        upper,
        0.0,
        lower_h + 0.5 * upper_h,
        0.0,
        Quat::IDENTITY,
    );
    Ok(())
}

fn emit_roof_mesh(
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
    name: &str,
    mesh: Mesh,
    x: f32,
    y: f32,
    z: f32,
    rot: Quat,
) {
    let id = graph.add_child(
        parent,
        name.to_string(),
        "mesh",
        Transform::from_trs(Vec3::new(x, y, z), rot, Vec3::ONE),
    );
    graph.set_mesh(id, mesh);
    graph.nodes[id.0 as usize].origin = origin.clone();
    graph.nodes[id.0 as usize].role = Some("roof".into());
    graph.nodes[id.0 as usize]
        .tags
        .extend(["building".into(), "roof".into()]);
    inherit_material_from_chain(id, graph);
}

fn emit_gable_wall(
    parent: NodeId,
    graph: &mut SceneGraph,
    origin: &Option<std::path::PathBuf>,
    name: &str,
    mesh: Mesh,
    pos: Vec3,
    rot: Quat,
) {
    let id = graph.add_child(
        parent,
        name.to_string(),
        "mesh",
        Transform::from_trs(pos, rot, Vec3::ONE),
    );
    graph.set_mesh(id, mesh);
    graph.nodes[id.0 as usize].origin = origin.clone();
    graph.nodes[id.0 as usize].role = Some("gable_wall".into());
    graph.nodes[id.0 as usize]
        .tags
        .extend(["building".into(), "gable_wall".into()]);
    inherit_material_from_chain(id, graph);
}

/// 2D contour for a gable end wall: an isoceles triangle of width
/// `width` (running along the contour's X axis) and apex height `h`.
/// Bottom edge at y=0, apex at (0, h). The contour is wound CCW so the
/// extrude pipeline triangulates the cap correctly.
fn gable_triangle(width: f32, h: f32) -> Vec<[f32; 2]> {
    vec![
        [-0.5 * width, 0.0],
        [0.5 * width, 0.0],
        [0.0, h],
    ]
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

#[derive(Clone, Copy, Debug)]
enum AxisAlong {
    X,
    Z,
}
