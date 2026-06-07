//! Terrain carving: `hole` voids punched into the baked mesh and `road`
//! corridors flattened into the height field.
//!
//! Both read the `terrain` node's child declarations (the wrapper consumes its
//! own children, so these never reach the generic lowering path). They sit at
//! opposite ends of the pipeline:
//!
//! - **roads** mutate the `HeightField` *before* emit, as one more retouch pass
//!   (like `smooth`/`terrace`). Levelling the field rather than the mesh keeps
//!   the chunk emitter's crack-free seams and LOD skirts working untouched, and
//!   lets the road follow a smoothed longitudinal profile at the terrain's own
//!   height (no trench). The flattened cells are recorded in `field.road_mask`
//!   so emit can tint COLOR_0 toward a dirt/gravel surface colour.
//! - **holes** are applied *during* emit (see `emit.rs`): cells whose centre
//!   falls inside a footprint are dropped, the rim is walled down to a floor
//!   depth, and `cap="floor"` adds a flat floor quad to seal the pit.

use glam::Vec2;

use crate::ast::Node;

use super::config::TerrainCfg;
use super::field::HeightField;

/// A 2-D footprint in world XZ for a `hole`.
#[derive(Clone, Copy)]
pub(super) enum Footprint {
    Circle { c: Vec2, r: f32 },
    Rect { c: Vec2, half: Vec2 },
}

impl Footprint {
    /// Is world point `(x, z)` inside the footprint?
    pub fn contains(&self, x: f32, z: f32) -> bool {
        match *self {
            Footprint::Circle { c, r } => {
                let dx = x - c.x;
                let dz = z - c.y;
                dx * dx + dz * dz <= r * r
            }
            Footprint::Rect { c, half } => {
                (x - c.x).abs() <= half.x && (z - c.y).abs() <= half.y
            }
        }
    }

    /// World-XZ axis-aligned bounds `(min, max)`.
    fn bounds(&self) -> (Vec2, Vec2) {
        match *self {
            Footprint::Circle { c, r } => (c - Vec2::splat(r), c + Vec2::splat(r)),
            Footprint::Rect { c, half } => (c - half, c + half),
        }
    }
}

/// Whether a `hole` is left open at the bottom (a basement/cave mouth plugs it)
/// or sealed with a flat floor quad.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HoleCap {
    Open,
    Floor,
}

/// A void punched into the terrain mesh. `floor_y` is the world Y the rim walls
/// drop to (and the floor cap sits at, for `HoleCap::Floor`).
pub(super) struct Hole {
    pub footprint: Footprint,
    pub cap: HoleCap,
    pub floor_y: f32,
}

/// A corridor flattened into the height field along a waypoint polyline.
pub(super) struct Road {
    /// Centreline waypoints in world XZ.
    pub pts: Vec<Vec2>,
    /// Half the carved corridor width (fully flattened band).
    pub half_width: f32,
    /// Blend band beyond `half_width` over which the corridor eases back into the
    /// natural terrain.
    pub shoulder: f32,
}

/// Read every `hole` child of the `terrain` node into world-space footprints.
/// `field` is sampled to anchor each pit's floor below the lowest rim point.
pub(super) fn read_holes(node: &Node, cfg: &TerrainCfg, field: &HeightField) -> Vec<Hole> {
    let amp = cfg.size[1];
    node.children
        .iter()
        .filter(|c| c.kind == "hole")
        .filter_map(|c| {
            let at = c.attr_pair("at").unwrap_or([0.0, 0.0]);
            let c_xz = Vec2::new(at[0], at[1]);
            let footprint = if let Some(r) = c.attr_number("radius") {
                Footprint::Circle { c: c_xz, r: r.max(0.0) }
            } else if let Some(s) = c.attr_pair("size") {
                Footprint::Rect {
                    c: c_xz,
                    half: Vec2::new(s[0].max(0.0) * 0.5, s[1].max(0.0) * 0.5),
                }
            } else {
                // No footprint shape — nothing to carve.
                return None;
            };
            let depth = c.attr_number("depth").unwrap_or(4.0).max(0.0);
            let cap = match c.attr_string("cap") {
                Some("floor") => HoleCap::Floor,
                _ => HoleCap::Open,
            };
            // Floor sits `depth` below the lowest surface height inside the
            // footprint, so the rim walls never poke back above ground.
            let min_h = min_surface_height(field, cfg, &footprint).unwrap_or(0.0);
            let floor_y = min_h * amp - depth;
            Some(Hole {
                footprint,
                cap,
                floor_y,
            })
        })
        .collect()
}

