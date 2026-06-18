use mogen_core::{SceneGraph, Transform};

use super::config::{ColliderMode, DungeonCfg};
use super::{emit, generate, materials, poi};
use crate::ast::Node;
use crate::lower::procedural::finish_procedural;

fn ast_node() -> Node {
    Node {
        kind: "dungeon".into(),
        name: Some("dungeon".into()),
        attrs: vec![],
        children: vec![],
        span: Default::default(),
        kind_span: Default::default(),
        use_id: None,
        origin: None,
    }
}

fn cfg(levels: u32) -> DungeonCfg {
    DungeonCfg {
        seed: 7,
        mat_style: String::new(),
        size: [48.0, 4.0, 48.0],
        cell: 4.0,
        levels,
        rooms: 6,
        room_min: 2,
        room_max: 5,
        spacing: 1,
        corridor_width: 1,
        loops: 1,
        stairs: 1,
        entrances_per_floor: vec![1],
        wall_thickness: 0.4,
        floor_thickness: 0.4,
        ceilings: true,
        colliders: ColliderMode::All,
        prop_spots: 4,
        lod_scale: 1.0,
        debug_hide_roof: false,
        debug_render_floor: None,
        debug_show_poi: false,
    }
}

fn build(c: &DungeonCfg) -> SceneGraph {
    let node = ast_node();
    let mut graph = SceneGraph::new();
    let parent = graph.add_root("dungeon", "dungeon", Transform::IDENTITY);
    materials::ensure_defaults(&mut graph, None);
    let layout = generate::generate(c);
    let pre = graph.nodes.len();
    emit::emit(&node, c, &layout, parent, &mut graph);
    poi::emit_pois(&node, c, &layout, parent, &mut graph);
    finish_procedural(&mut graph, pre);
    graph
}

#[test]
fn placement_is_deterministic() {
    let c = cfg(2);
    let a = generate::generate(&c);
    let b = generate::generate(&c);
    assert_eq!(a.rooms.len(), b.rooms.len());
    for (ra, rb) in a.rooms.iter().zip(&b.rooms) {
        assert_eq!((ra.x0, ra.z0, ra.w, ra.d), (rb.x0, rb.z0, rb.w, rb.d));
    }
    assert_eq!(a.stairs.len(), b.stairs.len());
}

#[test]
fn rooms_do_not_overlap_with_spacing() {
    let layout = generate::generate(&cfg(1));
    let rooms = &layout.rooms;
    for (i, a) in rooms.iter().enumerate() {
        for b in &rooms[i + 1..] {
            if a.level != b.level {
                continue;
            }
            // No overlap: separated on at least one axis.
            let sep_x = a.x0 + a.w <= b.x0 || b.x0 + b.w <= a.x0;
            let sep_z = a.z0 + a.d <= b.z0 || b.z0 + b.d <= a.z0;
            assert!(sep_x || sep_z, "rooms overlap: {a:?} vs {b:?}");
        }
    }
}

#[test]
fn single_level_has_no_stairs() {
    assert!(generate::generate(&cfg(1)).stairs.is_empty());
}

#[test]
fn multi_level_threads_a_staircase() {
    // With several rooms per level the foot/head floor-overlap test should find
    // at least one valid run.
    let layout = generate::generate(&cfg(3));
    assert!(
        !layout.stairs.is_empty(),
        "expected at least one staircase across 3 levels"
    );
    for s in &layout.stairs {
        let foot = *s.cells.first().unwrap();
        let head = *s.cells.last().unwrap();
        assert!(layout.is_floor(s.lower_level, foot.0, foot.1));
        assert!(layout.is_floor(s.lower_level + 1, head.0, head.1));
    }
}

#[test]
fn emits_floor_wall_and_step_geometry() {
    let graph = build(&cfg(2));
    let count_role = |role: &str| {
        graph
            .nodes
            .iter()
            .filter(|n| n.role.as_deref() == Some(role))
            .count()
    };
    assert!(count_role("floor") > 0, "no floor decks emitted");
    assert!(count_role("wall") > 0, "no walls emitted");
    assert!(count_role("stair") > 0, "no steps emitted");
}

