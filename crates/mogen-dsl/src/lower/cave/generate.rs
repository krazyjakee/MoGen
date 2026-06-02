//! Cave layout solver. Turns a `CaveCfg` into the implicit-field carvers that
//! hollow a rock block into a traversable cave, plus the chamber anchors the
//! decoration pass scatters features onto.
//!
//! Pipeline:
//! 1. Place `chambers` oblate-ellipsoid chambers, distributed across `levels`
//!    vertical bands so cavities at different heights overlap into multiple
//!    connected floors.
//! 2. Connect every chamber with a spanning tree of passages (plus optional
//!    `loops`) so the whole cave is traversable.
//! 3. Any passage steeper than `max_slope` is rebuilt as a switchback ramp —
//!    each leg's rise-over-run is capped, so no walkable surface exceeds the
//!    angle limit even when linking distant floors.
//! 4. Punch `entrances` horizontal mouths out through the nearest side face so
//!    the cave can actually be entered.
//!
//! Every carver is a `Subtract` `BlobChild`; `emit.rs` pairs them with an
//! additive bounding box and meshes `box − ⋃ carvers` with surface nets.

use glam::{Mat4, Quat, Vec3};

use mogen_geom::{BlobChild, SdfOp, SdfPrim};

use super::config::CaveCfg;
use super::rng::{rand_in, sub_seed};

/// One carved cavity. Oblate (`half.y < half.x`) so the floor reads as gently
/// curved rather than a deep bowl.
#[derive(Clone, Copy, Debug)]
pub(super) struct Chamber {
    pub center: Vec3,
    pub half: Vec3,
    /// Vertical band index this chamber was placed in. Read by tests and kept
    /// for downstream passes that may want to reason about floors.
    #[allow(dead_code)]
    pub level: u32,
}

impl Chamber {
    pub fn floor_y(&self) -> f32 {
        self.center.y - self.half.y
    }
    pub fn ceiling_y(&self) -> f32 {
        self.center.y + self.half.y
    }
    /// Radius of the roughly-flat walkable disc at the chamber floor.
    pub fn floor_radius(&self) -> f32 {
        0.55 * self.half.x.min(self.half.z)
    }
}

pub(super) struct CaveLayout {
    pub chambers: Vec<Chamber>,
    /// Subtract carvers (chambers + passages + entrances).
    pub carvers: Vec<BlobChild>,
    pub block_half: Vec3,
    pub block_center: Vec3,
}

pub(super) fn generate(cfg: &CaveCfg) -> CaveLayout {
    let [w, h, d] = cfg.size;
    let block_half = Vec3::new(0.5 * w, 0.5 * h, 0.5 * d);
    // Base of the block sits on y=0 (consistent with `building`).
    let block_center = Vec3::new(0.0, 0.5 * h, 0.0);

    let chambers = place_chambers(cfg, block_half);

    let mut carvers: Vec<BlobChild> = Vec::new();

    // Chamber cavities.
    for c in &chambers {
        carvers.push(ellipsoid_carver(c.center, c.half));
    }

    // Passages: spanning tree + a few loop edges, each slope-capped.
    let mut edges = spanning_edges(&chambers);
    edges.extend(loop_edges(&chambers, &edges, cfg.loops));
    for (i, j) in edges {
        add_passage(
            &mut carvers,
            chambers[i].center,
            chambers[j].center,
            cfg.passage_radius,
            cfg.max_slope,
        );
    }

    // Entrances: punch horizontal mouths out through the nearest side face.
    add_entrances(&mut carvers, &chambers, cfg, block_half);

    CaveLayout {
        chambers,
        carvers,
        block_half,
        block_center,
    }
}

/// Maximum rejection-sampling attempts per chamber before accepting the best
/// candidate found so far.
const PLACE_ATTEMPTS: u32 = 24;

