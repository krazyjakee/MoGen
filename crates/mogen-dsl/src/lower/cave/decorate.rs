//! Decoration pass: scatters the optional cave features (stalagmites,
//! stalactites, rock piles, pools, lakes) onto the chamber floors and ceilings
//! produced by the layout solver.
//!
//! Decorations are independent leaf meshes parented under a `decorations`
//! group — they are not carved into the rock field, so the pass is pure mesh
//! construction with no CSG. Placement is fully seeded, so the same `seed=`
//! always lands the same drip in the same spot.

use std::f32::consts::TAU;

use glam::{Quat, Vec3};

use mogen_core::{Mesh, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{cone_mesh, disc_mesh, icosphere_mesh, jitter, transform_mesh};

use crate::ast::Node;

use super::config::{CaveCfg, DecoGroup, DecoKind};
use super::generate::{CaveLayout, Chamber};
use super::materials::{ROCK_MAT, WATER_MAT};
use super::rng::{rand_f01, rand_in, rand_range, sub_seed};

pub(super) fn emit_decorations(
    node: &Node,
    cfg: &CaveCfg,
    layout: &CaveLayout,
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    if cfg.decorations.is_empty() || layout.chambers.is_empty() {
        return;
    }
    let origin = node.origin.clone();

    let deco_group = graph.add_child(parent, "decorations".to_string(), "group", Transform::IDENTITY);
    graph.nodes[deco_group.0 as usize].origin = origin.clone();
    graph.nodes[deco_group.0 as usize]
        .tags
        .extend(["cave".to_string(), "decorations".to_string()]);

    for (gi, group) in cfg.decorations.iter().enumerate() {
        let mut state = sub_seed(cfg.seed, 0x0DEC_0000 ^ (gi as u32));
        let group_node = graph.add_child(
            deco_group,
            format!("{}s", group.kind.label()),
            "group",
            Transform::IDENTITY,
        );
        graph.nodes[group_node.0 as usize].origin = origin.clone();

        for n in 0..group.count {
            let c = &layout.chambers[rand_range(&mut state, layout.chambers.len() as u32) as usize];
            let size = rand_in(&mut state, group.min_size, group.max_size);
            let (name, mesh, transform) = match group.kind {
                DecoKind::Stalagmite => stalagmite(c, size, &mut state, n),
                DecoKind::Stalactite => stalactite(c, size, &mut state, n),
                DecoKind::RockPile => rock_pile(c, size, &mut state, n),
                DecoKind::Pool => water(c, size, &mut state, n, false),
                DecoKind::Lake => water(c, size, &mut state, n, true),
            };
            let id = graph.add_child(group_node, name, "mesh", transform);
            graph.set_mesh(id, mesh);
            graph.nodes[id.0 as usize].origin = origin.clone();
            graph.nodes[id.0 as usize].role = Some(group.kind.label().to_string());
            graph.nodes[id.0 as usize]
                .tags
                .extend(["cave".to_string(), group.kind.label().to_string()]);
            bind_decoration_material(id, group, node.origin.as_deref(), graph);
        }
    }
}

/// A random point on the walkable disc of a chamber floor (uniform over area).
fn floor_point(c: &Chamber, state: &mut u32) -> Vec3 {
    let ang = rand_in(state, 0.0, TAU);
    let rr = c.floor_radius() * rand_f01(state).sqrt();
    Vec3::new(c.center.x + rr * ang.cos(), c.floor_y(), c.center.z + rr * ang.sin())
}

fn ceiling_point(c: &Chamber, state: &mut u32) -> Vec3 {
    let ang = rand_in(state, 0.0, TAU);
    let rr = c.floor_radius() * rand_f01(state).sqrt();
    Vec3::new(c.center.x + rr * ang.cos(), c.ceiling_y(), c.center.z + rr * ang.sin())
}

fn stalagmite(c: &Chamber, size: f32, state: &mut u32, n: u32) -> (String, Mesh, Transform) {
    let height = size * 2.0;
    let base_r = size * 0.45;
    let mut mesh = cone_mesh(base_r, height, 8, UvMode::Tile);
    jitter(&mut mesh, 0.12, sub_seed(*state, n), None);
    // Cone base sits at -height/2; lift so the base rests on the floor.
    let pos = floor_point(c, state) + Vec3::new(0.0, height * 0.5, 0.0);
    let yaw = rand_in(state, 0.0, TAU);
    (
        format!("stalagmite_{n}"),
        mesh,
        Transform::from_trs(pos, Quat::from_rotation_y(yaw), Vec3::ONE),
    )
}

fn stalactite(c: &Chamber, size: f32, state: &mut u32, n: u32) -> (String, Mesh, Transform) {
    let height = size * 2.0;
    let base_r = size * 0.4;
    let mut mesh = cone_mesh(base_r, height, 8, UvMode::Tile);
    jitter(&mut mesh, 0.12, sub_seed(*state, n), None);
    // Flip so the apex points down; base sits flush against the ceiling.
    let pos = ceiling_point(c, state) - Vec3::new(0.0, height * 0.5, 0.0);
    let flip = Quat::from_rotation_x(std::f32::consts::PI) * Quat::from_rotation_y(rand_in(state, 0.0, TAU));
    (
        format!("stalactite_{n}"),
        mesh,
        Transform::from_trs(pos, flip, Vec3::ONE),
    )
}

fn rock_pile(c: &Chamber, size: f32, state: &mut u32, n: u32) -> (String, Mesh, Transform) {
    let base = floor_point(c, state);
    let count = 3 + rand_range(state, 4); // 3..6 boulders
    let mut acc = Mesh::default();
    for k in 0..count {
        let r = size * rand_in(state, 0.35, 0.7);
        let ox = rand_in(state, -size, size);
        let oz = rand_in(state, -size, size);
        let oy = r * 0.7;
        let mut boulder = icosphere_mesh(r, 1, UvMode::Tile);
        jitter(&mut boulder, 0.18, sub_seed(*state, n * 17 + k), None);
        let placed = transform_mesh(
            &boulder,
            glam::Mat4::from_translation(Vec3::new(ox, oy, oz)),
        );
        append_mesh(&mut acc, &placed);
    }
    (
        format!("rock_pile_{n}"),
        acc,
        Transform::from_translation(base),
    )
}

fn water(c: &Chamber, size: f32, state: &mut u32, n: u32, lake: bool) -> (String, Mesh, Transform) {
    // Clamp the surface to the chamber so a lake can't spill through the wall.
    let radius = size.min(c.floor_radius() * if lake { 1.6 } else { 0.9 }).max(0.4);
    let mesh = disc_mesh(radius, 24, UvMode::Tile);
    // Pool surface sits just above the floor.
    let pos = Vec3::new(
        c.center.x + rand_in(state, -0.2, 0.2),
        c.floor_y() + 0.05,
        c.center.z + rand_in(state, -0.2, 0.2),
    );
    let label = if lake { "lake" } else { "pool" };
    (format!("{label}_{n}"), mesh, Transform::from_translation(pos))
}

fn append_mesh(acc: &mut Mesh, src: &Mesh) {
    let base = acc.positions.len() as u32;
    acc.positions.extend_from_slice(&src.positions);
    acc.normals.extend_from_slice(&src.normals);
    for &i in &src.indices {
        acc.indices.push(base + i);
    }
    if !src.uvs.is_empty() {
        acc.uvs.extend_from_slice(&src.uvs);
    }
}

/// Bind the decoration's material: an explicit `feature(mat=…)` wins, then the
/// kind's default (`cave_water` for pools/lakes, `cave_rock` otherwise). The
/// rock default deliberately differs from the cave shell so scattered features
/// stay legible against the walls when the user hasn't themed them.
fn bind_decoration_material(
    id: NodeId,
    group: &DecoGroup,
    origin: Option<&std::path::Path>,
    graph: &mut SceneGraph,
) {
    let name = group.mat.as_deref().unwrap_or_else(|| {
        if group.kind.is_water() {
            WATER_MAT
        } else {
            ROCK_MAT
        }
    });
    if let Some(mid) = graph.find_material_scoped(name, origin) {
        graph.set_material(id, mid);
    }
}
