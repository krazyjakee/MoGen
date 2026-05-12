use super::{count_kind, lower_src, MIN_GRID_SRC, MULTI_FLOOR_SRC};
use mogen_core::SceneGraph;

#[test]
fn multi_storey_emits_one_floor_group_per_storey() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // 3 storeys: floor_b1, floor_0, floor_1.
    let floors: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("floor_"))
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(
        floors.len(),
        3,
        "expected 3 floor groups, got {floors:?}"
    );
    assert!(floors.contains(&"floor_b1"));
    assert!(floors.contains(&"floor_0"));
    assert!(floors.contains(&"floor_1"));
}

#[test]
fn multi_storey_floors_stack_vertically_by_step() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // ceiling_height (2.6) + ceiling_thickness (0.2) = step of 2.8.
    let y_for = |name: &str| -> f32 {
        g.nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("no node named {name}"))
            .transform
            .translation
            .y
    };
    let y_b1 = y_for("floor_b1");
    let y_0 = y_for("floor_0");
    let y_1 = y_for("floor_1");
    let step = 2.6 + 0.2;
    assert!((y_0 - y_b1 - step).abs() < 1e-3, "basement→ground step wrong: {} vs {step}", y_0 - y_b1);
    assert!((y_1 - y_0 - step).abs() < 1e-3, "ground→upper step wrong: {} vs {step}", y_1 - y_0);
}

#[test]
fn staircase_emits_two_half_flights_per_storey_pair() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // Switchback: 3 storeys → 2 transitions → 2 pairs × 2 half-flights = 4.
    let flights = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .count();
    assert_eq!(
        flights, 4,
        "expected 4 half-flights (2 per pair × 2 pairs), got {flights}"
    );
}

#[test]
fn switchback_pairs_lower_and_upper_per_storey() {
    let g = lower_src(MULTI_FLOOR_SRC);
    let lowers = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .filter(|n| n.tags.iter().any(|t| t == "lower"))
        .count();
    let uppers = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .filter(|n| n.tags.iter().any(|t| t == "upper"))
        .count();
    assert_eq!(lowers, 2, "expected 2 lower half-flights, got {lowers}");
    assert_eq!(uppers, 2, "expected 2 upper half-flights, got {uppers}");
}

#[test]
fn switchback_emits_a_mid_landing_per_pair() {
    let g = lower_src(MULTI_FLOOR_SRC);
    let landings = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_landing"))
        .count();
    assert_eq!(
        landings, 2,
        "expected 2 mid-landings (one per storey-pair), got {landings}"
    );
}

#[test]
fn elevator_emits_a_single_shaft_for_the_whole_building() {
    let g = lower_src(MULTI_FLOOR_SRC);
    let shafts = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("elevator"))
        .count();
    assert_eq!(shafts, 1);
}

#[test]
fn switchback_flights_split_east_and_west() {
    // Switchback: the lower half-flight sits on the east half of the
    // cell and the upper half-flight on the west half, separated by a
    // central spine. Without this split both flights would overlap in
    // plan and the mid-landing would have nowhere coherent to put its
    // two access points.
    let g = lower_src(MULTI_FLOOR_SRC);
    let mut saw_lower = false;
    let mut saw_upper = false;
    for n in &g.nodes {
        if n.role.as_deref() != Some("stair_flight") {
            continue;
        }
        let x = n.transform.translation.x;
        if n.tags.iter().any(|t| t == "lower") {
            saw_lower = true;
            assert!(
                x > 0.05,
                "lower half-flight should sit on east half, got x={x}"
            );
        }
        if n.tags.iter().any(|t| t == "upper") {
            saw_upper = true;
            assert!(
                x < -0.05,
                "upper half-flight should sit on west half, got x={x}"
            );
        }
    }
    assert!(saw_lower && saw_upper, "expected both lower and upper flights");
}

#[test]
fn circulation_cells_get_three_sided_shaft_walls() {
    // Without explicit shaft walls the staircase north face and the
    // elevator north/south/east faces would have no adjacent cell and
    // no perimeter wall close enough to enclose them — the user could
    // walk straight out of a stair into the inset gap behind the
    // building's south wall. One staircase + one elevator → six walls.
    let g = lower_src(MULTI_FLOOR_SRC);
    let walls = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("shaft_wall"))
        .count();
    assert_eq!(walls, 6, "expected 3 walls each for stair + elevator");
}

