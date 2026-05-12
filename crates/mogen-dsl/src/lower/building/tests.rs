use crate::lower::lower;
use crate::parser::parse;
use mogen_core::SceneGraph;

fn lower_src(src: &str) -> SceneGraph {
    let ast = parse(src).expect("parse");
    lower(&ast).expect("lower")
}

fn count_kind(g: &SceneGraph, kind: &str) -> usize {
    g.nodes.iter().filter(|n| n.kind == kind).count()
}

fn has_tag(g: &SceneGraph, tag: &str) -> bool {
    g.nodes
        .iter()
        .any(|n| n.tags.iter().any(|t| t == tag))
}

const MIN_GRID_SRC: &str = r#"
material "concrete" (color=[0.8, 0.8, 0.8])
building "shed" (
  seed=5, style="grid", floor_area=40, rooms=4, windows=2, entrances=1,
  mat="concrete",
) {
  room_type "office" (kind=staff_only, density=1)
}
"#;

#[test]
fn building_lowers_to_walls_slabs_doors() {
    let g = lower_src(MIN_GRID_SRC);
    // Top-level wrapper plus floor + shell + rooms + openings groups.
    assert!(
        g.nodes.iter().any(|n| n.kind == "building"),
        "wrapper node missing"
    );
    // Two slabs (floor + ceiling).
    let slabs = count_kind(&g, "slab");
    assert!(slabs >= 2, "expected ≥ 2 slab nodes, got {slabs}");
    // Four perimeter walls + at least three interior walls (4 rooms → ≥3 shared edges).
    let walls = count_kind(&g, "wall");
    assert!(walls >= 4, "expected ≥ 4 wall nodes, got {walls}");
    // Building tag propagated.
    assert!(has_tag(&g, "building"), "no `building` tag on subtree");
    // At least one entrance instance — door_simple module body becomes the
    // subtree under an `ext_door_*` group.
    assert!(
        g.nodes.iter().any(|n| n.role.as_deref() == Some("ext_door")),
        "no ext_door role on any node"
    );
}

#[test]
fn building_apartment_smoke() {
    let src = r#"
        material "warm_wood" (color=[0.55, 0.38, 0.22])
        material "tile" (color=[0.9, 0.9, 0.85])
        building "flat" (
          seed=2, style="apartment-block",
          floor_area=70, rooms=5, windows=4, entrances=1,
          mat="warm_wood",
        ) {
          room_type "bedroom" (kind=private, density=2)
          room_type "kitchen" (kind=service, density=1, mat="tile")
          room_type "living"  (kind=public,  density=2)
          adjacency "kitchen" (adjacent_to=["living"])
        }
    "#;
    let g = lower_src(src);
    assert!(count_kind(&g, "slab") >= 2);
    assert!(count_kind(&g, "wall") >= 4);
    // Per-room groups appear with `room` role.
    let rooms = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("room"))
        .count();
    assert!(rooms >= 2, "expected ≥ 2 rooms, got {rooms}");
}

#[test]
fn building_is_deterministic_under_same_seed() {
    let a = lower_src(MIN_GRID_SRC);
    let b = lower_src(MIN_GRID_SRC);
    assert_eq!(a.nodes.len(), b.nodes.len(), "node count diverged");
    // Position hash: sum of every mesh vertex coordinate. A divergence in
    // layout or geometry shifts this.
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
    assert!((hash(&a) - hash(&b)).abs() < 1e-3, "mesh hash diverged");
}

#[test]
fn building_seed_changes_layout() {
    let src1 = MIN_GRID_SRC.replace("seed=5", "seed=5");
    let src2 = MIN_GRID_SRC.replace("seed=5", "seed=99");
    let a = lower_src(&src1);
    let b = lower_src(&src2);
    // Same room count → same wall + slab counts; but the door spans differ
    // because the entrance jitter changes with seed.
    let door_x = |g: &SceneGraph| -> Vec<f32> {
        g.nodes
            .iter()
            .filter(|n| n.role.as_deref() == Some("ext_door"))
            .map(|n| n.transform.translation.x)
            .collect()
    };
    let xa = door_x(&a);
    let xb = door_x(&b);
    assert_eq!(xa.len(), xb.len(), "entrance count diverged across seeds");
    let any_diff = xa.iter().zip(&xb).any(|(x, y)| (x - y).abs() > 1e-4);
    assert!(any_diff, "entrance positions identical across distinct seeds");
}