#[test]
fn walls_span_full_storey_pitch() {
    // Walls must cover the full pitch (room height + floor thickness) so the
    // storey-boundary band stays sealed even where a staircase opening removes
    // the floor slab — otherwise a thin slit opens around stairs.
    let c = cfg(2);
    let graph = build(&c);
    let wall_h = c.size[1] + c.floor_thickness;
    let mut saw_wall = false;
    for n in graph.nodes.iter().filter(|n| n.role.as_deref() == Some("wall")) {
        let m = n.mesh.as_ref().unwrap();
        let lo = m.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        let hi = m
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (hi - lo - wall_h).abs() < 1e-3,
            "wall height {} != storey pitch {wall_h}",
            hi - lo
        );
        saw_wall = true;
    }
    assert!(saw_wall, "expected at least one wall");
}

#[test]
fn meshes_are_non_degenerate() {
    // Every emitted box must have geometry — a watertight closed solid has 24
    // verts / 36 indices from box_mesh.
    let graph = build(&cfg(2));
    for n in &graph.nodes {
        if let Some(m) = &n.mesh {
            assert!(!m.positions.is_empty(), "empty mesh on {:?}", n.role);
            assert!(!m.indices.is_empty(), "no indices on {:?}", n.role);
        }
    }
}

#[test]
fn entrance_is_carved_and_marked() {
    let layout = generate::generate(&cfg(1));
    assert_eq!(layout.entrances.len(), 1, "expected one ground entrance by default");
    let e = layout.entrances[0];
    assert_eq!(e.level, 0, "default entrance should be on the ground level");
    // The threshold sits on the grid border and is walkable floor.
    let on_border = e.i == 0 || e.i == layout.gw - 1 || e.j == 0 || e.j == layout.gd - 1;
    assert!(on_border, "entrance not on the grid border: {e:?}");
    assert!(layout.is_floor(0, e.i, e.j), "entrance cell is not floor: {e:?}");

    let graph = build(&cfg(1));
    let entrances = graph
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("entrance"))
        .count();
    assert_eq!(entrances, 1, "expected exactly one entrance marker");
}

#[test]
fn entrances_per_floor_selects_specific_levels() {
    // [1, 0, 1] → a door on the ground and top floors, none on the middle.
    let mut c = cfg(3);
    c.entrances_per_floor = vec![1, 0, 1];
    let layout = generate::generate(&c);
    let levels: Vec<usize> = layout.entrances.iter().map(|e| e.level).collect();
    assert!(levels.contains(&0), "expected a ground-floor entrance: {levels:?}");
    assert!(levels.contains(&2), "expected a top-floor entrance: {levels:?}");
    assert!(!levels.contains(&1), "middle floor should have no entrance: {levels:?}");
    assert_eq!(levels.len(), 2, "expected exactly two entrances: {levels:?}");
}

#[test]
fn top_floor_stairwell_is_roofed() {
    let c = cfg(2);
    let layout = generate::generate(&c);
    assert!(!layout.stairs.is_empty(), "expected a staircase");
    let graph = build(&c);

    let cell = c.cell;
    let pitch = c.size[1] + c.floor_thickness;
    let half_w = layout.gw as f32 * cell * 0.5;
    let half_d = layout.gd as f32 * cell * 0.5;
    let cx = |i: i32| -half_w + (i as f32 + 0.5) * cell;
    let cz = |j: i32| -half_d + (j as f32 + 0.5) * cell;
    // Centre Y of the roof slab (topmost deck).
    let roof_y = layout.levels as f32 * pitch + c.floor_thickness * 0.5;

    for stair in &layout.stairs {
        // Only flights that land on the top level need a roof check here.
        if stair.lower_level + 1 != layout.levels - 1 {
            continue;
        }
        let (hi, hj) = *stair.cells.last().unwrap();
        let (ex, ez) = (cx(hi), cz(hj));
        let roofed = graph.nodes.iter().any(|n| {
            n.role.as_deref() == Some("ceiling")
                && (n.transform.translation.x - ex).abs() < 0.01
                && (n.transform.translation.z - ez).abs() < 0.01
                && (n.transform.translation.y - roof_y).abs() < 0.01
        });
        assert!(roofed, "stair landing at ({hi}, {hj}) has no roof slab");
    }
}