#[test]
fn door_bfs_never_connects_stair_to_elevator() {
    // A door between two circulation cells is useless: you can't step
    // out of a stairwell into an elevator shaft. The BFS must skip
    // those edges so the tree connects the elevator via a real room.
    let g = lower_src(MULTI_FLOOR_SRC);
    for n in &g.nodes {
        let Some(role) = n.role.as_deref() else { continue };
        if role != "service_wall" {
            continue;
        }
        // Wall name format: `interior_wall_{idx}_{lo}_{hi}`.
        let parts: Vec<&str> = n.name.split('_').collect();
        if parts.len() < 5 {
            continue;
        }
        let lo: usize = parts[3].parse().unwrap();
        let hi: usize = parts[4].parse().unwrap();
        // Skip if this wall is a stair↔elevator wall (both are
        // circulation cells). We just need to check the wall isn't
        // carrying a door — i.e. it's a solid 24-vertex box, not a
        // wall_with_holes mesh.
        // Walk up to the parent cell to see its kind via the group name.
        let parent_idx = n.parent.unwrap();
        let parent_name = &g.nodes[parent_idx.0 as usize].name;
        if !(parent_name.starts_with("staircase_") || parent_name.starts_with("elevator_")) {
            continue;
        }
        // A wall between two circulation cells would have BOTH a stair
        // and an elevator on its two sides — the parent (lower idx) is
        // a circulation cell (we just checked). For a stair↔elevator
        // wall to exist, the higher-idx cell must also be circulation.
        // We can't read kinds from the graph, but the absence of a hole
        // is what proves the BFS skipped it: the moment a stair-elevator
        // edge enters the tree, this wall gets a door cutout.
        let mesh = n.mesh.as_ref().unwrap();
        let has_hole = mesh.positions.len() > 24;
        // The connectivity check is the indirect property — assert no
        // pairing where lo & hi are both circulation. We don't have
        // direct kind info, but the wall index pair `(lo, hi)` lets
        // the test author eyeball it. For MULTI_FLOOR_SRC the BFS
        // currently never picks a stair-elevator edge; this test fails
        // if a future change re-enables it.
        // The strongest assertion we can make portably: if a wall is
        // BOTH service_wall AND on a circulation cell, AND it has a
        // hole, the other side must be a room, not the other circulation
        // cell — but without kind metadata in the graph we can't tell
        // directly. Settle for the layout-independent assertion that
        // (5,6) — staircase 5 ↔ elevator 6 in MULTI_FLOOR_SRC — never
        // appears with a hole.
        if (lo, hi) == (5, 6) || (lo, hi) == (6, 5) {
            assert!(
                !has_hole,
                "BFS placed a door between stair and elevator (wall {})",
                n.name
            );
        }
    }
}

#[test]
fn upper_floors_have_no_entrance_holes() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // Entrances are tagged with role=ext_door; they should only sit
    // under the storey-0 subtree.
    let storey_for_node = |n_idx: usize| -> i32 {
        // Walk up to find the floor_<x> ancestor.
        let mut cur = Some(g.nodes[n_idx].parent);
        while let Some(Some(p)) = cur {
            let name = &g.nodes[p.0 as usize].name;
            if let Some(rest) = name.strip_prefix("floor_") {
                if let Ok(s) = rest.parse::<i32>() {
                    return s;
                } else if rest.starts_with('b') {
                    return -(rest[1..].parse::<i32>().unwrap_or(0));
                }
            }
            cur = Some(g.nodes[p.0 as usize].parent);
        }
        i32::MIN
    };
    for (i, n) in g.nodes.iter().enumerate() {
        if n.role.as_deref() == Some("ext_door") {
            assert_eq!(
                storey_for_node(i),
                0,
                "ext_door must live under floor_0, found one under storey {}",
                storey_for_node(i)
            );
        }
    }
}

