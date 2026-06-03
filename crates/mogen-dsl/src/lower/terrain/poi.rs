//! Point-of-interest markers for terrain.
//!
//! Like `cave`/`building`, terrain drops transform-only marker nodes a game
//! engine reads from the glTF to place gameplay content the generator leaves
//! out: `peak` summits, `flat_spot` buildable areas, and `shoreline` points at
//! the water's edge. All markers route through the shared `emit_poi_group`
//! harness so the POI contract stays identical across generators.

use glam::Vec3;

use mogen_core::{NodeId, SceneGraph, Transform};

use crate::ast::Node;
use crate::lower::poi::{emit_poi_group, PoiDebug, PoiMarker};

use super::config::TerrainCfg;
use super::field::HeightField;
use super::materials::{poi_debug_color, poi_debug_mat_name};

const MARKER_RADIUS: f32 = 0.4;

pub(super) fn emit_pois(
    node: &Node,
    cfg: &TerrainCfg,
    field: &HeightField,
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    let n = field.n;
    let segments = (n - 1) as f32;
    let w = cfg.size[0];
    let d = cfg.size[2];
    let amp = cfg.size[1];
    let world = |i: usize, j: usize| -> Vec3 {
        let x = -0.5 * w + (i as f32 / segments) * w;
        let z = -0.5 * d + (j as f32 / segments) * d;
        Vec3::new(x, field.at(i, j) * amp, z)
    };

    let mut markers: Vec<PoiMarker> = Vec::new();

    if cfg.peaks > 0 {
        for (i, j) in select_peaks(field, cfg.peaks as usize) {
            markers.push(marker("peak", world(i, j)));
        }
    }
    if cfg.flat_spots > 0 {
        for (i, j) in select_flat_spots(field, cfg, cfg.flat_spots as usize) {
            markers.push(marker("flat_spot", world(i, j)));
        }
    }
    if cfg.sea_level > 0.0 && cfg.shore_points > 0 {
        for (i, j) in select_shoreline(field, cfg.sea_level, cfg.shore_points as usize) {
            markers.push(marker("shoreline", world(i, j)));
        }
    }

    emit_poi_group(
        graph,
        parent,
        node.origin.as_deref(),
        "points_of_interest",
        &["terrain".to_string(), "poi".to_string()],
        cfg.debug_show_poi,
        markers,
    );
}

fn marker(kind: &str, pos: Vec3) -> PoiMarker {
    PoiMarker {
        name_key: kind.to_string(),
        role: kind.to_string(),
        tags: vec!["terrain".to_string(), "poi".to_string(), kind.to_string()],
        transform: Transform::from_translation(pos),
        debug: Some(PoiDebug {
            mat_name: poi_debug_mat_name(kind),
            color: poi_debug_color(kind),
            radius: MARKER_RADIUS,
        }),
    }
}

/// Local maxima (strictly higher than all 8 neighbours), highest first, spread
/// out so two markers never land on the same hill.
fn select_peaks(field: &HeightField, count: usize) -> Vec<(usize, usize)> {
    let n = field.n;
    let mut cands: Vec<(f32, usize, usize)> = Vec::new();
    for j in 1..n - 1 {
        for i in 1..n - 1 {
            let h = field.at(i, j);
            let mut is_max = true;
            'nb: for dj in -1i32..=1 {
                for di in -1i32..=1 {
                    if di == 0 && dj == 0 {
                        continue;
                    }
                    let ii = (i as i32 + di) as usize;
                    let jj = (j as i32 + dj) as usize;
                    if field.at(ii, jj) >= h {
                        is_max = false;
                        break 'nb;
                    }
                }
            }
            if is_max {
                cands.push((h, i, j));
            }
        }
    }
    cands.sort_by(|a, b| b.0.total_cmp(&a.0));
    pick_spread(cands, count, min_separation(n, count))
}

/// Cells whose neighbourhood height range is smallest (flattest), above sea
/// level, flattest first, spread out.
fn select_flat_spots(
    field: &HeightField,
    cfg: &TerrainCfg,
    count: usize,
) -> Vec<(usize, usize)> {
    let n = field.n;
    let mut cands: Vec<(f32, usize, usize)> = Vec::new();
    for j in 1..n - 1 {
        for i in 1..n - 1 {
            let h = field.at(i, j);
            if h <= cfg.sea_level + 1e-4 {
                continue; // not under/at water
            }
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for dj in -1i32..=1 {
                for di in -1i32..=1 {
                    let v = field.at((i as i32 + di) as usize, (j as i32 + dj) as usize);
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            let range = hi - lo;
            // Lower range = flatter. Sort ascending by storing the negated
            // range so the shared descending `pick_spread` returns flattest
            // first.
            cands.push((-range, i, j));
        }
    }
    cands.sort_by(|a, b| b.0.total_cmp(&a.0));
    pick_spread(cands, count, min_separation(n, count))
}

/// Cells at the water's edge. Land keeps its real shape underwater now (no flat
/// flooded floor), so the shoreline is the sea-level *crossing*: a submerged (or
/// at-water) cell with at least one land neighbour above the waterline. Markers
/// are ranked by closeness to the waterline so they sit right on the coast.
fn select_shoreline(field: &HeightField, sea: f32, count: usize) -> Vec<(usize, usize)> {
    let n = field.n;
    let mut cands: Vec<(f32, usize, usize)> = Vec::new();
    for j in 1..n - 1 {
        for i in 1..n - 1 {
            let h = field.at(i, j);
            if h > sea {
                continue; // mark the water side of the line, not the land side
            }
            let mut touches_land = false;
            'nb: for dj in -1i32..=1 {
                for di in -1i32..=1 {
                    let v = field.at((i as i32 + di) as usize, (j as i32 + dj) as usize);
                    if v > sea {
                        touches_land = true;
                        break 'nb;
                    }
                }
            }
            if touches_land {
                // Rank by closeness to the waterline (smallest depth below sea);
                // pick_spread takes the highest score first, so negate the depth.
                cands.push((-(sea - h), i, j));
            }
        }
    }
    cands.sort_by(|a, b| b.0.total_cmp(&a.0));
    // The shoreline is a thin 1D ring, so the area-based separation used for
    // peaks/flats would let only a handful fit. Use a quarter of it so markers
    // walk the coast at a sensible spacing.
    let sep = (min_separation(n, count) / 4).max(1);
    pick_spread(cands, count, sep)
}

/// Greedily take the top candidates, skipping any within `sep` grid cells
/// (Chebyshev) of an already-picked one, so markers stay spread across the patch.
fn pick_spread(cands: Vec<(f32, usize, usize)>, count: usize, sep: i32) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (_, i, j) in cands {
        if out.len() >= count {
            break;
        }
        let too_close = out.iter().any(|&(oi, oj)| {
            (oi as i32 - i as i32).abs() < sep && (oj as i32 - j as i32).abs() < sep
        });
        if !too_close {
            out.push((i, j));
        }
    }
    out
}

/// Minimum grid separation between markers — scales with grid size and inversely
/// with the requested count so a few markers spread wide and many pack tighter.
fn min_separation(n: usize, count: usize) -> i32 {
    if count == 0 {
        return 1;
    }
    ((n as f32 / (count as f32).sqrt() * 0.5) as i32).max(1)
}
