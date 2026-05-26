use super::*;
use crate::lower::*;
use glam::Vec3;

#[test]
fn scalar_size_expands_to_cube() {
    let g = lower_src(r#"scene { box "b" (size=2) }"#);
    let (min, max) = mesh_aabb(&g, "b");
    assert!((min - Vec3::splat(-1.0)).abs().max_element() < 1e-5);
    assert!((max - Vec3::splat(1.0)).abs().max_element() < 1e-5);
}

#[test]
fn whd_shortcuts_populate_size() {
    let g = lower_src(r#"scene { box "b" (w=2, h=4, d=6) }"#);
    let (min, max) = mesh_aabb(&g, "b");
    assert!((max.x - min.x - 2.0).abs() < 1e-5);
    assert!((max.y - min.y - 4.0).abs() < 1e-5);
    assert!((max.z - min.z - 6.0).abs() < 1e-5);
}

#[test]
fn whd_overrides_individual_size_components() {
    let g = lower_src(r#"scene { box "b" (size=[1, 1, 1], h=3) }"#);
    let (min, max) = mesh_aabb(&g, "b");
    assert!((max.x - min.x - 1.0).abs() < 1e-5);
    assert!((max.y - min.y - 3.0).abs() < 1e-5);
    assert!((max.z - min.z - 1.0).abs() < 1e-5);
}

#[test]
fn xyz_shortcuts_set_translation() {
    let g = lower_src(r#"scene { box "b" (y=1.5, size=1) }"#);
    let t = find_mesh_node(&g, "b").transform.translation;
    assert!((t - Vec3::new(0.0, 1.5, 0.0)).abs().max_element() < 1e-5);
}

#[test]
fn rxyz_shortcuts_set_rotation() {
    let g = lower_src(r#"scene { box "b" (ry=90, size=1) }"#);
    let q = find_mesh_node(&g, "b").transform.rotation;
    // 90° around Y rotates +X to -Z.
    let v = q * Vec3::X;
    assert!((v - Vec3::new(0.0, 0.0, -1.0)).abs().max_element() < 1e-4,
        "got {v:?}");
}

#[test]
fn anchor_bottom_places_mesh_above_origin() {
    let g = lower_src(r#"scene { box "b" (size=2, anchor=bottom) }"#);
    let (min, max) = mesh_aabb(&g, "b");
    assert!(min.y.abs() < 1e-5, "expected bottom on y=0, got {min:?}");
    assert!((max.y - 2.0).abs() < 1e-5);
}

#[test]
fn anchor_corner_combines_axes() {
    let g = lower_src(r#"scene { box "b" (size=2, anchor=bottom_left_front) }"#);
    let (min, _) = mesh_aabb(&g, "b");
    // All three mins should sit at 0.
    assert!(min.x.abs() < 1e-5 && min.y.abs() < 1e-5 && min.z.abs() < 1e-5,
        "expected all-mins at 0, got {min:?}");
}

#[test]
fn anchor_shifts_default_connectors() {
    // Anchor=bottom puts the box's bottom face on y=0; the `bottom`
    // default connector must follow — otherwise attach math breaks.
    let g = lower_src(r#"scene { box "b" (size=2, anchor=bottom) }"#);
    let n = find_mesh_node(&g, "b");
    let bottom = n.connectors.iter().find(|c| c.name == "bottom")
        .expect("missing bottom connector");
    assert!(bottom.pos.y.abs() < 1e-5,
        "bottom connector should be at y=0, got {:?}", bottom.pos);
    let top = n.connectors.iter().find(|c| c.name == "top")
        .expect("missing top connector");
    assert!((top.pos.y - 2.0).abs() < 1e-5,
        "top connector should be at y=2, got {:?}", top.pos);
}

#[test]
fn slab_defaults_to_bottom_anchor() {
    let g = lower_src(r#"scene { slab "floor" (size=[2, 0.2, 2]) }"#);
    let (min, max) = mesh_aabb(&g, "floor");
    assert!(min.y.abs() < 1e-5, "slab should sit on y=0");
    assert!((max.y - 0.2).abs() < 1e-5);
}

#[test]
fn panel_defaults_to_back_anchor() {
    let g = lower_src(r#"scene { panel "p" (size=[2, 2, 0.1]) }"#);
    let (min, max) = mesh_aabb(&g, "p");
    // Back face is the +Z face. Anchor=back means the +Z face lands at z=0.
    assert!(max.z.abs() < 1e-5, "panel back face should be at z=0, got max.z={}", max.z);
    assert!((min.z + 0.1).abs() < 1e-5);
}

#[test]
fn from_to_derives_size_and_pos() {
    let g = lower_src(r#"scene { box "b" (from=[-1, 0, -1], to=[1, 2, 1]) }"#);
    let t = find_mesh_node(&g, "b").transform.translation;
    assert!((t - Vec3::new(0.0, 1.0, 0.0)).abs().max_element() < 1e-5);
    let (min, max) = mesh_aabb(&g, "b");
    assert!((max - min - Vec3::new(2.0, 2.0, 2.0)).abs().max_element() < 1e-5);
}

#[test]
fn stack_y_packs_children_bottom_up() {
    let g = lower_src(
        r#"
        scene {
          stack "tower" (axis=y) {
            box "a" (size=[1, 1, 1])
            box "b" (size=[1, 2, 1])
            box "c" (size=[1, 0.5, 1])
          }
        }
        "#,
    );
    let ay = find_mesh_node(&g, "a").transform.translation.y;
    let by = find_mesh_node(&g, "b").transform.translation.y;
    let cy = find_mesh_node(&g, "c").transform.translation.y;
    // Each box's *center* sits at cumulative_base + half_height.
    // a: 0 + 0.5 = 0.5; b: 1 + 1 = 2.0; c: 3 + 0.25 = 3.25.
    assert!((ay - 0.5).abs() < 1e-4, "got a.y={ay}");
    assert!((by - 2.0).abs() < 1e-4, "got b.y={by}");
    assert!((cy - 3.25).abs() < 1e-4, "got c.y={cy}");
}

#[test]
fn stack_gap_inserts_space_between_children() {
    let g = lower_src(
        r#"
        scene {
          stack "s" (axis=y, gap=0.5) {
            box "a" (size=[1, 1, 1])
            box "b" (size=[1, 1, 1])
          }
        }
        "#,
    );
    let ay = find_mesh_node(&g, "a").transform.translation.y;
    let by = find_mesh_node(&g, "b").transform.translation.y;
    // a center at 0.5; gap of 0.5 → b center at 1 + 0.5 + 0.5 = 2.0.
    assert!((by - ay - 1.5).abs() < 1e-4, "gap not applied: a={ay} b={by}");
}

#[test]
fn grid_replicates_children() {
    let g = lower_src(
        r#"
        scene {
          grid "tiles" (count=[3, 1, 2], step=[1, 0, 1]) {
            box "t" (size=[0.9, 0.1, 0.9])
          }
        }
        "#,
    );
    // Expect 3*1*2 = 6 instance wrappers, each with a nested box.
    let t_count = g.nodes.iter().filter(|n| n.name == "t").count();
    assert_eq!(t_count, 6, "grid should produce 6 tiles, got {t_count}");
}

#[test]
fn grid_attach_applies_uniformly_per_instance() {
    // Regression: attach inside a grid body must not leak to the global
    // resolve_attaches pass — otherwise the first instance gets attach
    // applied twice (once globally, once per-instance) and ends up at
    // 2× the offset of the others.
    let g = lower_src(
        r#"
        scene {
          grid "row" (count=[4, 1, 1], step=[0.5, 0, 0]) {
            sphere "body" (radius=0.1)
            cylinder "cap" (radius=0.05, height=0.02)
            attach (parent="body", child="cap", socket="top", plug="bottom")
          }
        }
        "#,
    );
    let cap_ys: Vec<f32> = g
        .nodes
        .iter()
        .filter(|n| n.name == "cap")
        .map(|n| n.transform.translation.y)
        .collect();
    assert_eq!(cap_ys.len(), 4, "expected 4 cap instances, got {}", cap_ys.len());
    let first = cap_ys[0];
    for (i, y) in cap_ys.iter().enumerate() {
        assert!(
            (y - first).abs() < 1e-5,
            "cap[{i}] y={y} differs from cap[0] y={first} — attach applied unevenly across grid instances"
        );
    }
}

#[test]
fn relative_placement_above_snaps_flush() {
    let g = lower_src(
        r#"
        scene {
          group "world" {
            box "base" (size=[2, 1, 2])
            box "hat"  (size=[1, 1, 1], above="base")
          }
        }
        "#,
    );
    // base center y=0, top y=0.5. hat bottom flush → center at y=1.0.
    let hat_y = find_mesh_node(&g, "hat").transform.translation.y;
    assert!((hat_y - 1.0).abs() < 1e-4, "hat should be at y=1.0, got {hat_y}");
}

#[test]
fn relative_placement_honors_gap() {
    let g = lower_src(
        r#"
        scene {
          group "world" {
            box "base" (size=[2, 1, 2])
            box "hat"  (size=[1, 1, 1], above="base", gap=0.25)
          }
        }
        "#,
    );
    let hat_y = find_mesh_node(&g, "hat").transform.translation.y;
    assert!((hat_y - 1.25).abs() < 1e-4, "hat should be at y=1.25, got {hat_y}");
}

#[test]
fn explicit_pos_axis_wins_over_relative_placement() {
    // Explicit `pos` along the placement axis must survive — without this,
    // `behind` silently overwrites `pos.z`, which gave the imported
    // bed-with-headboard a different headboard position than the
    // standalone bed (top-level siblings skipped relative placement
    // entirely, so `pos` happened to win there).
    let g = lower_src(
        r#"
        scene {
          group "world" {
            box "base" (size=[2, 1, 2])
            box "back" (size=[2, 1, 0.1], behind="base", pos=[0, 0, 0.75])
          }
        }
        "#,
    );
    let z = find_mesh_node(&g, "back").transform.translation.z;
    assert!((z - 0.75).abs() < 1e-4, "explicit pos.z should win, got {z}");
}

#[test]
fn relative_placement_still_fires_when_pos_axis_is_zero() {
    // Pos on a perpendicular axis must not block the snap.
    let g = lower_src(
        r#"
        scene {
          group "world" {
            box "base" (size=[2, 1, 2])
            box "hat"  (size=[1, 1, 1], above="base", pos=[0.25, 0, 0])
          }
        }
        "#,
    );
    let t = find_mesh_node(&g, "hat").transform.translation;
    // Snap on Y still fires: hat at y=1.0 (base top + hat half-height).
    assert!((t.y - 1.0).abs() < 1e-4, "snap on Y should still fire, got {t:?}");
    // Pos.x preserved.
    assert!((t.x - 0.25).abs() < 1e-4, "pos.x should be preserved, got {t:?}");
}
