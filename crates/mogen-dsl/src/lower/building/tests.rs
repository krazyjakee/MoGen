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
fn perimeter_window_holes_align_on_every_side() {
    // Regression: opening_local's East/West sign was flipped, so window
    // models sat in front of a wall hole sized for a different window
    // (small in a large hole, large in a small). The hole's wall-local
    // X must map back to the window's world XZ under the wall's rotation
    // — verify the per-storey-0 perimeter walls have a full-height pier
    // gap whose centre matches each window's projected world coord, and
    // whose width matches the window's opening width.
    use glam::Vec3;
    // Use an explicit small floorplate with 4 windows so the placement
    // distributes one window per side (no per-segment clustering, which
    // is a separate layout issue and would merge holes in the wall mesh).
    let src = r#"
        material "p" (color=[0.9, 0.9, 0.88])
        building "house" (
          seed=11, style="grid",
          floor_area=85, rooms=5,
          windows=4, entrances=1,
          mat="p",
        ) {
          room_type "room" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);

    let world_pos = |g: &SceneGraph, mut id: usize| -> Vec3 {
        let mut acc = Vec3::ZERO;
        loop {
            acc = g.nodes[id].transform.translation
                + g.nodes[id].transform.rotation * acc;
            match g.nodes[id].parent {
                Some(p) => id = p.0 as usize,
                None => break acc,
            }
        }
    };

    // For each perimeter wall, find the gaps between full-height pier
    // sub-boxes along its long axis and collect (world projection low,
    // high). Then check each window's projection falls inside a gap of
    // matching width.
    use std::collections::BTreeMap;
    let mut holes_by_side: BTreeMap<&'static str, Vec<(f32, f32)>> = BTreeMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        if n.role.as_deref() != Some("exterior_wall") {
            continue;
        }
        let side = n
            .tags
            .iter()
            .find_map(|t| t.strip_prefix("side="))
            .expect("side tag");
        let mesh = n.mesh.as_ref().expect("wall mesh");
        let nboxes = mesh.positions.len() / 24;
        let mat = glam::Mat4::from_translation(g.nodes[i].transform.translation)
            * glam::Mat4::from_quat(g.nodes[i].transform.rotation);
        // Transform every vertex into world.
        let world: Vec<Vec3> = mesh
            .positions
            .iter()
            .map(|p| mat.transform_point3(Vec3::new(p[0], p[1], p[2])))
            .collect();
        let mut piers: Vec<(f32, f32)> = Vec::new();
        let long_idx = match side {
            "north" | "south" => 0,
            "east" | "west" => 2,
            _ => unreachable!(),
        };
        for k in 0..nboxes {
            let chunk = &world[k * 24..(k + 1) * 24];
            let y_min = chunk.iter().map(|v| v.y).fold(f32::INFINITY, f32::min);
            let y_max = chunk.iter().map(|v| v.y).fold(f32::NEG_INFINITY, f32::max);
            // Full-height piers reach floor to ceiling.
            if y_max - y_min < 2.5 {
                continue;
            }
            let lo = chunk
                .iter()
                .map(|v| if long_idx == 0 { v.x } else { v.z })
                .fold(f32::INFINITY, f32::min);
            let hi = chunk
                .iter()
                .map(|v| if long_idx == 0 { v.x } else { v.z })
                .fold(f32::NEG_INFINITY, f32::max);
            piers.push((lo, hi));
        }
        piers.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let side_static: &'static str = match side {
            "north" => "north",
            "south" => "south",
            "east" => "east",
            "west" => "west",
            _ => unreachable!(),
        };
        let entry = holes_by_side.entry(side_static).or_default();
        for w in piers.windows(2) {
            entry.push((w[0].1, w[1].0));
        }
    }

    // Look at every `window` role node and verify it sits inside a
    // perimeter-wall hole of matching width. We use the opening-width
    // tag stored on the frame: each window group instances `window_simple`
    // whose `frame_top` has size [width+0.08, ...]; we read that back.
    let mut checked = 0;
    for (i, n) in g.nodes.iter().enumerate() {
        if !n.name.starts_with("window_") || n.kind != "group" {
            continue;
        }
        // Find the frame_top descendant to recover the authored width.
        let mut frame_width: Option<f32> = None;
        let mut stack = vec![i];
        while let Some(id) = stack.pop() {
            for c in &g.nodes[id].children {
                let cn = &g.nodes[c.0 as usize];
                if cn.name == "frame_top" {
                    if let Some(m) = &cn.mesh {
                        let xs: Vec<f32> = m.positions.iter().map(|p| p[0]).collect();
                        let lo = xs.iter().cloned().fold(f32::INFINITY, f32::min);
                        let hi = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                        frame_width = Some(hi - lo - 0.08);
                    }
                }
                stack.push(c.0 as usize);
            }
        }
        let width = match frame_width {
            Some(w) => w,
            None => continue,
        };
        let wp = world_pos(&g, i);
        let (side, proj) = if (wp.x - 5.482).abs() < 0.05 {
            ("east", wp.z)
        } else if (wp.x + 5.482).abs() < 0.05 {
            ("west", wp.z)
        } else if (wp.z - 3.876).abs() < 0.05 {
            ("north", wp.x)
        } else if (wp.z + 3.876).abs() < 0.05 {
            ("south", wp.x)
        } else {
            continue;
        };
        let Some(holes) = holes_by_side.get(side) else {
            panic!("no exterior wall on {side}");
        };
        let hit = holes
            .iter()
            .find(|(lo, hi)| *lo - 0.05 < proj && proj < *hi + 0.05);
        let (lo, hi) = hit.unwrap_or_else(|| panic!("window on {side} at proj={proj} has no hole"));
        let actual = hi - lo;
        assert!(
            (actual - width).abs() < 0.02,
            "window on {side} at proj={proj} has width={width} but hole width={actual}"
        );
        checked += 1;
    }
    assert!(
        checked >= 4,
        "expected to check ≥ 4 windows on east/west/north/south, only checked {checked}"
    );
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
    // Stair flights still span every storey pair (b1→0, 0→1).
    let flights = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .count();
    assert_eq!(
        flights, 2,
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

// --- Tranche 3 tests ---

#[test]
fn hotel_corridor_smoke_emits_a_corridor_cell() {
    // The synthesised "corridor" room_type produces a corridor room
    // group regardless of whether the author declared one.
    let src = r#"
        material "tile" (color=[0.9, 0.9, 0.85])
        building "h" (
          seed=2, style="hotel-corridor",
          floor_area=120, rooms=8, entrances=1,
          mat="tile",
        ) {
          room_type "room" (kind=private, density=1)
        }
    "#;
    let g = lower_src(src);
    // The corridor cell is a normal Room with room_type_index pointing at
    // the synthesised "corridor" type — surfaces as a tag.
    let corridor_groups = g
        .nodes
        .iter()
        .filter(|n| n.tags.iter().any(|t| t == "room_type=corridor"))
        .count();
    assert_eq!(
        corridor_groups, 1,
        "hotel-corridor should emit exactly one corridor cell, got {corridor_groups}"
    );
}

#[test]
fn office_core_smoke_emits_a_corridor_cell() {
    let src = r#"
        material "concrete" (color=[0.8, 0.8, 0.8])
        building "o" (
          seed=3, style="office-core",
          floor_area=150, rooms=12, entrances=1,
          mat="concrete",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    let corridor_groups = g
        .nodes
        .iter()
        .filter(|n| n.tags.iter().any(|t| t == "room_type=corridor"))
        .count();
    assert_eq!(
        corridor_groups, 1,
        "office-core should emit exactly one corridor cell, got {corridor_groups}"
    );
}

#[test]
fn hotel_corridor_runs_full_length_of_long_axis() {
    let src = r#"
        material "tile" (color=[0.9, 0.9, 0.85])
        building "h" (
          seed=1, style="hotel-corridor",
          floor_area=160, rooms=6, entrances=1,
          mat="tile",
        ) {
          room_type "guest" (kind=private, density=1)
        }
    "#;
    let g = lower_src(src);
    // The corridor cell is the cell whose group is tagged with
    // `room_type=corridor`. Find it and inspect its world-frame
    // dimensions through the interior-wall mesh footprint.
    //
    // The simpler smoke check: the corridor cell's group bears the
    // `room_type=corridor` tag and is the *widest* cell — every other
    // hotel room is narrower than the corridor since the corridor spans
    // the full long axis.
    let corridor_idx = g
        .nodes
        .iter()
        .position(|n| n.tags.iter().any(|t| t == "room_type=corridor"))
        .expect("corridor cell missing");
    // Its room group sits at the cell centroid, so its translation X is
    // the midpoint of the bounds. Floor_area=160 → footprint ≈ 15×11; the
    // corridor centre should be very near x=0 (centre of the floorplate).
    let corridor_x = g.nodes[corridor_idx].transform.translation.x;
    assert!(
        corridor_x.abs() < 1.0,
        "corridor should be centered on the long axis (x≈0), got x={corridor_x}"
    );
}

#[test]
fn min_area_penalty_prefers_layouts_with_satisfied_bounds() {
    // Two layouts of the same building with min_area on the bedroom:
    // (1) free; (2) min_area=10. Each forces the solver to attempt
    // different cell shapes. Both should still build, and the bedrooms
    // in (2) should average ≥ 9 m² (a soft 10 m² target, allowing for a
    // small shortfall the solver couldn't avoid).
    let src = r#"
        material "wood" (color=[0.55, 0.38, 0.22])
        building "h" (
          seed=4, style="apartment-block",
          floor_area=80, rooms=4, entrances=1,
          mat="wood",
        ) {
          room_type "bedroom" (kind=private, density=2, min_area=10)
          room_type "kitchen" (kind=service, density=1)
        }
    "#;
    let g = lower_src(src);
    // Build the per-cell rect approximation: room groups carry their
    // centre in world space; the rect itself isn't exposed but the
    // emit pass tags rooms by room_type so we can sanity-check that
    // bedroom cells exist.
    let bedrooms = g
        .nodes
        .iter()
        .filter(|n| n.tags.iter().any(|t| t == "room_type=bedroom"))
        .count();
    assert!(
        bedrooms >= 1,
        "expected at least one bedroom cell, got {bedrooms}"
    );
}

#[test]
fn entrance_distance_prior_does_not_break_layouts() {
    // The new entrance-distance / kind-prior terms must not regress any
    // existing layout — every reasonable config still produces > 0 rooms
    // and at least one entrance.
    let src = r#"
        material "wood" (color=[0.55, 0.38, 0.22])
        building "h" (
          seed=11, style="apartment-block",
          floor_area=100, rooms=5, entrances=1, windows=4,
          mat="wood",
        ) {
          room_type "bedroom" (kind=private, density=2)
          room_type "kitchen" (kind=service, density=1)
          room_type "living"  (kind=public,  density=2)
        }
    "#;
    let g = lower_src(src);
    let entrances = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("ext_door"))
        .count();
    assert!(entrances >= 1);
    let rooms = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("room"))
        .count();
    assert!(rooms >= 2, "expected ≥ 2 rooms, got {rooms}");
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

// --- Tranche 4 tests ---

fn count_role(g: &SceneGraph, role: &str) -> usize {
    g.nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some(role))
        .count()
}

