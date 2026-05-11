//! Adjacency scoring for layout attempts. Higher score = better. The solver
//! picks the highest-scoring attempt; ties break toward the lowest attempt
//! index so the result remains deterministic in the user-facing `seed=`.

use super::{cell_type, Floorplate};
use super::super::config::BuildingCfg;

pub(super) fn score(cfg: &BuildingCfg, plate: &Floorplate) -> f32 {
    let mut total = 0.0;

    // Pair-wise adjacency contributions.
    for (i, a) in plate.rooms.iter().enumerate() {
        let a_type = cell_type(cfg, a);
        for b in plate.rooms.iter().skip(i + 1) {
            let edge = a.rect.shared_edge_length(&b.rect);
            if edge <= 0.0 {
                continue;
            }
            let b_type = cell_type(cfg, b);
            // Look up rules in both directions.
            total += rule_pair_score(cfg, &a_type.name, &b_type.name) * edge;
            total += rule_pair_score(cfg, &b_type.name, &a_type.name) * edge;
        }
    }

    // Heuristic priors: penalise long thin rooms (encourage usable
    // floorplans). Mild because the BSP already caps aspect — this only
    // discourages slivers when several attempts are otherwise tied.
    for cell in &plate.rooms {
        let r = &cell.rect;
        let aspect = r.width().max(r.depth()) / r.width().min(r.depth()).max(1e-3);
        if aspect > 3.0 {
            total -= (aspect - 3.0) * 0.05;
        }
    }

    total
}

/// +1 per metre of shared edge if `b` is in `a`'s `adjacent_to` list; -1 per
/// metre if `b` is in `a`'s `away_from` list. Returns 0 if no rule matches.
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
    use super::*;
    use super::super::*;
    use super::super::super::config::{
        AdjacencyRule, BuildingCfg, RoomKind, RoomType, Roof, Style, WindowModules,
    };

    fn rect(x_min: f32, x_max: f32, z_min: f32, z_max: f32) -> Rect2 {
        Rect2 { x_min, x_max, z_min, z_max }
    }

    fn cfg_with_rule(rule_name: &str, adjacent_to: Vec<&str>, away_from: Vec<&str>) -> BuildingCfg {
        BuildingCfg {
            seed: 1,
            style: Style::Grid,
            mat_style: String::new(),
            floor_area: 100.0,
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
        }
    }

    #[test]
    fn adjacent_rule_satisfied_scores_positive() {
        // kitchen adjacent to living; layout puts them sharing an edge.
        let cfg = cfg_with_rule("kitchen", vec!["living"], vec![]);
        let plate = Floorplate {
            bounds: rect(0.0, 8.0, 0.0, 4.0),
            rooms: vec![
                RoomCell { rect: rect(0.0, 4.0, 0.0, 4.0), room_type_index: 0 },
                RoomCell { rect: rect(4.0, 8.0, 0.0, 4.0), room_type_index: 1 },
            ],
        };
        let s = score(&cfg, &plate);
        assert!(s > 0.0, "expected positive score, got {s}");
    }

    #[test]
    fn away_from_rule_violated_scores_negative() {
        // kitchen away from bedroom; layout puts them sharing an edge.
        let cfg = cfg_with_rule("kitchen", vec![], vec!["bedroom"]);
        let plate = Floorplate {
            bounds: rect(0.0, 8.0, 0.0, 4.0),
            rooms: vec![
                RoomCell { rect: rect(0.0, 4.0, 0.0, 4.0), room_type_index: 0 },
                RoomCell { rect: rect(4.0, 8.0, 0.0, 4.0), room_type_index: 2 },
            ],
        };
        let s = score(&cfg, &plate);
        assert!(s < 0.0, "expected negative score, got {s}");
    }

    #[test]
    fn no_shared_edge_contributes_zero_adjacency() {
        let cfg = cfg_with_rule("kitchen", vec!["living"], vec![]);
        let plate = Floorplate {
            bounds: rect(0.0, 10.0, 0.0, 4.0),
            rooms: vec![
                RoomCell { rect: rect(0.0, 3.0, 0.0, 4.0), room_type_index: 0 },
                RoomCell { rect: rect(5.0, 8.0, 0.0, 4.0), room_type_index: 1 },
            ],
        };
        let s = score(&cfg, &plate);
        // Only the aspect-ratio prior should bite (or nothing).
        assert!(s <= 0.0, "expected no positive adjacency contribution, got {s}");
    }
}