/// Place chambers across the vertical bands, keeping every cavity a `margin`
/// rock shell away from the block faces AND at least `spacing` metres of rock
/// from every other chamber. Separation is what keeps chambers distinct rooms
/// linked by passages instead of merging into one continuous blob.
fn place_chambers(cfg: &CaveCfg, block_half: Vec3) -> Vec<Chamber> {
    let mut state = sub_seed(cfg.seed, 0x0CA7_E001);
    let margin = cfg.margin;
    let levels = cfg.levels.max(1);

    // Vertical band each level occupies inside the rock shell.
    let usable_lo = margin;
    let usable_hi = (2.0 * block_half.y - margin).max(usable_lo + 1.0);
    let band = (usable_hi - usable_lo) / levels as f32;

    let mut chambers: Vec<Chamber> = Vec::with_capacity(cfg.chambers as usize);
    for i in 0..cfg.chambers {
        let level = ((i as u32) * levels / cfg.chambers.max(1)).min(levels - 1);

        // Radius, clamped so the chamber fits the footprint with its rock shell.
        let max_fit = (block_half.x.min(block_half.z) - margin).max(0.6);
        let r = rand_in(&mut state, cfg.chamber_min, cfg.chamber_max).min(max_fit);
        let hy = (r * cfg.chamber_flatten).max(0.4);

        let band_lo = usable_lo + level as f32 * band;
        let band_hi = band_lo + band;
        let cy_lo = (band_lo + hy).max(margin + hy);
        let cy_hi = (band_hi - hy).min(2.0 * block_half.y - margin - hy);
        let x_lim = (block_half.x - margin - r).max(0.0);
        let z_lim = (block_half.z - margin - r).max(0.0);

        // Rejection sample: keep the candidate that sits furthest from its
        // nearest neighbour (relative to the required gap), so even a crowded
        // footprint degrades gracefully instead of stacking chambers.
        let mut best: Option<(f32, Vec3)> = None;
        for _ in 0..PLACE_ATTEMPTS {
            let cy = if cy_hi > cy_lo {
                rand_in(&mut state, cy_lo, cy_hi)
            } else {
                0.5 * (cy_lo + cy_hi)
            };
            let cand = Vec3::new(
                rand_in(&mut state, -x_lim, x_lim),
                cy,
                rand_in(&mut state, -z_lim, z_lim),
            );
            // Slack = nearest surface-to-surface gap minus the required spacing.
            // Positive means the candidate clears every existing chamber.
            let mut slack = f32::INFINITY;
            for other in &chambers {
                let surf_gap = cand.distance(other.center) - r - other.half.x - cfg.spacing;
                slack = slack.min(surf_gap);
            }
            if best.map_or(true, |(bs, _)| slack > bs) {
                best = Some((slack, cand));
            }
            if slack >= 0.0 {
                break; // good enough — clears everyone
            }
        }
        let (slack, center) =
            best.unwrap_or((0.0, Vec3::new(0.0, 0.5 * (cy_lo + cy_hi), 0.0)));

        // If even the best spot still overlaps a neighbour (crowded footprint),
        // shrink this chamber to restore the gap rather than letting two rooms
        // merge. Clamped to a small floor so it stays a usable cavity.
        let r = if slack < 0.0 {
            (r + slack - 0.1).max(0.8)
        } else {
            r
        };
        let hy = (r * cfg.chamber_flatten).max(0.4);

        chambers.push(Chamber {
            center,
            half: Vec3::new(r, hy, r),
            level,
        });
    }
    chambers
}

/// The implicit-field children that define the rock solid: an additive
/// bounding box minus every cavity carver. Shared by the mesher (`emit`) and
/// the decoration placer (`decorate`), which marches this field to find the
/// true carved floor / ceiling under each feature.
pub(super) fn rock_field(layout: &CaveLayout) -> Vec<BlobChild> {
    let mut children = Vec::with_capacity(layout.carvers.len() + 1);
    children.push(BlobChild::new(
        SdfPrim::Box {
            half: layout.block_half,
        },
        SdfOp::Add,
        Mat4::from_translation(layout.block_center),
    ));
    children.extend(layout.carvers.iter().cloned());
    children
}

/// Minimum spanning tree over chamber centres (Prim's). Guarantees every
/// chamber is reachable from every other.
fn spanning_edges(chambers: &[Chamber]) -> Vec<(usize, usize)> {
    let n = chambers.len();
    if n <= 1 {
        return Vec::new();
    }
    let mut in_tree = vec![false; n];
    in_tree[0] = true;
    let mut edges = Vec::with_capacity(n - 1);
    for _ in 1..n {
        let mut best: Option<(f32, usize, usize)> = None;
        for i in 0..n {
            if !in_tree[i] {
                continue;
            }
            for j in 0..n {
                if in_tree[j] {
                    continue;
                }
                let dist = chambers[i].center.distance(chambers[j].center);
                if best.map_or(true, |(bd, _, _)| dist < bd) {
                    best = Some((dist, i, j));
                }
            }
        }
        if let Some((_, i, j)) = best {
            in_tree[j] = true;
            edges.push((i, j));
        }
    }
    edges
}

