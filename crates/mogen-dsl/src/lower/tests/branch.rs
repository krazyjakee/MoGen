use super::*;
use crate::lower::*;
use glam::Vec3;

#[test]
fn branch_expands_to_seg_and_leaf_nodes() {
    // depth=2, splits=2 → 1 + 2 + 4 = 7 segments and 4 leaves.
    let g = lower_src(
        r#"
        material "bark" (color=[0.4, 0.25, 0.15])
        material "leaf" (color=[0.3, 0.6, 0.2], alpha_mode="mask", double_sided=1)
        scene {
          branch "tree" (
            length=1.0, radius=0.1, depth=2, splits=2,
            length_falloff=0.7, radius_falloff=0.6,
            branch_angle=30, jitter=0.0,
            leaves=1, leaf_size=0.3, leaf_mat="leaf",
            mat="bark"
          )
        }
        "#,
    );
    let segs = g.nodes.iter().filter(|n| n.kind == "branch_seg").count();
    let leaves = g.nodes.iter().filter(|n| n.kind == "leaf_card").count();
    assert_eq!(segs, 7, "expected 7 branch segments at depth=2 splits=2, got {segs}");
    assert_eq!(leaves, 4, "expected 4 leaf cards at depth=0 tips, got {leaves}");
    // Bark inherited on segments; explicit leaf material on leaves.
    let bark = g.find_material("bark").expect("bark");
    let leaf = g.find_material("leaf").expect("leaf");
    for n in &g.nodes {
        if n.kind == "branch_seg" {
            assert_eq!(n.material, Some(bark), "segment {} should inherit bark", n.name);
        } else if n.kind == "leaf_card" {
            assert_eq!(n.material, Some(leaf), "leaf {} should bind leaf material", n.name);
        }
    }
}

#[test]
fn branch_is_deterministic_for_a_given_seed() {
    let a = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=3, splits=3, jitter=0.5, seed=7) }"#,
    );
    let b = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=3, splits=3, jitter=0.5, seed=7) }"#,
    );
    assert_eq!(a.nodes.len(), b.nodes.len());
    for (na, nb) in a.nodes.iter().zip(b.nodes.iter()) {
        assert_eq!(na.kind, nb.kind, "kind diverges for seeded branch");
        assert_eq!(na.transform.translation, nb.transform.translation);
    }
}

#[test]
fn branch_seed_changes_geometry() {
    let a = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=3, splits=3, jitter=0.5, seed=1) }"#,
    );
    let b = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=3, splits=3, jitter=0.5, seed=2) }"#,
    );
    // Same node count, but at least one transform should differ.
    assert_eq!(a.nodes.len(), b.nodes.len());
    let any_diff = a
        .nodes
        .iter()
        .zip(b.nodes.iter())
        .any(|(x, y)| x.transform.translation != y.transform.translation);
    assert!(any_diff, "different seeds should produce different forks");
}

#[test]
fn branch_no_leaves_when_disabled() {
    let g = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=2, splits=2, leaves=0) }"#,
    );
    let leaves = g.nodes.iter().filter(|n| n.kind == "leaf_card").count();
    assert_eq!(leaves, 0, "leaves=0 should suppress leaf cards");
}

#[test]
fn branch_leaves_align_to_branch_tip() {
    // With branch_angle=0 and bend=0 every segment grows along +Y, so a
    // depth=1 split should keep its single leaf pointing up — the leaf
    // card's local +Y should resolve to world +Y after composition.
    let g = lower_src(
        r#"scene {
            branch "t" (
                length=1, radius=0.1, depth=1, splits=1,
                branch_angle=0, bend=0, tropism=0, jitter=0,
                leaves=1, leaf_size=0.2
            )
        }"#,
    );
    // Find the leaf node and walk its world transform.
    let world = g.world_transforms();
    let leaf_idx = g
        .nodes
        .iter()
        .position(|n| n.kind == "leaf_card")
        .expect("leaf card present");
    let m = world[leaf_idx];
    let up = m.transform_vector3(Vec3::Y).normalize();
    assert!(up.y > 0.95, "leaf +Y should align with world +Y, got {up:?}");
}

#[test]
fn branch_segments_are_marked_non_editable() {
    let g = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=1, splits=2) }"#,
    );
    for n in &g.nodes {
        if matches!(n.kind.as_str(), "branch_seg" | "leaf_card") {
            assert!(!n.editable, "{} should be non-editable", n.name);
        }
    }
    // Wrapper itself stays editable so the user can tweak `branch` attrs.
    let wrapper = g
        .nodes
        .iter()
        .find(|n| n.kind == "branch")
        .expect("wrapper present");
    assert!(wrapper.editable, "branch wrapper should remain editable");
}
