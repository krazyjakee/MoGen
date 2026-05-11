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