/// The `count` shortest chamber pairs not already in `existing`, for loops.
fn loop_edges(
    chambers: &[Chamber],
    existing: &[(usize, usize)],
    count: u32,
) -> Vec<(usize, usize)> {
    if count == 0 || chambers.len() < 3 {
        return Vec::new();
    }
    let has = |a: usize, b: usize| {
        existing
            .iter()
            .any(|&(i, j)| (i == a && j == b) || (i == b && j == a))
    };
    let mut candidates: Vec<(f32, usize, usize)> = Vec::new();
    for i in 0..chambers.len() {
        for j in (i + 1)..chambers.len() {
            if has(i, j) {
                continue;
            }
            candidates.push((chambers[i].center.distance(chambers[j].center), i, j));
        }
    }
    candidates.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    candidates
        .into_iter()
        .take(count as usize)
        .map(|(_, i, j)| (i, j))
        .collect()
}

/// Add a passage between two points, capping slope at `max_slope` degrees by
/// inserting switchback legs when the direct line would be too steep.
fn add_passage(carvers: &mut Vec<BlobChild>, a: Vec3, b: Vec3, radius: f32, max_slope_deg: f32) {
    let dx = b.x - a.x;
    let dz = b.z - a.z;
    let horiz = (dx * dx + dz * dz).sqrt();
    let dy = b.y - a.y;
    let tan_max = max_slope_deg.to_radians().tan();

    // Effectively vertical pair (overlapping in plan): a single straight link
    // is the only option — chambers this stacked already overlap, so the steep
    // stub is short and lives inside the merged cavity.
    if horiz < 1e-3 {
        add_capsule(carvers, a, b, radius);
        return;
    }

    let max_rise = horiz * tan_max;
    if dy.abs() <= max_rise + 1e-3 {
        add_capsule(carvers, a, b, radius);
        return;
    }

    // Switchback: zig-zag between the two plan positions, climbing at most
    // `max_rise` per leg so every leg's slope stays within the cap.
    let n = ((dy.abs() / max_rise).ceil() as u32).max(2);
    let a_xz = Vec3::new(a.x, 0.0, a.z);
    let b_xz = Vec3::new(b.x, 0.0, b.z);
    let mut prev = a;
    for leg in 1..=n {
        let y = a.y + dy * (leg as f32 / n as f32);
        let xz = if leg % 2 == 1 { b_xz } else { a_xz };
        let next = Vec3::new(xz.x, y, xz.z);
        add_capsule(carvers, prev, next, radius);
        prev = next;
    }
    if prev.distance(b) > 1e-3 {
        add_capsule(carvers, prev, b, radius);
    }
}