/// Smallest normalised surface height over the grid samples inside `footprint`.
/// `None` when the footprint covers no grid sample (a sub-cell hole) — the
/// caller then falls back to `0`.
fn min_surface_height(field: &HeightField, cfg: &TerrainCfg, footprint: &Footprint) -> Option<f32> {
    let n = field.n;
    let segs = (n - 1) as f32;
    let (w, d) = (cfg.size[0], cfg.size[2]);
    let (half_w, half_d) = (w * 0.5, d * 0.5);
    let world_x = |i: usize| -half_w + (i as f32 / segs) * w;
    let world_z = |j: usize| -half_d + (j as f32 / segs) * d;

    let (mn, mx) = footprint.bounds();
    let (i0, i1, j0, j1) = grid_range(mn, mx, half_w, half_d, w, d, segs, n);

    let mut min_h: Option<f32> = None;
    for j in j0..=j1 {
        let z = world_z(j);
        for i in i0..=i1 {
            let x = world_x(i);
            if footprint.contains(x, z) {
                let h = field.at(i, j);
                min_h = Some(min_h.map_or(h, |m: f32| m.min(h)));
            }
        }
    }
    min_h
}

/// Read every `road` child of the `terrain` node.
pub(super) fn read_roads(node: &Node) -> Vec<Road> {
    node.children
        .iter()
        .filter(|c| c.kind == "road")
        .filter_map(|c| {
            let pts: Vec<Vec2> = c
                .attr_list_pair("path")?
                .into_iter()
                .map(|p| Vec2::new(p[0], p[1]))
                .collect();
            if pts.len() < 2 {
                return None;
            }
            let half_width = (c.attr_number("width").unwrap_or(3.0).max(0.0)) * 0.5;
            let shoulder = c.attr_number("shoulder").unwrap_or(half_width).max(0.0);
            Some(Road {
                pts,
                half_width,
                shoulder,
            })
        })
        .collect()
}

/// Flatten each road corridor into the field and record its intensity mask.
/// Within `half_width` of the centreline the height is forced to the road's
/// smoothed longitudinal profile; across the `shoulder` it eases back to the
/// natural terrain (smoothstep). The same blend weight is written to
/// `field.road_mask` (max-combined where roads overlap).
pub(super) fn carve_roads(field: &mut HeightField, cfg: &TerrainCfg, roads: &[Road]) {
    if roads.is_empty() {
        return;
    }
    let n = field.n;
    let segs = (n - 1) as f32;
    let (w, d) = (cfg.size[0], cfg.size[2]);
    let (half_w, half_d) = (w * 0.5, d * 0.5);
    let world_x = |i: usize| -half_w + (i as f32 / segs) * w;
    let world_z = |j: usize| -half_d + (j as f32 / segs) * d;
    let cell = (w / segs).min(d / segs).max(1e-3);

    for road in roads {
        if road.pts.len() < 2 {
            continue;
        }
        let reach = road.half_width + road.shoulder;

        // Dense centreline (≈ half a cell apart) carrying the smoothed profile;
        // distance + target height both come from its segments.
        let cl = densify(&road.pts, cell * 0.5);
        let mut prof: Vec<f32> = cl
            .iter()
            .map(|p| sample_height_norm(field, p.x, p.y, half_w, half_d, w, d))
            .collect();
        // Smooth the profile so the road follows the broad lie of the land
        // rather than every bump it crosses.
        let radius = (cl.len() / 6).clamp(2, 24);
        smooth_profile(&mut prof, radius);

        let (mut mn, mut mx) = (
            Vec2::new(f32::MAX, f32::MAX),
            Vec2::new(f32::MIN, f32::MIN),
        );
        for p in &road.pts {
            mn = mn.min(*p);
            mx = mx.max(*p);
        }
        mn -= Vec2::splat(reach);
        mx += Vec2::splat(reach);
        let (i0, i1, j0, j1) = grid_range(mn, mx, half_w, half_d, w, d, segs, n);

        for j in j0..=j1 {
            let z = world_z(j);
            for i in i0..=i1 {
                let x = world_x(i);
                let (dist, k, t) = nearest_seg(&cl, Vec2::new(x, z));
                if dist > reach {
                    continue;
                }
                let target = prof[k] * (1.0 - t) + prof[(k + 1).min(prof.len() - 1)] * t;
                let blend = 1.0 - smoothstep(road.half_width, reach, dist);
                let idx = j * n + i;
                field.h[idx] = field.h[idx] * (1.0 - blend) + target * blend;
                field.road_mask[idx] = field.road_mask[idx].max(blend);
            }
        }
    }
}