#[test]
fn debug_render_floor_isolates_one_level() {
    let mut c = cfg(3);
    c.debug_render_floor = Some(1);
    let graph = build(&c);

    let pitch = c.size[1] + c.floor_thickness;
    let ft = c.floor_thickness;
    // Floor slab of level L sits at y = L*pitch + ft/2.
    let level_floor_y = |l: usize| l as f32 * pitch + ft * 0.5;

    let floors: Vec<f32> = graph
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("floor"))
        .map(|n| n.transform.translation.y)
        .collect();
    assert!(!floors.is_empty(), "no floor emitted for the isolated level");
    for y in &floors {
        assert!(
            (y - level_floor_y(1)).abs() < 0.01,
            "floor slab at y={y} is not level 1's floor"
        );
    }

    // Isolation emits no ceiling/roof, so you can see in from above.
    let ceilings = graph
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("ceiling"))
        .count();
    assert_eq!(ceilings, 0, "isolation should emit no ceiling/roof");

    // Wrapper tagged `floating` so the connectivity check skips the cut-away.
    assert!(
        graph.nodes[0].tags.iter().any(|t| t == "floating"),
        "isolated dungeon wrapper must be tagged `floating`"
    );
}

#[test]
fn debug_render_floor_out_of_range_renders_all() {
    let mut c = cfg(2);
    c.debug_render_floor = Some(9); // no such level
    let graph = build(&c);
    // Falls back to rendering every level: ceilings are present and the
    // wrapper is not tagged `floating`.
    assert!(
        graph
            .nodes
            .iter()
            .any(|n| n.role.as_deref() == Some("ceiling")),
        "out-of-range isolation should render all levels (ceilings present)"
    );
    assert!(
        !graph.nodes[0].tags.iter().any(|t| t == "floating"),
        "out-of-range isolation must not tag the wrapper `floating`"
    );
}

#[test]
fn spawn_marker_present_on_ground() {
    let graph = build(&cfg(1));
    let spawns = graph
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("spawn"))
        .count();
    assert_eq!(spawns, 1, "expected exactly one spawn marker");
}

#[test]
fn dungeon_subtree_is_non_editable() {
    let graph = build(&cfg(1));
    // Every mesh node generated below the wrapper must be non-editable —
    // a rebuild from the seed would wipe any hand-edit.
    for n in &graph.nodes {
        if n.mesh.is_some() {
            assert!(!n.editable, "generated mesh {:?} should be non-editable", n.role);
        }
    }
}

#[test]
fn colliders_none_leaves_everything_collider_free() {
    let mut c = cfg(1);
    c.colliders = ColliderMode::None;
    let graph = build(&c);
    assert!(
        graph.nodes.iter().all(|n| n.collider.is_none()),
        "colliders=None should leave every dungeon node collider-free"
    );
}

#[test]
fn colliders_all_adds_trimesh_to_every_solid() {
    let c = cfg(1); // ColliderMode::All by default
    let graph = build(&c);
    let solids: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.mesh.is_some())
        .collect();
    assert!(!solids.is_empty(), "expected mesh nodes");
    for n in &solids {
        assert!(
            n.collider.is_some(),
            "colliders=All: solid {:?} should carry a collider",
            n.role
        );
    }
}

#[test]
fn seed_changes_room_layout() {
    let c1 = cfg(1);
    let mut c2 = cfg(1);
    c2.seed = 42;
    let a = generate::generate(&c1);
    let b = generate::generate(&c2);
    let positions_a: Vec<_> = a.rooms.iter().map(|r| (r.x0, r.z0)).collect();
    let positions_b: Vec<_> = b.rooms.iter().map(|r| (r.x0, r.z0)).collect();
    assert_ne!(
        positions_a, positions_b,
        "different seeds should produce different room layouts"
    );
}

#[test]
fn prop_spots_are_placed() {
    let mut c = cfg(1);
    c.prop_spots = 5;
    let graph = build(&c);
    let spots = graph
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("prop_spot"))
        .count();
    assert_eq!(spots, 5, "expected exactly 5 prop_spot markers");
    // POI markers carry no geometry.
    for n in graph.nodes.iter().filter(|n| n.role.as_deref() == Some("prop_spot")) {
        assert!(n.mesh.is_none(), "prop_spot markers carry no geometry");
    }
}

#[test]
fn ceilings_false_emits_no_ceiling_nodes() {
    let mut c = cfg(2);
    c.ceilings = false;
    let graph = build(&c);
    let ceilings = graph
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("ceiling"))
        .count();
    assert_eq!(ceilings, 0, "ceilings=false should emit no ceiling nodes");
}
