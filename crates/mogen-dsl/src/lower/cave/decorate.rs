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

use std::f32::consts::{PI, TAU};

use glam::{Mat4, Quat, Vec3};

use mogen_core::{Mesh, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{
    cone_mesh, evaluate_field, icosphere_mesh, jitter, recompute_normals, transform_mesh,
    weld_vertices, BlobChild,
};

use crate::ast::Node;

use super::config::{CaveCfg, DecoGroup, DecoKind};
use super::generate::{rock_field, CaveLayout, Chamber};
use super::materials::{ROCK_MAT, WATER_MAT};
use super::rng::{rand_f01, rand_in, rand_range, sub_seed};

/// Scatter the configured decorations and return, for every stone column
/// placed, a ground spot just outside its footprint, so the POI pass can mark
/// each column base without burying the marker inside the pillar mesh.
pub(super) fn emit_decorations(
    node: &Node,
    cfg: &CaveCfg,
    layout: &CaveLayout,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Vec<Vec3> {
    let mut column_bases: Vec<Vec3> = Vec::new();
    if cfg.decorations.is_empty() || layout.chambers.is_empty() {
        return column_bases;
    }
    let origin = node.origin.clone();
    let field = rock_field(layout);
    let blend = cfg.blend;
    let lod = cfg.lod_scale;
    // Sink features this far into the surface so the bumpy (roughened) rock
    // never leaves a visible gap under them.
    let embed = 0.2 + cfg.roughness * 0.3;

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
            let limit = march_limit(c, blend);

            let built: Option<(String, Mesh, Transform)> = match group.kind {
                // Pools/lakes fill the chamber bowl as a basin (see `basin_water`),
                // so they query the floor themselves rather than placing at a
                // single sampled XZ.
                DecoKind::Pool => Some(basin_water(&field, blend, c, size, &mut state, n, false, lod)),
                DecoKind::Lake => Some(basin_water(&field, blend, c, size, &mut state, n, true, lod)),
                // A column needs both a floor and a ceiling to span; when the
                // sampled spot has no reachable ceiling (an open passage mouth,
                // say) the column is skipped rather than left dangling.
                DecoKind::Column => match column(&field, blend, c, size, embed, &mut state, n, lod) {
                    Some((res, base)) => {
                        column_bases.push(base);
                        Some(res)
                    }
                    None => None,
                },
                _ => {
                    let (x, z) = pick_xz(&field, blend, c, &mut state);
                    // Surface y from the real field, with a geometric fallback.
                    let ceiling = group.kind == DecoKind::Stalactite;
                    let surf = if ceiling {
                        surface_y(&field, blend, x, z, c.center.y, 0.12, limit)
                            .unwrap_or_else(|| c.ceiling_y())
                    } else {
                        surface_y(&field, blend, x, z, c.center.y, -0.12, limit)
                            .unwrap_or_else(|| c.floor_y())
                    };
                    let anchor_y = if ceiling { surf + embed } else { surf - embed };
                    let anchor = Vec3::new(x, anchor_y, z);
                    Some(match group.kind {
                        DecoKind::Stalagmite => stalagmite(anchor, size, &mut state, n, lod),
                        DecoKind::Stalactite => stalactite(anchor, size, &mut state, n, lod),
                        DecoKind::RockPile => rock_pile(anchor, size, &mut state, n, lod),
                        DecoKind::Pool | DecoKind::Lake | DecoKind::Column => unreachable!(),
                    })
                }
            };

            let (name, mesh, transform) = match built {
                Some(v) => v,
                None => continue,
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
    column_bases
}

/// Scale a base segment/ring count by `lod_scale`, never below the minimum a
/// primitive needs to stay closed (3). Lower `lod_scale` ⇒ coarser meshes.
pub(super) fn lod_segs(base: u32, lod: f32) -> u32 {
    ((base as f32 * lod).round() as u32).max(3)
}

/// Vertical march distance: enough to cross the chamber plus its blend skirt.
pub(super) fn march_limit(c: &Chamber, blend: f32) -> f32 {
    c.half.y * 1.5 + blend + 2.0
}

/// Pick an XZ on the chamber's walkable disc that is genuinely inside the
/// carved void at mid-height (so the vertical march finds a real surface).
/// Falls back to the chamber centre, which is always inside.
pub(super) fn pick_xz(field: &[BlobChild], blend: f32, c: &Chamber, state: &mut u32) -> (f32, f32) {
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
pub(super) fn surface_y(
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

/// A cone whose coincident vertices (the apex ring, and the rim shared between
/// the side wall and the bottom cap) are welded into single vertices *before*
/// jittering. `jitter` perturbs each vertex independently along its own normal,
/// so on a raw `cone_mesh` the duplicated apex/rim vertices fly apart and tear
/// the surface open. Welding first keeps them moving as one; recomputing
/// normals afterwards restores correct shading for the displaced surface.
fn jittered_cone(base_r: f32, height: f32, segs: u32, amount: f32, seed: u32) -> Mesh {
    let mut mesh = weld_vertices(&cone_mesh(base_r, height, segs, UvMode::Tile), 1e-4);
    jitter(&mut mesh, amount, seed, None);
    recompute_normals(&mesh)
}

fn stalagmite(anchor: Vec3, size: f32, state: &mut u32, n: u32, lod: f32) -> (String, Mesh, Transform) {
    let height = size * 2.0;
    let base_r = size * 0.45;
    let mesh = jittered_cone(base_r, height, lod_segs(8, lod), 0.12, sub_seed(*state, n));
    // Cone base sits at -height/2; lift so the base rests at the anchor.
    let pos = anchor + Vec3::new(0.0, height * 0.5, 0.0);
    let yaw = rand_in(state, 0.0, TAU);
    (
        format!("stalagmite_{n}"),
        mesh,
        Transform::from_trs(pos, Quat::from_rotation_y(yaw), Vec3::ONE),
    )
}

fn stalactite(anchor: Vec3, size: f32, state: &mut u32, n: u32, lod: f32) -> (String, Mesh, Transform) {
    let height = size * 2.0;
    let base_r = size * 0.4;
    let mesh = jittered_cone(base_r, height, lod_segs(8, lod), 0.12, sub_seed(*state, n));
    // Flip so the apex points down; base sits flush against the ceiling anchor.
    let pos = anchor - Vec3::new(0.0, height * 0.5, 0.0);
    let flip = Quat::from_rotation_x(PI) * Quat::from_rotation_y(rand_in(state, 0.0, TAU));
    (
        format!("stalactite_{n}"),
        mesh,
        Transform::from_trs(pos, flip, Vec3::ONE),
    )
}

fn rock_pile(anchor: Vec3, size: f32, state: &mut u32, n: u32, lod: f32) -> (String, Mesh, Transform) {
    let count = 3 + rand_range(state, 4); // 3..6 boulders
    // Drop a subdivision level on low-detail bakes — an icosphere at subdiv 0
    // is already a 20-tri rock, plenty for a background boulder.
    let subdiv = if lod < 0.5 { 0 } else { 1 };
    let mut acc = Mesh::default();
    for k in 0..count {
        let r = size * rand_in(state, 0.35, 0.7);
        let ox = rand_in(state, -size, size);
        let oz = rand_in(state, -size, size);
        // Rest each boulder on the pile base (its bottom near anchor.y).
        let oy = r * 0.55;
        let mut boulder = icosphere_mesh(r, subdiv, UvMode::Tile);
        jitter(&mut boulder, 0.18, sub_seed(*state, n * 17 + k), None);
        let placed = transform_mesh(&boulder, Mat4::from_translation(Vec3::new(ox, oy, oz)));
        append_mesh(&mut acc, &placed);
    }
    (format!("rock_pile_{n}"), acc, Transform::from_translation(anchor))
}

/// Build a floor-to-ceiling stone column: a stalagmite rising from the floor
/// fused with a stalactite descending from the ceiling, overlapping at a waist
/// in the middle so the pair reads as one continuous pillar. Returns the mesh +
/// transform and a ground spot just outside the footprint (for the
/// `column_base` point of interest), or `None` when the sampled spot has no
/// reachable floor *and* ceiling to span — a column with only one end would
/// float.
#[allow(clippy::too_many_arguments)]
fn column(
    field: &[BlobChild],
    blend: f32,
    c: &Chamber,
    size: f32,
    embed: f32,
    state: &mut u32,
    n: u32,
    lod: f32,
) -> Option<((String, Mesh, Transform), Vec3)> {
    let (x, z) = pick_xz(field, blend, c, state);
    let limit = march_limit(c, blend);
    let floor = surface_y(field, blend, x, z, c.center.y, -0.12, limit)?;
    let ceil = surface_y(field, blend, x, z, c.center.y, 0.12, limit)?;
    let h = ceil - floor;
    if h < 1.0 {
        return None; // too short to read as a column
    }

    // Base radius is the requested size, capped so a tall column stays slender.
    let base_r = size.min(h * 0.16).max(0.12);
    let segs = lod_segs(8, lod);
    // Each cone climbs past the mid-height so the two surfaces cross at a waist
    // rather than meeting at a hairline; `embed` sinks each end into the rock.
    let cone_h = h * 0.62 + embed;

    // Lower half: stalagmite-like, base resting on the floor (sunk by `embed`).
    let lower = jittered_cone(base_r, cone_h, segs, 0.1, sub_seed(*state, n * 31 + 1));
    let lower = transform_mesh(&lower, Mat4::from_translation(Vec3::new(0.0, cone_h * 0.5 - embed, 0.0)));

    // Upper half: stalactite-like, flipped apex-down with its base on the
    // ceiling (raised by `embed`). Slightly thinner so the waist tapers.
    let upper = jittered_cone(base_r * 0.9, cone_h, segs, 0.1, sub_seed(*state, n * 31 + 2));
    let upper = transform_mesh(
        &upper,
        Mat4::from_translation(Vec3::new(0.0, h + embed - cone_h * 0.5, 0.0)) * Mat4::from_rotation_x(PI),
    );

    let mut acc = lower;
    append_mesh(&mut acc, &upper);

    let base = Vec3::new(x, floor, z);

    // POI marker spot: on the ground just outside the pillar footprint, not
    // buried inside the column mesh. Try a few seeded directions one base-radius
    // (plus a small gap) out; accept the first that lands in open floor. Fall
    // back to the base centre if every direction hits rock (column against a
    // wall) so a marker is still emitted.
    let clearance = base_r + 0.35;
    let mut marker = base;
    for _ in 0..6 {
        let ang = rand_in(state, 0.0, TAU);
        let mx = x + clearance * ang.cos();
        let mz = z + clearance * ang.sin();
        if evaluate_field(field, Vec3::new(mx, c.center.y, mz), blend) <= 0.0 {
            continue; // into rock — try another direction
        }
        if let Some(my) = surface_y(field, blend, mx, mz, c.center.y, -0.12, limit) {
            marker = Vec3::new(mx, my, mz);
            break;
        }
    }

    Some((
        (format!("column_{n}"), acc, Transform::from_translation(base)),
        marker,
    ))
}

/// Build a pool/lake as a **basin fill** rather than a floating disc.
///
/// A flat disc at a single floor height reads wrong: the carved floor is a
/// curved, roughened bowl, so a flat sheet hovers over the lower spots and
/// stops short of the rising walls — water with daylight under its rim. Instead
/// we pick a water level a little above the chamber's deepest floor point, then
/// cast rays outward to the **shoreline** — the radius at which the floor climbs
/// back up to that level (or a wall blocks it). The surface is a fan whose rim
/// sits on that shoreline, so the water meets the rock all the way round and
/// fully covers the depression beneath it: an enclosed water space.
#[allow(clippy::too_many_arguments)]
fn basin_water(
    field: &[BlobChild],
    blend: f32,
    c: &Chamber,
    size: f32,
    state: &mut u32,
    n: u32,
    lake: bool,
    lod: f32,
) -> (String, Mesh, Transform) {
    let limit = march_limit(c, blend);
    // Centre the basin near the chamber centre — the lowest point of the oblate
    // floor bowl — with a small jitter so two pools in one chamber don't stack.
    let jit = c.floor_radius() * 0.25;
    let cx = c.center.x + rand_in(state, -jit, jit);
    let cz = c.center.z + rand_in(state, -jit, jit);

    // Deepest floor under the basin centre, and the water level above it. Depth
    // scales with the requested size but is capped to the chamber so the pool
    // never rises above the room.
    let floor_c = surface_y(field, blend, cx, cz, c.center.y, -0.12, limit)
        .unwrap_or_else(|| c.floor_y());
    let max_depth = (c.half.y * 0.7).max(0.3);
    let depth = (0.15 + size * 0.22).clamp(0.15, max_depth);
    let wy = floor_c + depth;

    // Reach + tessellation. `max_r` is only a safety bound: in practice each ray
    // stops earlier at the shoreline or a wall (`shore_radius`). It must be
    // generous enough to let the shoreline be found — the floor can rise to `wy`
    // well past `floor_radius` — so it keys off the chamber's full footprint,
    // wider for lakes since they may spill through passages into neighbours.
    let span = c.half.x.max(c.half.z);
    let max_r = if lake { span * 2.2 + blend } else { span * 1.3 + blend };
    let segs: u32 = lod_segs(if lake { 48 } else { 32 }, lod);

    let mut rim: Vec<[f32; 3]> = Vec::with_capacity(segs as usize);
    for s in 0..segs {
        let ang = TAU * (s as f32 / segs as f32);
        let (dx, dz) = (ang.cos(), ang.sin());
        let r = shore_radius(field, blend, c, cx, cz, dx, dz, wy, max_r, limit);
        rim.push([dx * r, 0.0, dz * r]);
    }

    let label = if lake { "lake" } else { "pool" };
    (
        format!("{label}_{n}"),
        water_fan(&rim),
        Transform::from_translation(Vec3::new(cx, wy, cz)),
    )
}

/// March outward along `(dx, dz)` from the basin centre to the shoreline: the
/// first radius at which the carved floor has risen to the water level `wy`, or
/// solid rock stands at that level (a wall). Returns the capped reach if open
/// water would extend past `max_r`.
#[allow(clippy::too_many_arguments)]
fn shore_radius(
    field: &[BlobChild],
    blend: f32,
    c: &Chamber,
    cx: f32,
    cz: f32,
    dx: f32,
    dz: f32,
    wy: f32,
    max_r: f32,
    limit: f32,
) -> f32 {
    // The waterline is blocked once the floor has surfaced above `wy` (shore) or
    // rock stands at the water level (wall).
    let blocked = |r: f32| -> bool {
        let x = cx + dx * r;
        let z = cz + dz * r;
        if evaluate_field(field, Vec3::new(x, wy, z), blend) <= 0.0 {
            return true; // wall at the waterline
        }
        match surface_y(field, blend, x, z, c.center.y, -0.12, limit) {
            Some(fy) => fy >= wy,
            None => true, // no reachable floor here → treat as shore
        }
    };

    let step = 0.15;
    let mut prev = 0.0;
    let mut r = step;
    while r <= max_r {
        if blocked(r) {
            let (mut a, mut b) = (prev, r); // a open water, b blocked
            for _ in 0..12 {
                let m = 0.5 * (a + b);
                if blocked(m) {
                    b = m;
                } else {
                    a = m;
                }
            }
            return 0.5 * (a + b);
        }
        prev = r;
        r += step;
    }
    max_r
}

/// Triangle fan from a centre vertex out to a ring of `rim` points (local space,
/// all on y=0). Faces +Y with the same winding as `disc_mesh`, so the water
/// sheet is visible from above. The rim radii vary per angle so the edge traces
/// the shoreline rather than a circle.
fn water_fan(rim: &[[f32; 3]]) -> Mesh {
    let up = [0.0, 1.0, 0.0];
    let segs = rim.len();
    let mut positions = Vec::with_capacity(segs + 1);
    let mut normals = Vec::with_capacity(segs + 1);
    let mut uvs = Vec::with_capacity(segs + 1);
    let mut indices = Vec::with_capacity(segs * 3);

    positions.push([0.0, 0.0, 0.0]);
    normals.push(up);
    uvs.push([0.5, 0.5]);

    let max_r = rim
        .iter()
        .map(|p| (p[0] * p[0] + p[2] * p[2]).sqrt())
        .fold(0.0_f32, f32::max)
        .max(1e-3);
    for p in rim {
        positions.push(*p);
        normals.push(up);
        uvs.push([0.5 + 0.5 * p[0] / max_r, 0.5 + 0.5 * p[2] / max_r]);
    }
    for i in 0..segs {
        let this = 1 + i as u32;
        let next = 1 + ((i + 1) % segs) as u32;
        // centre → next → this (matches disc_mesh's +Y winding).
        indices.extend_from_slice(&[0, next, this]);
    }
    Mesh { positions, normals, uvs, indices, ..Default::default() }
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

    /// A chamber matching `field()`'s carved ellipsoid (centre y=5, half 3/2/3,
    /// floor ≈3, ceiling ≈7).
    fn chamber() -> Chamber {
        Chamber {
            center: Vec3::new(0.0, 5.0, 0.0),
            half: Vec3::new(3.0, 2.0, 3.0),
            rot: glam::Quat::IDENTITY,
            level: 0,
        }
    }

    #[test]
    fn shore_radius_lands_where_the_floor_meets_the_waterline() {
        let f = field();
        let c = chamber();
        // Water level 0.5 above the deepest floor (y≈3) → wy≈3.5.
        let wy = 3.5;
        let limit = march_limit(&c, 0.0);
        let r = shore_radius(&f, 0.0, &c, 0.0, 0.0, 1.0, 0.0, wy, 8.0, limit);
        // Ellipsoid floor: y = 5 - 2·√(1-(r/3)²). Solving y=3.5 → r≈1.98.
        assert!((r - 1.98).abs() < 0.2, "shoreline r={r}, expected ≈1.98");
        // At the rim the carved floor really sits at the waterline (no daylight
        // under the edge): floor(r) ≈ wy.
        let floor_at_rim = surface_y(&f, 0.0, r, 0.0, c.center.y, -0.12, limit).unwrap();
        assert!(
            (floor_at_rim - wy).abs() < 0.15,
            "rim floats: floor {floor_at_rim} vs waterline {wy}"
        );
    }

    #[test]
    fn shore_radius_stops_at_a_wall_when_the_floor_stays_low() {
        // A flat-bottomed box void with rock both below and around it: the floor
        // never rises, so the shoreline can only be the side wall (x=4). The void
        // sits at y∈[1,3] so there is real rock floor beneath at y≈1.
        let f = vec![
            BlobChild::new(
                SdfPrim::Box { half: Vec3::new(10.0, 6.0, 10.0) },
                SdfOp::Add,
                Mat4::from_translation(Vec3::new(0.0, 6.0, 0.0)),
            ),
            BlobChild::new(
                SdfPrim::Box { half: Vec3::new(4.0, 1.0, 4.0) },
                SdfOp::Subtract,
                Mat4::from_translation(Vec3::new(0.0, 2.0, 0.0)),
            ),
        ];
        let c = Chamber { center: Vec3::new(0.0, 2.0, 0.0), half: Vec3::new(4.0, 1.0, 4.0), rot: glam::Quat::IDENTITY, level: 0 };
        let wy = 1.5; // 0.5 above the flat floor at y≈1
        let limit = march_limit(&c, 0.0);
        let r = shore_radius(&f, 0.0, &c, 0.0, 0.0, 1.0, 0.0, wy, 20.0, limit);
        // The carved void wall is at x=4; the water meets it there, not beyond.
        assert!((r - 4.0).abs() < 0.3, "shoreline should hit the wall at x≈4, got {r}");
    }

    #[test]
    fn water_fan_covers_the_floor_and_meets_the_rock() {
        let f = field();
        let c = chamber();
        let mut state = 12345;
        let (name, mesh, xform) = basin_water(&f, 0.0, &c, 1.5, &mut state, 0, false, 1.0);
        assert!(name.starts_with("pool_"));
        assert!(mesh.indices.len() >= 3 * 32, "fan should be fully tessellated");
        // The surface sits above the deepest floor (covers the depression)…
        let wy = xform.translation.y;
        let floor_c = surface_y(&f, 0.0, xform.translation.x, xform.translation.z, c.center.y, -0.12, march_limit(&c, 0.0)).unwrap();
        assert!(wy > floor_c, "water level {wy} should sit above floor {floor_c}");
        // …and every rim vertex meets the rock at the waterline: the floor under
        // it is at (or above) the surface, never below it with daylight beneath.
        for p in mesh.positions.iter().skip(1) {
            let (x, z) = (p[0] + xform.translation.x, p[2] + xform.translation.z);
            match surface_y(&f, 0.0, x, z, c.center.y, -0.12, march_limit(&c, 0.0)) {
                Some(fy) => assert!(
                    fy >= wy - 0.2,
                    "rim vertex floats: floor {fy} well below waterline {wy}"
                ),
                None => {} // rim met a wall — fine
            }
        }
    }

    #[test]
    fn jittered_cone_stays_watertight() {
        // Regression: `jitter` perturbs each vertex independently, so on a raw
        // `cone_mesh` the duplicated apex ring and shared rim tore apart into a
        // gappy star. `jittered_cone` welds first, so the displaced cone must
        // still be a closed 2-manifold — no holes at the tip or base seam.
        for seed in [1u32, 7, 42, 1000] {
            let m = jittered_cone(0.4, 2.0, 8, 0.12, seed);
            assert!(
                mogen_geom::is_closed_manifold(&m),
                "jittered cone (seed {seed}) tore open"
            );
        }
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