#[test]
fn building_unknown_door_module_falls_back_to_panel() {
    let src = r#"
        material "concrete" (color=[0.8, 0.8, 0.8])
        building "x" (
          seed=1, style="grid", floor_area=30, rooms=2,
          entrances=1, external_door="no_such_module",
          mat="concrete",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    // The validator would flag this as unknown-module; lowering should still
    // succeed and emit the fallback panel.
    let g = lower_src(src);
    // Fallback panel kind is "panel".
    let panel_count = count_kind(&g, "panel");
    assert!(
        panel_count >= 1,
        "expected fallback panel, got {panel_count}"
    );
}

#[test]
fn building_subtree_marked_non_editable() {
    let g = lower_src(MIN_GRID_SRC);
    // Wrapper stays editable; everything below it is non-editable.
    let wrapper = g
        .nodes
        .iter()
        .position(|n| n.kind == "building")
        .expect("wrapper");
    assert!(g.nodes[wrapper].editable, "wrapper should stay editable");
    // Any descendant should have editable=false.
    let any_non_editable = g.nodes.iter().enumerate().any(|(i, n)| {
        i != wrapper && !n.editable && n.kind != "scene"
    });
    assert!(any_non_editable, "no non-editable descendants found");
}

#[test]
fn building_requires_room_type() {
    let src = r#"
        material "concrete" (color=[0.8, 0.8, 0.8])
        building "empty" (seed=1, style="grid", floor_area=20, rooms=1, mat="concrete") {}
    "#;
    let ast = parse(src).expect("parse");
    let err = lower(&ast).expect_err("must require at least one room_type");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("room_type"),
        "expected error mentioning `room_type`, got: {msg}"
    );
}

// --- Tranche 2 tests ---

const MULTI_FLOOR_SRC: &str = r#"
material "concrete" (color=[0.8, 0.8, 0.8])
building "tower" (
  seed=3, style="grid",
  floor_area=80, rooms=8,
  floors_above=2, floors_below=1,
  staircases=1, elevators=1,
  mat="concrete",
) {
  room_type "office" (kind=staff_only, density=1)
}
"#;

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
fn staircase_emits_one_flight_per_storey_pair() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // 3 storeys → 2 transitions → 2 flights.
    let flights = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .count();
    assert_eq!(flights, 2, "expected 2 flights between 3 storeys, got {flights}");
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
    // 3 storeys (b1, 0, 1). debug_render_floor=1 should keep only floor_1
    // and skip vertical circulation between storeys.
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
    // No ceiling slab on the rendered floor (which was top → no ceiling
    // means only one slab, the floor).
    assert_eq!(count_kind(&g, "slab"), 1, "isolated floor has no ceiling");
    // Circulation skipped entirely.
    let flights = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .count();
    assert_eq!(flights, 0, "circulation should be skipped when isolating a floor");
    let shafts = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("elevator"))
        .count();
    assert_eq!(shafts, 0, "elevator should be skipped when isolating a floor");
}

#[test]
fn stair_xy_consistent_across_storeys() {
    let g = lower_src(MULTI_FLOOR_SRC);
    let flight_positions: Vec<(f32, f32)> = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .map(|n| {
            // Flight transform is storey-local — walk up to its staircase
            // parent and read that parent's XZ.
            let parent_idx = n.parent.unwrap();
            let p = &g.nodes[parent_idx.0 as usize];
            (p.transform.translation.x, p.transform.translation.z)
        })
        .collect();
    assert!(flight_positions.len() >= 2);
    let (x0, z0) = flight_positions[0];
    for (i, (x, z)) in flight_positions.iter().enumerate().skip(1) {
        assert!(
            (x - x0).abs() < 1e-3 && (z - z0).abs() < 1e-3,
            "flight {i} XZ ({x},{z}) diverges from flight 0 ({x0},{z0}) — stairs misaligned across storeys"
        );
    }
}
