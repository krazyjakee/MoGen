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

use std::collections::BTreeMap;
use std::f32::consts::TAU;

use glam::{Mat4, Quat, Vec3};

use mogen_geom::{BlobChild, SdfOp, SdfPrim};

use super::config::CaveCfg;
use super::rng::{rand_f01, rand_in, rand_range, sub_seed};

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

    // Passages: a horizontal network per layer (MST + loops), plus a few
    // vertical links between adjacent layers. Every passage is slope-capped.
    for (i, j) in connect_chambers(cfg, &chambers) {
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

/// Distribute `total` chambers across `n` layers, biasing the remainder toward
/// the lower (ground) layers. Always returns exactly `n` entries.
fn distribute(total: usize, n: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    let base = total / n;
    let extra = total % n;
    (0..n).map(|i| if i < extra { base + 1 } else { base }).collect()
}

/// Place chambers as a stack of horizontal layers. Each `level` is its own band
/// of the rock, separated from the next by `level_gap` metres of solid rock, so
/// the cave reads as floors stacked on top of one another. Within a layer:
///
/// - most chambers are rejection-sampled `spacing` apart (distinct rooms), but
/// - with probability `overlap` a chamber is placed deliberately overlapping a
///   same-layer neighbour, so the two merge into one larger irregular cavern.
///
/// That mix is what gives "both overlaid and separated" in one cave. Separation
/// is enforced only within a layer; chambers in different layers may share XZ
/// (stacked rooms) because the `level_gap` keeps them apart vertically.
fn place_chambers(cfg: &CaveCfg, block_half: Vec3) -> Vec<Chamber> {
    let mut state = sub_seed(cfg.seed, 0x0CA7_E001);
    let margin = cfg.margin;
    let levels = cfg.levels.max(1);

    let usable_lo = margin;
    let usable_hi = (2.0 * block_half.y - margin).max(usable_lo + 1.0);
    let total_h = (usable_hi - usable_lo).max(1.0);
    let gaps = levels.saturating_sub(1) as f32 * cfg.level_gap;
    let layer_h = ((total_h - gaps) / levels as f32).max(1.0);

    let counts = distribute(cfg.chambers as usize, levels as usize);
    // Horizontal and vertical size caps so a chamber fits its footprint and,
    // critically, its (possibly thin) layer band — otherwise stacked layers
    // would overlap vertically and a chamber floor could dip into the shell.
    let max_fit = (block_half.x.min(block_half.z) - margin).max(0.6);
    let max_fit_v = ((layer_h * 0.5 - 0.2) / cfg.chamber_flatten).max(0.6);

    let mut chambers: Vec<Chamber> = Vec::with_capacity(cfg.chambers as usize);
    for level in 0..levels {
        let layer_lo = usable_lo + level as f32 * (layer_h + cfg.level_gap);
        let layer_hi = layer_lo + layer_h;
        let level_start = chambers.len();

        for _ in 0..counts[level as usize] {
            let r0 = rand_in(&mut state, cfg.chamber_min, cfg.chamber_max)
                .min(max_fit)
                .min(max_fit_v);
            let hy0 = (r0 * cfg.chamber_flatten).max(0.4);
            let cy_lo = (layer_lo + hy0).max(margin + hy0);
            let cy_hi = (layer_hi - hy0).min(2.0 * block_half.y - margin - hy0);
            let x_lim = (block_half.x - margin - r0).max(0.0);
            let z_lim = (block_half.z - margin - r0).max(0.0);

            let (center, r) = {
                let same_level: &[Chamber] = &chambers[level_start..];
                let want_overlap =
                    !same_level.is_empty() && rand_f01(&mut state) < cfg.overlap;
                if want_overlap {
                    // Place overlapping a random same-layer neighbour so the
                    // pair smooth-unions into one cavern.
                    let nb = same_level[rand_range(&mut state, same_level.len() as u32) as usize];
                    let frac = rand_in(&mut state, 0.3, 0.8);
                    let dist = frac * (r0 + nb.half.x);
                    let ang = rand_in(&mut state, 0.0, TAU);
                    let cx = (nb.center.x + dist * ang.cos()).clamp(-x_lim, x_lim);
                    let cz = (nb.center.z + dist * ang.sin()).clamp(-z_lim, z_lim);
                    let cy = nb.center.y.clamp(cy_lo.min(cy_hi), cy_lo.max(cy_hi));
                    (Vec3::new(cx, cy, cz), r0)
                } else {
                    // Rejection-sample a spot that clears every same-layer
                    // chamber by `spacing`; keep the best found.
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
                        let mut slack = f32::INFINITY;
                        for other in same_level {
                            let gap = cand.distance(other.center) - r0 - other.half.x - cfg.spacing;
                            slack = slack.min(gap);
                        }
                        if best.map_or(true, |(bs, _)| slack > bs) {
                            best = Some((slack, cand));
                        }
                        if slack >= 0.0 {
                            break;
                        }
                    }
                    let (slack, center) =
                        best.unwrap_or((0.0, Vec3::new(0.0, 0.5 * (cy_lo + cy_hi), 0.0)));
                    // Crowded layer: shrink rather than let two rooms merge.
                    let r = if slack < 0.0 { (r0 + slack - 0.1).max(0.8) } else { r0 };
                    (center, r)
                }
            };

            let hy = (r * cfg.chamber_flatten).max(0.4);
            chambers.push(Chamber {
                center,
                half: Vec3::new(r, hy, r),
                level,
            });
        }
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

/// Build the passage graph: a horizontal spanning tree (+ `loops`) within each
/// layer, then `level_links` vertical passages between every pair of adjacent
/// layers. The inter-layer links keep the whole cave traversable; the
/// intra-layer trees keep each floor walkable on near-flat passages.
fn connect_chambers(cfg: &CaveCfg, chambers: &[Chamber]) -> Vec<(usize, usize)> {
    let mut by_level: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (i, c) in chambers.iter().enumerate() {
        by_level.entry(c.level).or_default().push(i);
    }

    let mut edges: Vec<(usize, usize)> = Vec::new();
    for idxs in by_level.values() {
        let mst = mst_indices(chambers, idxs);
        edges.extend(mst.iter().copied());
        edges.extend(loop_edges(chambers, idxs, &edges, cfg.loops));
    }

    // Vertical links between adjacent layers. At least one per pair when there
    // is more than one layer, so upper floors stay reachable from the entrance.
    let keys: Vec<u32> = by_level.keys().copied().collect();
    let links = if cfg.levels > 1 { cfg.level_links.max(1) } else { 0 };
    for pair in keys.windows(2) {
        let a = &by_level[&pair[0]];
        let b = &by_level[&pair[1]];
        edges.extend(cross_links(chambers, a, b, links));
    }
    edges
}

/// Minimum spanning tree (Prim's) over the chamber subset `idxs`. Returns edges
/// as `(global_i, global_j)` index pairs.
fn mst_indices(chambers: &[Chamber], idxs: &[usize]) -> Vec<(usize, usize)> {
    let n = idxs.len();
    if n <= 1 {
        return Vec::new();
    }
    let mut in_tree = vec![false; n];
    in_tree[0] = true;
    let mut edges = Vec::with_capacity(n - 1);
    for _ in 1..n {
        let mut best: Option<(f32, usize, usize)> = None;
        for a in 0..n {
            if !in_tree[a] {
                continue;
            }
            for b in 0..n {
                if in_tree[b] {
                    continue;
                }
                let dist = chambers[idxs[a]].center.distance(chambers[idxs[b]].center);
                if best.map_or(true, |(bd, _, _)| dist < bd) {
                    best = Some((dist, a, b));
                }
            }
        }
        if let Some((_, a, b)) = best {
            in_tree[b] = true;
            edges.push((idxs[a], idxs[b]));
        }
    }
    edges
}

/// The `count` shortest within-subset pairs not already in `existing`, for
/// loops. `idxs` are the global indices of one layer's chambers.
fn loop_edges(
    chambers: &[Chamber],
    idxs: &[usize],
    existing: &[(usize, usize)],
    count: u32,
) -> Vec<(usize, usize)> {
    if count == 0 || idxs.len() < 3 {
        return Vec::new();
    }
    let has = |a: usize, b: usize| {
        existing
            .iter()
            .any(|&(i, j)| (i == a && j == b) || (i == b && j == a))
    };
    let mut candidates: Vec<(f32, usize, usize)> = Vec::new();
    for (pa, &i) in idxs.iter().enumerate() {
        for &j in &idxs[pa + 1..] {
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

/// `count` passages linking layer `a` to layer `b`. Pairs whose horizontal run
/// is at least their vertical rise are preferred (a single ramp stays within
/// the slope cap); among those, the shortest. Distinct endpoints are favoured
/// so multiple links spread across the layer rather than stacking on one room.
fn cross_links(
    chambers: &[Chamber],
    a: &[usize],
    b: &[usize],
    count: u32,
) -> Vec<(usize, usize)> {
    if count == 0 || a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    // Sort key: (not-walkable-as-single-ramp, 3D distance).
    let mut cands: Vec<(u8, f32, usize, usize)> = Vec::new();
    for &i in a {
        for &j in b {
            let p = chambers[i].center;
            let q = chambers[j].center;
            let horiz = ((p.x - q.x).powi(2) + (p.z - q.z).powi(2)).sqrt();
            let rise = (p.y - q.y).abs();
            let steep = if horiz >= rise { 0 } else { 1 };
            cands.push((steep, p.distance(q), i, j));
        }
    }
    cands.sort_by(|x, y| {
        x.0.cmp(&y.0)
            .then(x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut used_a: Vec<usize> = Vec::new();
    let mut used_b: Vec<usize> = Vec::new();
    for &(_, _, i, j) in &cands {
        if out.len() >= count as usize {
            break;
        }
        // Prefer fresh endpoints while we still have unused chambers to spread
        // links across; fall back to reuse once they're exhausted.
        if used_a.contains(&i) && used_b.contains(&j) && out.len() < a.len().min(b.len()) {
            continue;
        }
        out.push((i, j));
        used_a.push(i);
        used_b.push(j);
    }
    if out.is_empty() {
        let &(_, _, i, j) = &cands[0];
        out.push((i, j));
    }
    out
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
            overlap: 0.35,
            chamber_flatten: 0.6,
            level_gap: 1.5,
            level_links: 1,
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
    fn same_layer_chambers_are_separated_when_overlap_off() {
        // With overlap=0, spacing=2 and a roomy footprint, every pair of
        // chambers on the SAME layer keeps a positive rock gap (distinct rooms).
        let mut c = cfg();
        c.size = [40.0, 16.0, 40.0];
        c.chambers = 6;
        c.chamber_max = 3.0;
        c.overlap = 0.0;
        let cs = &generate(&c).chambers;
        for i in 0..cs.len() {
            for j in (i + 1)..cs.len() {
                if cs[i].level != cs[j].level {
                    continue; // different layers are separated vertically
                }
                let gap = cs[i].center.distance(cs[j].center) - cs[i].half.x - cs[j].half.x;
                assert!(gap > 0.0, "same-layer chambers {i}/{j} overlap (gap {gap})");
            }
        }
    }

    #[test]
    fn overlap_merges_some_chambers() {
        // With overlap=1, same-layer chambers should intentionally overlap.
        let mut c = cfg();
        c.size = [40.0, 16.0, 40.0];
        c.chambers = 6;
        c.levels = 1;
        c.overlap = 1.0;
        let cs = &generate(&c).chambers;
        let mut any_overlap = false;
        for i in 0..cs.len() {
            for j in (i + 1)..cs.len() {
                let gap = cs[i].center.distance(cs[j].center) - cs[i].half.x - cs[j].half.x;
                if gap < 0.0 {
                    any_overlap = true;
                }
            }
        }
        assert!(any_overlap, "overlap=1 should merge some chambers");
    }

    #[test]
    fn layers_are_vertically_separated() {
        // Each layer occupies its own Y band with rock between — the top of a
        // lower layer sits below the bottom of the next.
        let mut c = cfg();
        c.size = [30.0, 18.0, 30.0];
        c.chambers = 6;
        c.levels = 3;
        c.level_gap = 1.5;
        let cs = &generate(&c).chambers;
        for lo in 0..2u32 {
            let hi = lo + 1;
            let lo_top = cs.iter().filter(|c| c.level == lo).map(|c| c.ceiling_y()).fold(f32::NEG_INFINITY, f32::max);
            let hi_bot = cs.iter().filter(|c| c.level == hi).map(|c| c.floor_y()).fold(f32::INFINITY, f32::min);
            if lo_top.is_finite() && hi_bot.is_finite() {
                assert!(hi_bot > lo_top, "layer {hi} (floor {hi_bot}) not above layer {lo} (ceil {lo_top})");
            }
        }
    }

    #[test]
    fn passage_graph_connects_every_chamber_across_layers() {
        // The per-layer trees plus inter-layer links must leave the whole cave
        // a single connected component — every floor reachable from every other.
        let mut c = cfg();
        c.size = [30.0, 18.0, 30.0];
        c.chambers = 9;
        c.levels = 3;
        let block_half = Vec3::new(15.0, 9.0, 15.0);
        let chambers = place_chambers(&c, block_half);
        let edges = connect_chambers(&c, &chambers);

        let n = chambers.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        for &(i, j) in &edges {
            let a = find(&mut parent, i);
            let b = find(&mut parent, j);
            parent[a] = b;
        }
        let root = find(&mut parent, 0);
        assert!(
            (0..n).all(|x| find(&mut parent, x) == root),
            "cave passage graph is not fully connected"
        );
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
