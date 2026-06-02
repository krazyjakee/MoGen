//! End-to-end lowering tests for the `cave` node.

use crate::lower::lower;
use crate::parser::parse;
use mogen_core::SceneGraph;

fn lower_src(src: &str) -> SceneGraph {
    let ast = parse(src).expect("parse");
    lower(&ast).expect("lower")
}

fn count_role(g: &SceneGraph, role: &str) -> usize {
    g.nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some(role))
        .count()
}

const BASIC: &str = r#"
cave "den" (
  seed=3,
  size=[20, 9, 20],
  chambers=5,
  levels=2,
  resolution=48,
  entrances=1,
)
"#;

const BASIC_ALT_SEED: &str = r#"
cave "den" (
  seed=99,
  size=[20, 9, 20],
  chambers=5,
  levels=2,
  resolution=48,
  entrances=1,
)
"#;

#[test]
fn cave_emits_a_rock_shell() {
    let g = lower_src(BASIC);
    let rock = g
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .expect("rock node");
    let mesh = rock.mesh.as_ref().expect("rock mesh");
    assert!(
        mesh.positions.len() > 100,
        "expected a substantial rock mesh, got {} verts",
        mesh.positions.len()
    );
}

#[test]
fn cave_subtree_is_non_editable() {
    let g = lower_src(BASIC);
    // The wrapper stays editable; everything generated under it does not.
    let rock = g
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    assert!(!rock.editable, "generated rock should be non-editable");
}

#[test]
fn cave_rock_gets_a_trimesh_collider() {
    let g = lower_src(BASIC);
    let rock = g
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    assert!(rock.collider.is_some(), "rock should carry a collider");
}

#[test]
fn cave_is_deterministic_under_same_seed() {
    let a = lower_src(BASIC);
    let b = lower_src(BASIC);
    assert_eq!(a.nodes.len(), b.nodes.len());
    let ra = a
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    let rb = b
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    assert_eq!(
        ra.mesh.as_ref().unwrap().positions.len(),
        rb.mesh.as_ref().unwrap().positions.len()
    );
}

#[test]
fn cave_seed_changes_geometry() {
    let a = lower_src(BASIC);
    let b = lower_src(BASIC_ALT_SEED);
    let ra = a
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    let rb = b
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    // Different seed → different chamber layout → (almost surely) a different
    // vertex count or at least different positions.
    let pa = &ra.mesh.as_ref().unwrap().positions;
    let pb = &rb.mesh.as_ref().unwrap().positions;
    assert!(pa != pb, "different seeds should produce different caves");
}

#[test]
fn cave_decorations_emit_nodes() {
    let src = r#"
cave "grotto" (
  seed=4,
  size=[22, 10, 22],
  chambers=5,
  resolution=40,
  stalagmites=6,
  stalactites=4,
  rock_piles=2,
  pools=1,
  lakes=1,
)
"#;
    let g = lower_src(src);
    assert_eq!(count_role(&g, "stalagmite"), 6);
    assert_eq!(count_role(&g, "stalactite"), 4);
    assert_eq!(count_role(&g, "rock_pile"), 2);
    assert_eq!(count_role(&g, "pool"), 1);
    assert_eq!(count_role(&g, "lake"), 1);
}

#[test]
fn cave_feature_overrides_count_and_material() {
    let src = r#"
material "ice" (color=[0.7, 0.85, 0.95])
cave "ice_cave" (
  seed=2,
  size=[20, 9, 20],
  chambers=4,
  resolution=40,
  stalagmites=2,
) {
  feature "spikes" (kind=stalagmite, count=5, mat="ice", min_size=0.4, max_size=0.9)
}
"#;
    let g = lower_src(src);
    // The feature's count=5 overrides the top-level stalagmites=2.
    assert_eq!(count_role(&g, "stalagmite"), 5);
    let ice = g.find_material("ice").expect("ice material");
    let any_ice = g
        .nodes
        .iter()
        .any(|n| n.role.as_deref() == Some("stalagmite") && n.material == Some(ice));
    assert!(any_ice, "stalagmites should bind the overridden material");
}

#[test]
fn cave_water_uses_default_water_material() {
    let src = r#"
cave "spring" (seed=1, size=[18, 8, 18], chambers=4, resolution=40, pools=2)
"#;
    let g = lower_src(src);
    let water = g.find_material("cave_water").expect("default water material");
    let pools = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("pool"))
        .count();
    assert_eq!(pools, 2);
    assert!(g
        .nodes
        .iter()
        .any(|n| n.role.as_deref() == Some("pool") && n.material == Some(water)));
}