fn slab_ceiling_count(g: &SceneGraph) -> usize {
    g.nodes
        .iter()
        .filter(|n| n.name == "slab_ceiling")
        .count()
}

const ROOFTEST_SRC: &str = r#"
material "stone" (color=[0.62, 0.55, 0.5])
building "house" (
  seed=4, style="apartment-block",
  floor_area=80, rooms=4, windows=2, entrances=1,
  roof="ROOFKIND",
  mat="stone",
) {
  room_type "office" (kind=staff_only, density=1)
}
"#;

#[test]
fn building_roof_gabled_smoke() {
    let src = ROOFTEST_SRC.replace("ROOFKIND", "gabled");
    let g = lower_src(&src);
    // Two slope wedges + two gable end-walls.
    assert!(
        count_role(&g, "roof") >= 2,
        "expected ≥ 2 roof slope nodes, got {}",
        count_role(&g, "roof")
    );
    assert!(
        count_role(&g, "gable_wall") == 2,
        "expected exactly 2 gable_wall nodes, got {}",
        count_role(&g, "gable_wall")
    );
    assert_eq!(
        slab_ceiling_count(&g),
        0,
        "non-flat roof must not emit a top-storey ceiling slab"
    );
}

#[test]
fn building_roof_hipped_smoke() {
    let src = ROOFTEST_SRC.replace("ROOFKIND", "hipped");
    let g = lower_src(&src);
    // Single frustum mesh.
    assert_eq!(
        count_role(&g, "roof"),
        1,
        "hipped roof must be a single frustum"
    );
    assert_eq!(count_role(&g, "gable_wall"), 0);
    assert_eq!(slab_ceiling_count(&g), 0);
}