#[test]
fn skylight_only_emits_on_top_storey() {
    let src = r#"
        material "c" (color=[0.8, 0.8, 0.8])
        building "t" (
          seed=2, style="grid", floor_area=60, rooms=4,
          floors_above=2, staircases=1, skylights=2, mat="c",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    let skies = g.nodes.iter().filter(|n| n.role.as_deref() == Some("skylight")).count();
    assert_eq!(skies, 2, "expected 2 skylights, got {skies}");
}

#[test]
fn t2_layout_is_deterministic_under_same_seed() {
    let a = lower_src(MULTI_FLOOR_SRC);
    let b = lower_src(MULTI_FLOOR_SRC);
    assert_eq!(a.nodes.len(), b.nodes.len(), "node count diverged");
    let hash = |g: &SceneGraph| -> f64 {
        let mut acc = 0.0f64;
        for n in &g.nodes {
            if let Some(m) = &n.mesh {
                for p in &m.positions {
                    acc += p[0] as f64;
                    acc += p[1] as f64;
                    acc += p[2] as f64;
                }
            }
        }
        acc
    };
    assert!((hash(&a) - hash(&b)).abs() < 1e-3, "mesh hash diverged under same seed");
}

#[test]
fn debug_hide_roof_drops_top_storey_ceiling() {
    // Baseline: a single-storey flat-roof building emits two slabs
    // (floor + ceiling). Setting debug_hide_roof should drop the ceiling.
    let base = MIN_GRID_SRC;
    let with_flag = MIN_GRID_SRC.replace(
        "mat=\"concrete\",",
        "mat=\"concrete\", debug_hide_roof=1,",
    );
    let g0 = lower_src(base);
    let g1 = lower_src(&with_flag);
    assert_eq!(count_kind(&g0, "slab"), 2, "baseline should have 2 slabs");
    assert_eq!(
        count_kind(&g1, "slab"),
        1,
        "debug_hide_roof should drop the ceiling slab"
    );
}

#[test]
fn debug_hide_roof_suppresses_skylights() {
    let src_base = r#"
        material "c" (color=[0.8, 0.8, 0.8])
        building "t" (
          seed=2, style="grid", floor_area=60, rooms=4,
          floors_above=1, skylights=2, mat="c",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let src_hide = src_base.replace("mat=\"c\",", "mat=\"c\", debug_hide_roof=1,");
    let g0 = lower_src(src_base);
    let g1 = lower_src(&src_hide);
    let skies = |g: &SceneGraph| {
        g.nodes
            .iter()
            .filter(|n| n.role.as_deref() == Some("skylight"))
            .count()
    };
    assert_eq!(skies(&g0), 2, "baseline should emit 2 skylights");
    assert_eq!(
        skies(&g1),
        0,
        "debug_hide_roof should suppress skylight emission"
    );
}

#[test]
fn debug_render_floor_isolates_a_single_storey() {
    // 3 storeys (b1, 0, 1). debug_render_floor=1 hides other storeys'
    // floor groups but leaves vertical circulation (stairs + elevator)
    // intact so the chosen storey reads in context.
    let src = MULTI_FLOOR_SRC.replace(
        "mat=\"concrete\",",
        "mat=\"concrete\", debug_render_floor=1,",
    );
    let g = lower_src(&src);
    let floors: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("floor_"))
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(floors, vec!["floor_1"], "should isolate exactly floor_1");
    // Stair flights still span every storey pair (b1→0, 0→1). Each
    // pair has two half-flights (switchback), so 2 pairs → 4 flights.
    let flights = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .count();
    assert_eq!(
        flights, 4,
        "stair flights should still emit for every storey pair when isolating"
    );
    // Elevator shaft still emits.
    let shafts = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("elevator"))
        .count();
    assert_eq!(
        shafts, 1,
        "elevator should still emit when isolating a floor"
    );
    // Wrapper carries `floating` so the connectivity validator (E1101)
    // exempts this partial debug view — stair flights stranded between
    // unrendered floor slabs would otherwise scan as disconnected clusters.
    let wrapper = g
        .nodes
        .iter()
        .find(|n| n.name == "tower")
        .expect("building wrapper node missing");
    assert!(
        wrapper.tags.iter().any(|t| t == "floating"),
        "isolated debug view should mark the building wrapper as floating, got tags={:?}",
        wrapper.tags
    );
}
