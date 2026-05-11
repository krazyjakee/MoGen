//! Layout subsystem. Turns a `BuildingCfg` into a `Floorplate` — a list of
//! non-overlapping axis-aligned room cells that tile a rectangle.
//!
//! Each `style=` value picks its own algorithm; all return the same
//! `Floorplate` shape so the emit pass is style-agnostic.
//!
//! Multiple attempts are run with different sub-seeds; the highest-scoring
//! attempt (per `score.rs`) wins. Score ties break toward the lowest attempt
//! index so the result is deterministic in the user-facing `seed=`.

mod grid;
mod bsp;
mod score;

use anyhow::{bail, Result};

use super::config::{BuildingCfg, RoomType, Style};
use super::rng::{attempt_seed, weighted_pick};

/// 2D axis-aligned rectangle in floor-local space. `x`/`z` are the floor
/// plane (matches the building's local frame after wrapper translation).
#[derive(Clone, Copy, Debug)]
pub(super) struct Rect2 {
    pub x_min: f32,
    pub x_max: f32,
    pub z_min: f32,
    pub z_max: f32,
}

impl Rect2 {
    pub fn width(&self) -> f32 {
        self.x_max - self.x_min
    }
    pub fn depth(&self) -> f32 {
        self.z_max - self.z_min
    }
    pub fn area(&self) -> f32 {
        self.width() * self.depth()
    }
    pub fn centre(&self) -> [f32; 2] {
        [
            0.5 * (self.x_min + self.x_max),
            0.5 * (self.z_min + self.z_max),
        ]
    }

    /// Length of the shared edge with `other`, or `0.0` if they don't touch
    /// along a full edge (touching at a corner counts as 0).
    pub fn shared_edge_length(&self, other: &Rect2) -> f32 {
        // Vertical shared edge: x matches one of x_min/x_max on each side.
        let x_share = (self.x_max - other.x_min).abs() < EDGE_EPS
            || (other.x_max - self.x_min).abs() < EDGE_EPS;
        let z_share = (self.z_max - other.z_min).abs() < EDGE_EPS
            || (other.z_max - self.z_min).abs() < EDGE_EPS;
        if x_share && !z_share {
            let z_lo = self.z_min.max(other.z_min);
            let z_hi = self.z_max.min(other.z_max);
            (z_hi - z_lo).max(0.0)
        } else if z_share && !x_share {
            let x_lo = self.x_min.max(other.x_min);
            let x_hi = self.x_max.min(other.x_max);
            (x_hi - x_lo).max(0.0)
        } else {
            0.0
        }
    }
}

const EDGE_EPS: f32 = 1e-3;

/// A single room cell on a floorplate.
#[derive(Clone, Debug)]
pub(super) struct RoomCell {
    pub rect: Rect2,
    pub room_type_index: usize,
}

#[derive(Clone, Debug)]
pub(super) struct Floorplate {
    /// Outer rectangle (interior face — does not include exterior wall thickness).
    pub bounds: Rect2,
    pub rooms: Vec<RoomCell>,
}

/// Top-level entry point. Runs `ATTEMPTS` layout attempts and keeps the best.
pub(super) fn solve(cfg: &BuildingCfg) -> Result<Floorplate> {
    let mut best: Option<(f32, Floorplate)> = None;
    for attempt in 0..ATTEMPTS {
        let mut state = attempt_seed(cfg.seed, attempt);
        let assigned_types = assign_room_types(cfg, cfg.rooms as usize, &mut state);
        let layout = match cfg.style {
            Style::Grid => grid::layout(cfg, &assigned_types, &mut state),
            Style::ApartmentBlock => bsp::layout(cfg, &assigned_types, &mut state),
        };
        let s = score::score(cfg, &layout);
        match &best {
            None => best = Some((s, layout)),
            // strict `>` so ties keep the lower attempt index (deterministic)
            Some((bs, _)) if s > *bs => best = Some((s, layout)),
            _ => {}
        }
    }
    match best {
        Some((_, plate)) => {
            if plate.rooms.is_empty() {
                bail!("building layout produced 0 rooms — try increasing `floor_area` or lowering `rooms`");
            }
            Ok(plate)
        }
        None => bail!("building layout solver failed to produce any candidate"),
    }
}

const ATTEMPTS: u32 = 10;

/// Sample `count` room-type indices from `cfg.room_types`, weighted by
/// density. The resulting list drives both the per-room count (each entry
/// becomes one cell) and the room-type assignment for layout placement.
///
/// Always guarantees every declared room type with density > 0 appears at
/// least once if `count >= active_types`, which keeps adjacency rules from
/// silently no-op'ing because a sampled distribution happened to skip a type.
fn assign_room_types(cfg: &BuildingCfg, count: usize, state: &mut u32) -> Vec<usize> {
    let weights = cfg.density_weights();
    let active_indices: Vec<usize> = weights
        .iter()
        .enumerate()
        .filter(|(_, w)| **w > 0.0)
        .map(|(i, _)| i)
        .collect();
    if active_indices.is_empty() {
        return Vec::new();
    }
    let mut result: Vec<usize> = Vec::with_capacity(count);
    // First, seed one of each active type up to `count`. Any remaining slots
    // are sampled proportionally to weight.
    for &i in active_indices.iter().take(count) {
        result.push(i);
    }
    while result.len() < count {
        let pick = weighted_pick(state, &weights);
        result.push(pick);
    }
    // Shuffle so the "guaranteed singletons" don't all land at the front of
    // the layout. Fisher-Yates with our deterministic RNG.
    for i in (1..result.len()).rev() {
        let j = (super::rng::step(state) as usize) % (i + 1);
        result.swap(i, j);
    }
    result
}

/// Aspect-aware floor dimensions for a target area. Width is along X, depth
/// along Z; aspect target ≈ √2 keeps small footprints reading as buildings
/// rather than corridors. Style-specific algorithms may further deform.
pub(super) fn floor_dims(area: f32) -> (f32, f32) {
    let target_aspect = std::f32::consts::SQRT_2;
    let depth = (area / target_aspect).sqrt();
    let width = area / depth;
    (width.max(2.0), depth.max(2.0))
}

/// Look up the `RoomType` for a cell. Helper used by both layout and emit.
pub(super) fn cell_type<'a>(cfg: &'a BuildingCfg, cell: &RoomCell) -> &'a RoomType {
    &cfg.room_types[cell.room_type_index]
}