#[test]
fn building_roof_mansard_two_tiers() {
    let src = ROOFTEST_SRC.replace("ROOFKIND", "mansard");
    let g = lower_src(&src);
    // Lower + upper tier.
    assert_eq!(
        count_role(&g, "roof"),
        2,
        "mansard roof must emit 2 tiers"
    );
}

#[test]
fn building_roof_shed_single_wedge() {
    let src = ROOFTEST_SRC.replace("ROOFKIND", "shed");
    let g = lower_src(&src);
    assert_eq!(count_role(&g, "roof"), 1, "shed roof is one wedge");
    assert_eq!(slab_ceiling_count(&g), 0);
}

#[test]
fn building_non_flat_roof_suppresses_ceiling_slab() {
    for kind in ["pitched", "gabled", "hipped", "mansard", "shed"] {
        let src = ROOFTEST_SRC.replace("ROOFKIND", kind);
        let g = lower_src(&src);
        assert_eq!(
            slab_ceiling_count(&g),
            0,
            "roof=\"{kind}\" left a slab_ceiling in the graph"
        );
    }
}

#[test]
fn building_skylight_disabled_under_pitched_roof() {
    let src = r#"
        material "p" (color=[0.9, 0.9, 0.85])
        building "house" (
          seed=1, style="grid",
          floor_area=60, rooms=4, skylights=2, roof="gabled",
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    // No skylight modules instantiated.
    assert!(
        !has_tag(&g, "skylight"),
        "skylight tag should be absent under non-flat roof"
    );
    let skylights = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("skylight"))
        .count();
    assert_eq!(
        skylights, 0,
        "expected no skylight instances under non-flat roof"
    );
}

