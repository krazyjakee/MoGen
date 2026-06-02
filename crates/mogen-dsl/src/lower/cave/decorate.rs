//! Decoration pass: scatters the optional cave features (stalagmites,
//! stalactites, rock piles, pools, lakes) onto the chamber floors and ceilings.
//!
//! Placement marches the **actual carved rock field** (the same `box − ⋃
//! carvers` field the shell is meshed from) to find the true floor / ceiling
//! under each feature, then sinks the feature slightly into that surface. This
//! is what stops drips and boulders floating: the geometric ellipsoid floor is
//! not where the blended, eroded cavity surface actually lands, so we query the
//! field instead of assuming.
//!
//! Decorations are independent leaf meshes parented under a `decorations`
//! group — they are not carved into the field, so the pass is pure mesh
//! construction with no CSG. Placement is fully seeded, so the same `seed=`
//! always lands the same drip in the same spot.

use std::f32::consts::TAU;

use glam::{Quat, Vec3};

use mogen_core::{Mesh, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{cone_mesh, disc_mesh, evaluate_field, icosphere_mesh, jitter, transform_mesh, BlobChild};

use crate::ast::Node;

use super::config::{CaveCfg, DecoGroup, DecoKind};
use super::generate::{rock_field, CaveLayout, Chamber};
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
    let field = rock_field(layout);
    let blend = cfg.blend;
    // Sink features this far into the surface so the bumpy (roughened) rock
    // never leaves a visible gap under them.
    let embed = 0.2 + cfg.roughness * 0.3;
    // When the shell is cut away for inspection, drop features that would land
    // in the removed half (they'd otherwise float in the opened section).
    let cut_z = layout.block_center.z;

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
            let (x, z) = pick_xz(&field, blend, c, &mut state);

            // Surface y from the real field, with a geometric fallback.
            let ceiling = group.kind == DecoKind::Stalactite;
            let surf = if ceiling {
                surface_y(&field, blend, x, z, c.center.y, 0.12, march_limit(c, blend))
                    .unwrap_or_else(|| c.ceiling_y())
            } else {
                surface_y(&field, blend, x, z, c.center.y, -0.12, march_limit(c, blend))
                    .unwrap_or_else(|| c.floor_y())
            };
            let anchor_y = match group.kind {
                DecoKind::Stalactite => surf + embed,
                DecoKind::Pool | DecoKind::Lake => surf + 0.04,
                _ => surf - embed,
            };
            let anchor = Vec3::new(x, anchor_y, z);

            let (name, mesh, transform) = match group.kind {
                DecoKind::Stalagmite => stalagmite(anchor, size, &mut state, n),
                DecoKind::Stalactite => stalactite(anchor, size, &mut state, n),
                DecoKind::RockPile => rock_pile(anchor, size, &mut state, n),
                DecoKind::Pool => water(anchor, size.min(c.floor_radius() * 0.9), &mut state, n, false),
                DecoKind::Lake => water(anchor, size.min(c.floor_radius() * 1.6), &mut state, n, true),
            };

            // Cull features hidden by the debug cutaway (still drawn from the
            // rng stream above so placement is identical with the shell shown).
            if cfg.debug_hide_shell && z >= cut_z {
                continue;
            }

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

/// Vertical march distance: enough to cross the chamber plus its blend skirt.
fn march_limit(c: &Chamber, blend: f32) -> f32 {
    c.half.y * 1.5 + blend + 2.0
}

/// Pick an XZ on the chamber's walkable disc that is genuinely inside the
/// carved void at mid-height (so the vertical march finds a real surface).
/// Falls back to the chamber centre, which is always inside.
fn pick_xz(field: &[BlobChild], blend: f32, c: &Chamber, state: &mut u32) -> (f32, f32) {
    for _ in 0..5 {
        let ang = rand_in(state, 0.0, TAU);
        let rr = c.floor_radius() * rand_f01(state).sqrt();
        let x = c.center.x + rr * ang.cos();
        let z = c.center.z + rr * ang.sin();
        if evaluate_field(field, Vec3::new(x, c.center.y, z), blend) > 0.0 {
            return (x, z);
        }
    }
    (c.center.x, c.center.z)
}

/// March from `(x, y0, z)` — assumed inside the void — by `step` (signed) until
/// the field crosses into rock, then bisect for the surface y. `step < 0`
/// finds the floor, `step > 0` the ceiling. Returns `None` if no crossing is
/// found within `limit`.
fn surface_y(
    field: &[BlobChild],
    blend: f32,
    x: f32,
    z: f32,
    y0: f32,
    step: f32,
    limit: f32,
) -> Option<f32> {
    let inside = |yy: f32| evaluate_field(field, Vec3::new(x, yy, z), blend) > 0.0;
    if !inside(y0) {
        return None;
    }
    let mut prev = y0;
    let mut y = y0;
    let mut traveled = 0.0;
    while traveled < limit {
        y += step;
        traveled += step.abs();
        if !inside(y) {
            let (mut a, mut b) = (prev, y); // a inside, b rock
            for _ in 0..14 {
                let m = 0.5 * (a + b);
                if inside(m) {
                    a = m;
                } else {
                    b = m;
                }
            }
            return Some(0.5 * (a + b));
        }
        prev = y;
    }
    None
}

fn stalagmite(anchor: Vec3, size: f32, state: &mut u32, n: u32) -> (String, Mesh, Transform) {
    let height = size * 2.0;
    let base_r = size * 0.45;
    let mut mesh = cone_mesh(base_r, height, 8, UvMode::Tile);
    jitter(&mut mesh, 0.12, sub_seed(*state, n), None);
    // Cone base sits at -height/2; lift so the base rests at the anchor.
    let pos = anchor + Vec3::new(0.0, height * 0.5, 0.0);
    let yaw = rand_in(state, 0.0, TAU);
    (
        format!("stalagmite_{n}"),
        mesh,
        Transform::from_trs(pos, Quat::from_rotation_y(yaw), Vec3::ONE),
    )
}

fn stalactite(anchor: Vec3, size: f32, state: &mut u32, n: u32) -> (String, Mesh, Transform) {
    let height = size * 2.0;
    let base_r = size * 0.4;
    let mut mesh = cone_mesh(base_r, height, 8, UvMode::Tile);
    jitter(&mut mesh, 0.12, sub_seed(*state, n), None);
    // Flip so the apex points down; base sits flush against the ceiling anchor.
    let pos = anchor - Vec3::new(0.0, height * 0.5, 0.0);
    let flip =
        Quat::from_rotation_x(std::f32::consts::PI) * Quat::from_rotation_y(rand_in(state, 0.0, TAU));
    (
        format!("stalactite_{n}"),
        mesh,
        Transform::from_trs(pos, flip, Vec3::ONE),
    )
}

fn rock_pile(anchor: Vec3, size: f32, state: &mut u32, n: u32) -> (String, Mesh, Transform) {
    let count = 3 + rand_range(state, 4); // 3..6 boulders
    let mut acc = Mesh::default();
    for k in 0..count {
        let r = size * rand_in(state, 0.35, 0.7);
        let ox = rand_in(state, -size, size);
        let oz = rand_in(state, -size, size);
        // Rest each boulder on the pile base (its bottom near anchor.y).
        let oy = r * 0.55;
        let mut boulder = icosphere_mesh(r, 1, UvMode::Tile);
        jitter(&mut boulder, 0.18, sub_seed(*state, n * 17 + k), None);
        let placed = transform_mesh(&boulder, glam::Mat4::from_translation(Vec3::new(ox, oy, oz)));
        append_mesh(&mut acc, &placed);
    }
    (format!("rock_pile_{n}"), acc, Transform::from_translation(anchor))
}

fn water(anchor: Vec3, radius: f32, state: &mut u32, n: u32, lake: bool) -> (String, Mesh, Transform) {
    let radius = radius.max(0.4);
    let mesh = disc_mesh(radius, 24, UvMode::Tile);
    let pos = anchor
        + Vec3::new(
            rand_in(state, -0.2, 0.2),
            0.0,
            rand_in(state, -0.2, 0.2),
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

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Mat4;
    use mogen_geom::{SdfOp, SdfPrim};

    /// A box hollowed by one ellipsoid, mirroring `rock_field`'s shape.
    fn field() -> Vec<BlobChild> {
        vec![
            BlobChild::new(
                SdfPrim::Box { half: Vec3::new(10.0, 6.0, 10.0) },
                SdfOp::Add,
                Mat4::from_translation(Vec3::new(0.0, 6.0, 0.0)),
            ),
            BlobChild::new(
                SdfPrim::Ellipsoid { half: Vec3::new(3.0, 2.0, 3.0) },
                SdfOp::Subtract,
                Mat4::from_translation(Vec3::new(0.0, 5.0, 0.0)),
            ),
        ]
    }

    #[test]
    fn surface_y_lands_on_the_carved_floor() {
        let f = field();
        // March down from the chamber centre (inside the void) to the floor.
        let y = surface_y(&f, 0.0, 0.0, 0.0, 5.0, -0.1, 8.0).expect("floor found");
        // Ellipsoid centred at y=5, half-height 2 → floor at y≈3.
        assert!((y - 3.0).abs() < 0.2, "floor y={y}, expected ≈3");
        // The surface really separates void (above) from rock (below).
        assert!(evaluate_field(&f, Vec3::new(0.0, y + 0.3, 0.0), 0.0) > 0.0);
        assert!(evaluate_field(&f, Vec3::new(0.0, y - 0.3, 0.0), 0.0) < 0.0);
    }

    #[test]
    fn surface_y_finds_the_ceiling_marching_up() {
        let f = field();
        let y = surface_y(&f, 0.0, 0.0, 0.0, 5.0, 0.1, 8.0).expect("ceiling found");
        assert!((y - 7.0).abs() < 0.2, "ceiling y={y}, expected ≈7");
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