/// Clamp a world-XZ bounds box to inclusive grid index ranges `(i0, i1, j0, j1)`.
fn grid_range(
    mn: Vec2,
    mx: Vec2,
    half_w: f32,
    half_d: f32,
    w: f32,
    d: f32,
    segs: f32,
    n: usize,
) -> (usize, usize, usize, usize) {
    let to_i = |x: f32| (((x + half_w) / w) * segs).floor().clamp(0.0, segs) as usize;
    let to_j = |z: f32| (((z + half_d) / d) * segs).floor().clamp(0.0, segs) as usize;
    let i0 = to_i(mn.x);
    let i1 = (to_i(mx.x) + 1).min(n - 1);
    let j0 = to_j(mn.y);
    let j1 = (to_j(mx.y) + 1).min(n - 1);
    (i0, i1, j0, j1)
}

/// Subdivide a polyline so no segment exceeds `spacing`.
fn densify(pts: &[Vec2], spacing: f32) -> Vec<Vec2> {
    let mut out = vec![pts[0]];
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = (b - a).length();
        let steps = (len / spacing.max(1e-3)).ceil().max(1.0) as usize;
        for s in 1..=steps {
            out.push(a.lerp(b, s as f32 / steps as f32));
        }
    }
    out
}

/// Nearest point on a polyline to `p`: `(distance, segment_index, t)` where the
/// closest point is `lerp(cl[k], cl[k+1], t)`.
fn nearest_seg(cl: &[Vec2], p: Vec2) -> (f32, usize, f32) {
    let mut best = f32::MAX;
    let (mut bk, mut bt) = (0usize, 0.0f32);
    for k in 0..cl.len() - 1 {
        let (a, b) = (cl[k], cl[k + 1]);
        let ab = b - a;
        let len2 = ab.length_squared().max(1e-12);
        let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
        let proj = a + ab * t;
        let dd = (p - proj).length_squared();
        if dd < best {
            best = dd;
            bk = k;
            bt = t;
        }
    }
    (best.sqrt(), bk, bt)
}

/// Bilinear sample of the normalised height field at world `(x, z)`.
fn sample_height_norm(
    field: &HeightField,
    x: f32,
    z: f32,
    half_w: f32,
    half_d: f32,
    w: f32,
    d: f32,
) -> f32 {
    let n = field.n;
    let segs = (n - 1) as f32;
    let fx = (((x + half_w) / w) * segs).clamp(0.0, segs);
    let fz = (((z + half_d) / d) * segs).clamp(0.0, segs);
    let i0 = fx.floor() as usize;
    let j0 = fz.floor() as usize;
    let i1 = (i0 + 1).min(n - 1);
    let j1 = (j0 + 1).min(n - 1);
    let tx = fx - i0 as f32;
    let tz = fz - j0 as f32;
    let a = field.at(i0, j0) * (1.0 - tx) + field.at(i1, j0) * tx;
    let b = field.at(i0, j1) * (1.0 - tx) + field.at(i1, j1) * tx;
    a * (1.0 - tz) + b * tz
}

/// Box-blur a 1-D profile in place with the given window radius.
fn smooth_profile(prof: &mut [f32], radius: usize) {
    if prof.len() < 3 || radius == 0 {
        return;
    }
    let src = prof.to_vec();
    for (k, out) in prof.iter_mut().enumerate() {
        let lo = k.saturating_sub(radius);
        let hi = (k + radius).min(src.len() - 1);
        let sum: f32 = src[lo..=hi].iter().sum();
        *out = sum / (hi - lo + 1) as f32;
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if (edge1 - edge0).abs() < 1e-6 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