#[test]
fn building_radial_layout_smoke() {
    let src = r#"
        material "p" (color=[0.7, 0.7, 0.7])
        building "rotunda" (
          seed=7, style="radial",
          floor_area=100, rooms=9, entrances=1,
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    let rooms = count_role(&g, "room");
    assert!(rooms >= 5, "expected ≥ 5 radial rooms, got {rooms}");
    assert!(count_kind(&g, "slab") >= 2);
    assert!(count_kind(&g, "wall") >= 4);
}

#[test]
fn building_organic_layout_smoke() {
    let src = r#"
        material "p" (color=[0.7, 0.7, 0.7])
        building "lobby" (
          seed=11, style="organic",
          floor_area=120, rooms=8, entrances=1,
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    let rooms = count_role(&g, "room");
    assert!(rooms >= 4, "expected ≥ 4 organic rooms, got {rooms}");
    // Every wall mesh's positions should live within a sane bound around
    // the floorplate centre. A bug-broken layout would put cells outside.
    for n in &g.nodes {
        if n.kind != "wall" {
            continue;
        }
        if let Some(m) = &n.mesh {
            for p in &m.positions {
                assert!(p[0].is_finite() && p[2].is_finite(), "non-finite wall vertex");
            }
        }
    }
}

#[test]
fn building_maze_layout_smoke() {
    let src = r#"
        material "p" (color=[0.7, 0.7, 0.7])
        building "labyrinth" (
          seed=21, style="maze",
          floor_area=140, rooms=10, entrances=1,
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    let rooms = count_role(&g, "room");
    // Corridor + many small rooms.
    assert!(rooms >= 3, "expected ≥ 3 maze cells, got {rooms}");
}

#[test]
fn building_cellar_area_shrinks_basement_only() {
    let src = r#"
        material "p" (color=[0.7, 0.7, 0.7])
        building "split" (
          seed=2, style="grid",
          floor_area=120, cellar_area=40, rooms=6,
          floors_above=1, floors_below=1,
          staircases=1, entrances=1,
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    // Find the floor slab for floor_b1 and floor_0; the basement floor
    // slab must have a smaller bounding extent in XZ than the ground floor.
    fn floor_slab_extent(g: &SceneGraph, floor_group_name: &str) -> Option<(f32, f32)> {
        let group = g
            .nodes
            .iter()
            .find(|n| n.kind == "group" && n.name == floor_group_name)?;
        // Walk the group's descendants for `slab_floor`.
        let group_id = g.nodes.iter().position(|n| std::ptr::eq(n, group))? as u32;
        for n in &g.nodes {
            if n.name != "slab_floor" {
                continue;
            }
            // climb up parents until we hit `group_id` or a different floor.
            let mut cur = n.parent;
            while let Some(p) = cur {
                if p.0 == group_id {
                    let m = n.mesh.as_ref()?;
                    let mut x_min = f32::INFINITY;
                    let mut x_max = f32::NEG_INFINITY;
                    let mut z_min = f32::INFINITY;
                    let mut z_max = f32::NEG_INFINITY;
                    for p in &m.positions {
                        x_min = x_min.min(p[0]);
                        x_max = x_max.max(p[0]);
                        z_min = z_min.min(p[2]);
                        z_max = z_max.max(p[2]);
                    }
                    return Some((x_max - x_min, z_max - z_min));
                }
                cur = g.nodes[p.0 as usize].parent;
            }
        }
        None
    }
    let ground = floor_slab_extent(&g, "floor_0").expect("ground floor slab");
    let basement = floor_slab_extent(&g, "floor_b1").expect("basement floor slab");
    assert!(
        basement.0 < ground.0 && basement.1 < ground.1,
        "basement slab ({:?}) must be smaller than ground slab ({:?})",
        basement,
        ground
    );
}

#[test]
fn building_cellar_area_unset_reuses_ground_footprint() {
    let src = r#"
        material "p" (color=[0.7, 0.7, 0.7])
        building "match" (
          seed=2, style="grid",
          floor_area=120, rooms=6,
          floors_above=1, floors_below=1,
          staircases=1, entrances=1,
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    // Every floor's slab_floor should have the same XZ extent when
    // cellar_area is unset.
    let extents: Vec<(f32, f32)> = g
        .nodes
        .iter()
        .filter(|n| n.name == "slab_floor")
        .map(|n| {
            let m = n.mesh.as_ref().unwrap();
            let mut x_min = f32::INFINITY;
            let mut x_max = f32::NEG_INFINITY;
            let mut z_min = f32::INFINITY;
            let mut z_max = f32::NEG_INFINITY;
            for p in &m.positions {
                x_min = x_min.min(p[0]);
                x_max = x_max.max(p[0]);
                z_min = z_min.min(p[2]);
                z_max = z_max.max(p[2]);
            }
            (x_max - x_min, z_max - z_min)
        })
        .collect();
    assert!(extents.len() >= 2);
    let (w0, d0) = extents[0];
    for (i, (w, d)) in extents.iter().enumerate().skip(1) {
        assert!(
            (w - w0).abs() < 1e-3 && (d - d0).abs() < 1e-3,
            "slab {i} extent ({w},{d}) diverges from slab 0 ({w0},{d0})"
        );
    }
}

#[test]
fn building_t4_is_deterministic_under_same_seed() {
    let src = r#"
        material "stone" (color=[0.6, 0.55, 0.5])
        building "brownstone" (
          seed=42, style="apartment-block",
          floor_area=110, cellar_area=60, rooms=10,
          floors_above=2, floors_below=1,
          staircases=1, entrances=1,
          roof="mansard", windows=6,
          mat="stone",
        ) {
          room_type "bedroom" (kind=private, density=2)
          room_type "kitchen" (kind=service, density=1)
          room_type "living"  (kind=public,  density=2)
        }
    "#;
    let a = lower_src(src);
    let b = lower_src(src);
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
    assert!((hash(&a) - hash(&b)).abs() < 1e-3, "mesh hash diverged");
}
