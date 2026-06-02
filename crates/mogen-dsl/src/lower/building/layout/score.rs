//! Adjacency scoring for layout attempts. Higher score = better. The solver
//! picks the highest-scoring attempt; ties break toward the lowest attempt
//! index so the result remains deterministic in the user-facing `seed=`.
//!
//! Tranche 3 widens the score from "shared-edge adjacency only" to also
//! reward sensible architectural priors (service rooms cluster, public
//! rooms near the entrance, private/secure rooms away from it) and
//! penalise rooms whose area falls outside their `room_type.min_area`
//! / `max_area` band. All new terms are soft scalars so the solver can
//! still pick a "less ideal but feasible" layout when the constraints
//! collide.

use super::super::config::{BuildingCfg, RoomKind};
use super::{cell_type, entrance_sides, CellKind, EntranceSupport, Floorplate, WallSide};

pub(super) fn score(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    support: &EntranceSupport,
) -> f32 {
    let mut total = 0.0;

    for (i, a) in plate.rooms.iter().enumerate() {
        let Some(a_type) = cell_type(cfg, a) else {
            continue;
        };
        for b in plate.rooms.iter().skip(i + 1) {
            let edge = a.rect.shared_edge_length(&b.rect);
            if edge <= 0.0 {
                continue;
            }
            let Some(b_type) = cell_type(cfg, b) else {
                continue;
            };
            total += rule_pair_score(cfg, &a_type.name, &b_type.name) * edge;
            total += rule_pair_score(cfg, &b_type.name, &a_type.name) * edge;
            // Kind-based priors apply to every adjacent pair regardless
            // of declared rules — they're the soft "common sense" layer
            // that pushes the solver toward plausible floorplans even
            // when the author doesn't spell out every rule.
            total += kind_pair_prior(a_type.kind, b_type.kind) * edge;
        }
    }

    // Discourage long thin rooms — mild bias only.
    for cell in &plate.rooms {
        if !matches!(cell.kind, CellKind::Room) {
            continue;
        }
        let r = &cell.rect;
        let aspect = r.width().max(r.depth()) / r.width().min(r.depth()).max(1e-3);
        if aspect > 3.0 {
            total -= (aspect - 3.0) * 0.05;
        }
    }

    total += area_band_score(cfg, plate);
    total += entrance_distance_score(cfg, plate, support);

    total
}

/// Penalise cells whose area falls outside their type's `[min_area,
/// max_area]` band. Soft penalty: 0.2 per m² of shortfall/overshoot so a
/// modest violation can still be picked over a worse-adjacency layout,
/// but a glaring one (e.g. a 4 m² room declared `min_area=20`) will be
/// dominated by this term.
fn area_band_score(cfg: &BuildingCfg, plate: &Floorplate) -> f32 {
    let mut total = 0.0;
    for cell in &plate.rooms {
        if !matches!(cell.kind, CellKind::Room) {
            continue;
        }
        let Some(typ) = cell_type(cfg, cell) else {
            continue;
        };
        let area = cell.rect.area();
        if let Some(min_a) = typ.min_area {
            if area < min_a {
                total -= (min_a - area) * 0.2;
            }
        }
        if let Some(max_a) = typ.max_area {
            if area > max_a {
                total -= (area - max_a) * 0.2;
            }
        }
    }
    total
}