/// Punch `entrances` horizontal mouths out through the nearest side face,
/// hosted on the lowest chambers so the openings land at ground level.
fn add_entrances(
    carvers: &mut Vec<BlobChild>,
    chambers: &[Chamber],
    cfg: &CaveCfg,
    block_half: Vec3,
) {
    if cfg.entrances == 0 || chambers.is_empty() {
        return;
    }
    // Lowest chambers first (prefer ground-level mouths).
    let mut order: Vec<usize> = (0..chambers.len()).collect();
    order.sort_by(|&i, &j| {
        chambers[i]
            .center
            .y
            .partial_cmp(&chambers[j].center.y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for &idx in order.iter().take(cfg.entrances as usize) {
        let c = &chambers[idx];
        // Opening centre sits a passage-radius above the chamber floor so the
        // mouth's lower lip lands near walking height.
        let y = (c.floor_y() + cfg.passage_radius).min(c.ceiling_y());
        let start = Vec3::new(c.center.x, y, c.center.z);

        // Nearest of the four vertical faces.
        let dpx = block_half.x - c.center.x;
        let dnx = c.center.x + block_half.x;
        let dpz = block_half.z - c.center.z;
        let dnz = c.center.z + block_half.z;
        let min = dpx.min(dnx).min(dpz).min(dnz);
        let reach = block_half.x.max(block_half.z) + cfg.margin + cfg.passage_radius + 1.0;
        let end = if min == dpx {
            Vec3::new(block_half.x + cfg.margin + 1.0, y, c.center.z)
        } else if min == dnx {
            Vec3::new(-(block_half.x + cfg.margin + 1.0), y, c.center.z)
        } else if min == dpz {
            Vec3::new(c.center.x, y, block_half.z + cfg.margin + 1.0)
        } else {
            Vec3::new(c.center.x, y, -(block_half.z + cfg.margin + 1.0))
        };
        let _ = reach;
        add_capsule(carvers, start, end, cfg.passage_radius);
    }
}

/// A sphere carver at `center` with the given ellipsoid half-extents.
fn ellipsoid_carver(center: Vec3, half: Vec3) -> BlobChild {
    let xform = Mat4::from_translation(center);
    BlobChild::new(SdfPrim::Ellipsoid { half }, SdfOp::Subtract, xform)
}

/// A capsule carver spanning `p`→`q`. Degenerates to a sphere when the
/// endpoints coincide.
fn add_capsule(carvers: &mut Vec<BlobChild>, p: Vec3, q: Vec3, radius: f32) {
    let delta = q - p;
    let len = delta.length();
    if len < 1e-4 {
        let xform = Mat4::from_translation(p);
        carvers.push(BlobChild::new(
            SdfPrim::Sphere { radius },
            SdfOp::Subtract,
            xform,
        ));
        return;
    }
    let dir = delta / len;
    let rot = Quat::from_rotation_arc(Vec3::Y, dir);
    let center = (p + q) * 0.5;
    let xform = Mat4::from_scale_rotation_translation(Vec3::ONE, rot, center);
    carvers.push(BlobChild::new(
        SdfPrim::Capsule { radius, height: len },
        SdfOp::Subtract,
        xform,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::config::CaveCfg;

    fn cfg() -> CaveCfg {
        CaveCfg {
            seed: 7,
            mat_style: String::new(),
            size: [24.0, 12.0, 24.0],
            chambers: 6,
            levels: 2,
            chamber_min: 2.5,
            chamber_max: 4.0,
            spacing: 2.0,
            chamber_flatten: 0.6,
            passage_radius: 1.1,
            loops: 1,
            max_slope: 45.0,
            roughness: 0.3,
            blend: 1.5,
            margin: 2.0,
            resolution: 64,
            entrances: 1,
            water_mat: None,
            decorations: Vec::new(),
            debug_hide_shell: false,
        }
    }

    #[test]
    fn chambers_stay_inside_the_rock_shell() {
        let layout = generate(&cfg());
        let bh = layout.block_half;
        let margin = 2.0;
        for c in &layout.chambers {
            assert!(c.center.x - c.half.x >= -bh.x - 1e-3);
            assert!(c.center.x + c.half.x <= bh.x + 1e-3);
            assert!(c.floor_y() >= margin - 0.5, "floor {} below shell", c.floor_y());
            assert!(c.ceiling_y() <= 2.0 * bh.y - margin + 0.5);
        }
    }

    #[test]
    fn levels_are_distributed() {
        let layout = generate(&cfg());
        let max_level = layout.chambers.iter().map(|c| c.level).max().unwrap();
        assert_eq!(max_level, 1, "two levels expected to be populated");
    }

    #[test]
    fn deterministic_under_same_seed() {
        let a = generate(&cfg());
        let b = generate(&cfg());
        assert_eq!(a.chambers.len(), b.chambers.len());
        for (x, y) in a.chambers.iter().zip(&b.chambers) {
            assert!((x.center - y.center).length() < 1e-6);
        }
    }

    #[test]
    fn chambers_are_separated() {
        // With spacing=2 and a roomy footprint, every pair of chambers should
        // keep a positive rock gap between their surfaces (so they read as
        // distinct rooms rather than one merged blob).
        let mut c = cfg();
        c.size = [40.0, 14.0, 40.0];
        c.chambers = 5;
        c.chamber_max = 3.0;
        let layout = generate(&c);
        let cs = &layout.chambers;
        for i in 0..cs.len() {
            for j in (i + 1)..cs.len() {
                let gap = cs[i].center.distance(cs[j].center) - cs[i].half.x - cs[j].half.x;
                assert!(gap > 0.0, "chambers {i}/{j} overlap (gap {gap})");
            }
        }
    }

    #[test]
    fn passages_respect_slope_cap() {
        // A steep pair must be broken into switchback legs each within 45°.
        let mut carvers = Vec::new();
        let a = Vec3::new(0.0, 0.0, 0.0);
        let b = Vec3::new(1.0, 8.0, 0.0); // ~83° direct
        add_passage(&mut carvers, a, b, 1.0, 45.0);
        assert!(carvers.len() >= 2, "steep passage should switchback");
    }
}