/// Reward public rooms placed near any entrance and private / secure
/// rooms placed away from every entrance. Returns 0 if there's no usable
/// floorplate (degenerate case). Mirrors `place_entrances`'s round-robin
/// side distribution so the prior tracks where the doors will actually
/// land, not just the canonical south face.
fn entrance_distance_score(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    support: &EntranceSupport,
) -> f32 {
    let max_d2 =
        (plate.bounds.width().powi(2) + plate.bounds.depth().powi(2)).max(1e-3);
    let max_d = max_d2.sqrt();
    let anchors = entrance_anchors(cfg, plate, support);
    let mut total = 0.0;
    for cell in &plate.rooms {
        if !matches!(cell.kind, CellKind::Room) {
            continue;
        }
        let Some(typ) = cell_type(cfg, cell) else {
            continue;
        };
        let c = cell.rect.centre();
        let mut min_d = f32::INFINITY;
        for a in &anchors {
            let dx = c[0] - a[0];
            let dz = c[1] - a[1];
            let d = (dx * dx + dz * dz).sqrt();
            if d < min_d {
                min_d = d;
            }
        }
        let t = (min_d / max_d).clamp(0.0, 1.0);
        // t=0 ⇒ at entrance, t=1 ⇒ furthest corner.
        let weight = match typ.kind {
            RoomKind::Public => -t * 0.6,           // closer is better
            RoomKind::Private => -(1.0 - t) * 0.6,  // further is better
            RoomKind::Secure => -(1.0 - t) * 0.8,   // strongly prefer further
            RoomKind::Service => -t.min(1.0 - t) * 0.3, // prefer middle-ish
            RoomKind::Utility => -(1.0 - t) * 0.2,  // tucked away
            RoomKind::StaffOnly => -(1.0 - t) * 0.3,
        };
        total += weight;
    }
    total
}

/// Predicted entrance anchor positions for the layout scorer.
///
/// Layout solving runs before opening placement, so we can't read actual
/// entrance positions out of an `OpeningPlan`. Instead we duplicate the
/// round-robin side distribution from `emit::openings::place_entrances`,
/// sharing the same per-seed wall ordering via `entrance_side_order`, but
/// skip the per-entrance jitter and anchor each entrance at the midpoint
/// of its share of the wall. That's the expected entrance location to
/// within ~`door_w`; the scoring weights are soft enough that the small
/// jitter doesn't change which layout wins.
fn entrance_anchors(
    cfg: &BuildingCfg,
    plate: &Floorplate,
    support: &EntranceSupport,
) -> Vec<[f32; 2]> {
    let count = cfg.entrances.max(1) as usize;
    let sides = entrance_sides(cfg, &plate.bounds, support);
    if sides.is_empty() {
        return Vec::new();
    }
    let mut per_side = vec![0usize; sides.len()];
    for i in 0..count {
        per_side[i % sides.len()] += 1;
    }
    let bounds = &plate.bounds;
    let mut anchors: Vec<[f32; 2]> = Vec::with_capacity(count);
    for (side_idx, &(side, along_min, along_max)) in sides.iter().enumerate() {
        let n = per_side[side_idx];
        if n == 0 {
            continue;
        }
        let span = along_max - along_min;
        for i in 0..n {
            let t = (i as f32 + 1.0) / (n as f32 + 1.0);
            let along = along_min + t * span;
            anchors.push(match side {
                WallSide::South => [along, bounds.z_min],
                WallSide::North => [along, bounds.z_max],
                WallSide::East => [bounds.x_max, along],
                WallSide::West => [bounds.x_min, along],
            });
        }
    }
    anchors
}

/// Pairwise "good neighbour" prior keyed on `RoomKind`. These are gentle
/// nudges, not hard rules — an author-declared `adjacency` rule with
/// `±1.0 * edge_length` will outweigh them when they conflict.
fn kind_pair_prior(a: RoomKind, b: RoomKind) -> f32 {
    use RoomKind::*;
    match (a, b) {
        (Service, Service) => 0.25,
        (Private, Private) => 0.20,
        (Public, Service) | (Service, Public) => 0.15,
        (Public, Public) => 0.10,
        (Public, Private) | (Private, Public) => -0.25,
        (Service, Private) | (Private, Service) => 0.10,
        (Secure, Public) | (Public, Secure) => -0.35,
        (Secure, Secure) => 0.20,
        (StaffOnly, Public) | (Public, StaffOnly) => -0.20,
        (Utility, Public) | (Public, Utility) => -0.10,
        _ => 0.0,
    }
}


fn rule_pair_score(cfg: &BuildingCfg, a_name: &str, b_name: &str) -> f32 {
    for rule in &cfg.adjacencies {
        if rule.name != a_name {
            continue;
        }
        if rule.adjacent_to.iter().any(|n| n == b_name) {
            return 1.0;
        }
        if rule.away_from.iter().any(|n| n == b_name) {
            return -1.0;
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::super::super::config::{
        AdjacencyRule, BuildingCfg, RoomKind, RoomType, Roof, Style, WindowModules,
    };
    use super::super::*;
    use super::*;

    fn rect(x_min: f32, x_max: f32, z_min: f32, z_max: f32) -> Rect2 {
        Rect2 { x_min, x_max, z_min, z_max }
    }

    fn cfg_with_rule(rule_name: &str, adjacent_to: Vec<&str>, away_from: Vec<&str>) -> BuildingCfg {
        BuildingCfg {
            seed: 1,
            style: Style::Grid,
            mat_style: String::new(),
            floor_area: 100.0,
            cellar_area: None,
            rooms: 2,
            floors_above: 1,
            floors_below: 0,
            windows: 0,
            skylights: 0,
            roof: Roof::Flat,
            ceiling_height: 2.6,
            door_w: 0.9,
            door_h: 2.1,
            window_w: 1.2,
            window_h: 1.4,
            wall_thickness: 0.12,
            ceiling_thickness: 0.2,
            entrances: 1,
            external_door: "door_simple".into(),
            internal_door: "door_simple".into(),
            windows_mod: WindowModules {
                small: "window_simple".into(),
                medium: "window_simple".into(),
                large: "window_simple".into(),
            },
            skylight_mod: "skylight_simple".into(),
            elevators: 0,
            staircases: 0,
            room_types: vec![
                RoomType { name: "kitchen".into(), kind: RoomKind::Service, density: 1.0, mat: None, min_area: None, max_area: None },
                RoomType { name: "living".into(),  kind: RoomKind::Public,  density: 1.0, mat: None, min_area: None, max_area: None },
                RoomType { name: "bedroom".into(), kind: RoomKind::Private, density: 1.0, mat: None, min_area: None, max_area: None },
            ],
            adjacencies: vec![AdjacencyRule {
                name: rule_name.into(),
                adjacent_to: adjacent_to.into_iter().map(String::from).collect(),
                away_from: away_from.into_iter().map(String::from).collect(),
            }],
            debug_hide_roof: false,
            debug_render_floor: None,
            furnish: false,
            debug_show_poi: false,
        }
    }

    fn room_cell(rect: Rect2, idx: usize) -> RoomCell {
        RoomCell { rect, room_type_index: idx, kind: CellKind::Room, door_slots: Vec::new() }
    }

    #[test]
    fn adjacent_rule_satisfied_scores_positive() {
        let cfg = cfg_with_rule("kitchen", vec!["living"], vec![]);
        let plate = Floorplate {
            bounds: rect(0.0, 8.0, 0.0, 4.0),
            rooms: vec![
                room_cell(rect(0.0, 4.0, 0.0, 4.0), 0),
                room_cell(rect(4.0, 8.0, 0.0, 4.0), 1),
            ],
        };
        let s = score(&cfg, &plate, &EntranceSupport::none());
        assert!(s > 0.0, "expected positive score, got {s}");
    }

    #[test]
    fn away_from_rule_violated_scores_negative() {
        let cfg = cfg_with_rule("kitchen", vec![], vec!["bedroom"]);
        let plate = Floorplate {
            bounds: rect(0.0, 8.0, 0.0, 4.0),
            rooms: vec![
                room_cell(rect(0.0, 4.0, 0.0, 4.0), 0),
                room_cell(rect(4.0, 8.0, 0.0, 4.0), 2),
            ],
        };
        let s = score(&cfg, &plate, &EntranceSupport::none());
        assert!(s < 0.0, "expected negative score, got {s}");
    }

    #[test]
    fn circulation_cells_do_not_contribute_to_score() {
        let cfg = cfg_with_rule("kitchen", vec!["living"], vec![]);
        let plate = Floorplate {
            bounds: rect(0.0, 8.0, 0.0, 4.0),
            rooms: vec![
                room_cell(rect(0.0, 4.0, 0.0, 4.0), 0),
                // A circulation cell sharing an edge — must be ignored.
                RoomCell { rect: rect(4.0, 8.0, 0.0, 4.0), room_type_index: usize::MAX, kind: CellKind::Staircase, door_slots: Vec::new() },
            ],
        };
        let s = score(&cfg, &plate, &EntranceSupport::none());
        assert!(s <= 0.0, "circulation cells must not satisfy room adjacency, got {s}");
    }
}
